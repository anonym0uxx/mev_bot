//! Durable HELD-STATE ledger for the model lane: what a restart needs to rebuild held positions,
//! pending orders and the wallet identity together.
//!
//! * Separate file from the SAFETY_OFF latch (schema 1 there is unchanged).
//! * Atomic + fsync writes (temp, fsync, rename, fsync dir). A failed write is counted by the caller and
//!   fails CLOSED (the engine trips SAFETY_OFF while exposure exists that it cannot persist).
//! * Unknown is not zero: `inventory_tokens` is `None` when no fill ever established it.
//! * Restore is all-or-nothing and REFUSES by name; it never guesses or repairs.
//! * Times are absolute wire-clock unix ms, so a restored position keeps its TRUE age (downtime included).

use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

/// Schema of this file.
pub const HELD_SCHEMA: u64 = 3;

/// One held position, everything needed to rebuild the store entry, its attribution and its management
/// state. Fixed-point / integer, no floats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldEntry {
    /// Market.
    pub mint: [u8; 32],
    /// Entry price fixed point (basis of `mult_bps`).
    pub entry_price_fp: u64,
    /// Total deployed notional at entry (lamports).
    pub size_lamports: u64,
    /// Pro-rata entry cost (lamports) still attributed to the remaining position.
    pub cost_lamports: u64,
    /// Fraction of the original position still held, bps.
    pub remaining_bps: u32,
    /// Raw tokens held; `None` when never established by a fill.
    pub inventory_tokens: Option<u64>,
    /// Whether the position is model-managed.
    pub model_managed: bool,
    /// Committed entry spend in the open-lane attribution (lamports).
    pub entry_spend: u64,
    /// Realized lamports already booked against this position (partial sells).
    pub realized_acc: i128,
    /// `WlLane::index()` of the opening lane.
    pub lane_index: u8,
    /// `DiscoveryLane::index()` of the opening lane.
    pub discovery_lane_index: u8,
    /// Whether the position trades on the AMM.
    pub amm: bool,
    /// Wire-clock unix ms of the opening fill.
    pub fill_ms: i64,
    /// Highest / lowest observed price since the fill (fixed point).
    pub peak_fp: u64,
    /// Lowest price since the fill.
    pub trough_fp: u64,
    /// Management step already taken (true history, not reset).
    pub step: i64,
    /// Management version (bumped by fills).
    pub version: u64,
    /// The order that opened the position.
    pub position_order: u64,
}

/// One pending order. After a restart its acknowledgement is UNKNOWN, so it restores as uncertain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldPending {
    /// `entry`, `reduce`, `exit`, `add`.
    pub kind: String,
    /// Order id in its own namespace.
    pub id: u64,
    /// Market.
    pub mint: [u8; 32],
    /// Entry: clip lamports. Sells/ADD: intended tokens.
    pub intended: u64,
    /// Already filled (tokens for sells/ADD).
    pub filled: u64,
    /// ADD: max spend; 0 otherwise.
    pub max_spend: u64,
    /// ADD: spent so far; 0 otherwise.
    pub spent: u64,
    /// ADD: fee bp the reservation used.
    pub fee_bps: u32,
    /// Whether the venue was the AMM.
    pub amm: bool,
    /// Wire-clock ms the order was created.
    pub created_ms: i64,
    /// On-chain slot the feed had shown at creation.
    pub created_slot: u64,
    /// Management version the order was bound to (0 for entries).
    pub version: u64,
    /// Entry only: execution attempt.
    pub attempt: u32,
    /// Entry only: the prompt decision clock.
    pub snap_t_dec_ms: i64,
    /// Entry only: price limit as f64 bits (exact round trip), if any.
    pub price_limit_bits: Option<u64>,
    /// Entry only: snapshot price as f64 bits.
    pub snap_price_bits: u64,
    /// Entry only: `WlLane::index()`.
    pub lane_index: u8,
    /// Entry only: `DiscoveryLane::index()`.
    pub discovery_lane_index: u8,
}

/// Terminal evidence about one entry order (the durable form of the engine's `ReconcileOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldOutcome {
    NotFilled,
    Filled {
        entry_price_fp: u64,
        reserve_sol_lamports: u64,
    },
}

/// Durable identity of one SETTLED entry order (filled, closed, or cleared as not filled): the minimum a restarted
/// process needs to recognise a duplicate report, preserve a conflicting one, and never double-apply a fill. It
/// carries NO financial effect: balances and positions are restored from `realized_lamports`/`held`, never by
/// replaying these records. `state`: 2 = Filled, 3 = NotFilled, 4 = Closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldOrder {
    pub id: u64,
    pub mint: [u8; 32],
    pub attempt: u32,
    pub clip_lamports: u64,
    pub filled_clip_lamports: u64,
    pub state: u8,
    pub terminal: Option<HeldOutcome>,
}

/// One management (REDUCE / EXIT / ADD) order's durable identity and terminal evidence. Its own id namespace (the
/// management sequence), separate from entry orders. `state`: 1 completed, 2 ended partially filled, 3 ended
/// unfilled, 4 preempted (the position was closed by a protective path while the order was unfinished).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSell {
    pub id: u64,
    pub mint: [u8; 32],
    /// 0 reduce, 1 exit, 2 add.
    pub kind: u8,
    pub intended: u64,
    /// Cumulative tokens filled (the idempotent report key).
    pub filled: u64,
    /// ADD only: cumulative notional spent (the idempotent report key for buys).
    pub spent: u64,
    pub state: u8,
    pub last_price_fp: u64,
}

/// An unresolved management-order conflict, preserved verbatim; blocks new exposure on its mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSellFault {
    pub order_id: u64,
    pub mint: [u8; 32],
    /// One of [`SELL_FAULT_SOURCES`].
    pub source: String,
    /// Cumulative filled the books held when the conflict arose.
    pub books_filled: u64,
    /// Contradicting cumulative reports, verbatim.
    pub reported: Vec<u64>,
}

/// The management-conflict sources the engine can write; anything else makes the file untrusted.
pub const SELL_FAULT_SOURCES: [&str; 3] = [
    "report_exceeds_order",
    "report_contradicts_settled",
    "uncertain_sell_preempted",
];

/// An unresolved reconciliation fault, preserved verbatim. It blocks new exposure on its mint until resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldFault {
    pub order_id: u64,
    pub mint: [u8; 32],
    pub first: Option<HeldOutcome>,
    /// One of the engine's fixed fault sources (an unknown name makes the file untrusted).
    pub first_source: String,
    pub contradicting: Vec<HeldOutcome>,
}

/// Decision-path scheduling state the model lane reads (re-ask windows, dirty-market queue, management cadence).
/// ADVISORY: losing it can only cause an extra inference ask, never an execution effect; it is persisted so a restart
/// does not silently reset it. Excluded from the change digest (written with the ledger, not on its own).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionState {
    /// (mint, last entry ask wire-ms) for answered asks.
    pub last_ask: Vec<([u8; 32], i64)>,
    /// (mint, dirty-since wire-ms): markets with observations not yet asked about.
    pub dirty: Vec<([u8; 32], i64)>,
    /// (mint, last management ask ms, last management try ms).
    pub mgmt: Vec<([u8; 32], Option<i64>, i64)>,
    /// Engine wire clock (ms) at which this state was captured: the recovery point. Observations at or before it
    /// were already reflected in `dirty`/`last_ask`, so an overlap replay of them must not re-mark markets dirty.
    pub through_ms: i64,
}

/// The whole durable record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldLedger {
    /// Seed bankroll the books started from (a changed seed refuses restore).
    pub seed_lamports: u64,
    /// Cumulative realized lamports (`balance = seed + realized`).
    pub realized_lamports: i128,
    /// Entry order sequence counter.
    pub model_order_seq: u64,
    /// Management order sequence counter.
    pub mgmt_seq: u64,
    /// Wall ms the record was written.
    pub written_wall_ms: u64,
    /// Held positions.
    pub held: Vec<HeldEntry>,
    /// Pending orders.
    pub pending: Vec<HeldPending>,
    /// Decision-path scheduling state (advisory; see [`DecisionState`]).
    pub decision: DecisionState,
    /// Settled entry-order identity + terminal evidence (see [`HeldOrder`]).
    pub orders: Vec<HeldOrder>,
    /// Unresolved reconciliation faults.
    pub faults: Vec<HeldFault>,
    /// Settled management orders (identity + cumulative filled), pending ones live in `pending`.
    pub sells: Vec<HeldSell>,
    /// Unresolved management-order conflicts.
    pub sell_faults: Vec<HeldSellFault>,
    /// Management compaction floor (like `order_floor`, own namespace).
    pub sell_floor: u64,
    /// Compaction floor: an order id at or below it that is absent from `orders` was COMPACTED (named, never
    /// "unknown"). Zero when nothing was ever compacted.
    pub order_floor: u64,
}

/// Why a ledger could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerReadError {
    /// No file.
    Absent,
    /// Present but unreadable / not JSON / wrong shape / unknown schema.
    Untrusted(&'static str),
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn outcome_json(o: Option<HeldOutcome>) -> Value {
    match o {
        None => Value::Null,
        Some(HeldOutcome::NotFilled) => json!({"k": "not_filled"}),
        Some(HeldOutcome::Filled {
            entry_price_fp,
            reserve_sol_lamports,
        }) => {
            json!({"k": "filled", "px": entry_price_fp, "res": reserve_sol_lamports})
        }
    }
}

/// `Ok(None)` for JSON null; `Err(())` for anything malformed.
fn outcome_from(v: &Value) -> Result<Option<HeldOutcome>, ()> {
    if v.is_null() {
        return Ok(None);
    }
    match v["k"].as_str() {
        Some("not_filled") => Ok(Some(HeldOutcome::NotFilled)),
        Some("filled") => Ok(Some(HeldOutcome::Filled {
            entry_price_fp: v["px"].as_u64().ok_or(())?,
            reserve_sol_lamports: v["res"].as_u64().ok_or(())?,
        })),
        _ => Err(()),
    }
}

/// The fault sources the engine can write; anything else makes the file untrusted.
pub const FAULT_SOURCES: [&str; 3] = [
    "first_terminal_evidence",
    "paper_fill",
    "expired_or_cleared_without_evidence",
];

impl HeldLedger {
    /// Serialise (stable key order is not required; the digest is computed over the entries instead).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "schema": HELD_SCHEMA,
            "seed_lamports": self.seed_lamports,
            "realized_lamports": self.realized_lamports.to_string(),
            "model_order_seq": self.model_order_seq,
            "mgmt_seq": self.mgmt_seq,
            "written_wall_ms": self.written_wall_ms,
            "held": self.held.iter().map(|h| json!({
                "mint": hex(&h.mint),
                "entry_price_fp": h.entry_price_fp,
                "size_lamports": h.size_lamports,
                "cost_lamports": h.cost_lamports,
                "remaining_bps": h.remaining_bps,
                "inventory_tokens": h.inventory_tokens,
                "model_managed": h.model_managed,
                "entry_spend": h.entry_spend,
                "realized_acc": h.realized_acc.to_string(),
                "lane_index": h.lane_index,
                "discovery_lane_index": h.discovery_lane_index,
                "amm": h.amm,
                "fill_ms": h.fill_ms,
                "peak_fp": h.peak_fp,
                "trough_fp": h.trough_fp,
                "step": h.step,
                "version": h.version,
                "position_order": h.position_order,
            })).collect::<Vec<_>>(),
            "pending": self.pending.iter().map(|p| json!({
                "kind": p.kind,
                "id": p.id,
                "mint": hex(&p.mint),
                "intended": p.intended,
                "filled": p.filled,
                "max_spend": p.max_spend,
                "spent": p.spent,
                "fee_bps": p.fee_bps,
                "amm": p.amm,
                "created_ms": p.created_ms,
                "created_slot": p.created_slot,
                "version": p.version,
                "attempt": p.attempt,
                "snap_t_dec_ms": p.snap_t_dec_ms,
                "price_limit_bits": p.price_limit_bits,
                "snap_price_bits": p.snap_price_bits,
                "lane_index": p.lane_index,
                "discovery_lane_index": p.discovery_lane_index,
            })).collect::<Vec<_>>(),
            "order_floor": self.order_floor,
            "sell_floor": self.sell_floor,
            "sells": self.sells.iter().map(|o| json!({
                "id": o.id, "mint": hex(&o.mint), "kind": o.kind, "intended": o.intended,
                "filled": o.filled, "spent": o.spent, "state": o.state, "px": o.last_price_fp,
            })).collect::<Vec<_>>(),
            "sell_faults": self.sell_faults.iter().map(|f| json!({
                "order_id": f.order_id, "mint": hex(&f.mint), "source": f.source,
                "books_filled": f.books_filled, "reported": f.reported,
            })).collect::<Vec<_>>(),
            "orders": self.orders.iter().map(|o| json!({
                "id": o.id, "mint": hex(&o.mint), "attempt": o.attempt,
                "clip": o.clip_lamports, "filled_clip": o.filled_clip_lamports,
                "state": o.state, "terminal": outcome_json(o.terminal),
            })).collect::<Vec<_>>(),
            "faults": self.faults.iter().map(|f| json!({
                "order_id": f.order_id, "mint": hex(&f.mint), "first": outcome_json(f.first),
                "first_source": f.first_source,
                "contradicting": f.contradicting.iter().map(|c| outcome_json(Some(*c))).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "decision": {
                "last_ask": self.decision.last_ask.iter().map(|(m, t)| json!([hex(m), t])).collect::<Vec<_>>(),
                "dirty": self.decision.dirty.iter().map(|(m, t)| json!([hex(m), t])).collect::<Vec<_>>(),
                "mgmt": self.decision.mgmt.iter().map(|(m, a, t)| json!([hex(m), a, t])).collect::<Vec<_>>(),
                "through_ms": self.decision.through_ms,
            },
        })
    }

    /// Parse a ledger. Any missing or mistyped required field makes the whole file untrusted.
    ///
    /// # Errors
    /// [`LedgerReadError`].
    pub fn from_json(v: &Value) -> Result<Self, LedgerReadError> {
        let bad = |w: &'static str| LedgerReadError::Untrusted(w);
        if v["schema"].as_u64() != Some(HELD_SCHEMA) {
            return Err(bad("schema"));
        }
        let u = |x: &Value, k: &'static str| x[k].as_u64().ok_or(bad(k));
        let i = |x: &Value, k: &'static str| x[k].as_i64().ok_or(bad(k));
        let big = |x: &Value, k: &'static str| {
            x[k].as_str()
                .and_then(|s| s.parse::<i128>().ok())
                .ok_or(bad(k))
        };
        let mut held = Vec::new();
        for h in v["held"].as_array().ok_or(bad("held"))? {
            held.push(HeldEntry {
                mint: h["mint"].as_str().and_then(unhex).ok_or(bad("held.mint"))?,
                entry_price_fp: u(h, "entry_price_fp")?,
                size_lamports: u(h, "size_lamports")?,
                cost_lamports: u(h, "cost_lamports")?,
                remaining_bps: u32::try_from(u(h, "remaining_bps")?)
                    .map_err(|_| bad("remaining_bps"))?,
                inventory_tokens: match &h["inventory_tokens"] {
                    Value::Null => None,
                    x => Some(x.as_u64().ok_or(bad("inventory_tokens"))?),
                },
                model_managed: h["model_managed"].as_bool().ok_or(bad("model_managed"))?,
                entry_spend: u(h, "entry_spend")?,
                realized_acc: big(h, "realized_acc")?,
                lane_index: u8::try_from(u(h, "lane_index")?).map_err(|_| bad("lane_index"))?,
                discovery_lane_index: u8::try_from(u(h, "discovery_lane_index")?)
                    .map_err(|_| bad("discovery_lane_index"))?,
                amm: h["amm"].as_bool().ok_or(bad("amm"))?,
                fill_ms: i(h, "fill_ms")?,
                peak_fp: u(h, "peak_fp")?,
                trough_fp: u(h, "trough_fp")?,
                step: i(h, "step")?,
                version: u(h, "version")?,
                position_order: u(h, "position_order")?,
            });
        }
        let mut pending = Vec::new();
        for p in v["pending"].as_array().ok_or(bad("pending"))? {
            pending.push(HeldPending {
                kind: p["kind"].as_str().ok_or(bad("pending.kind"))?.to_string(),
                id: u(p, "id")?,
                mint: p["mint"]
                    .as_str()
                    .and_then(unhex)
                    .ok_or(bad("pending.mint"))?,
                intended: u(p, "intended")?,
                filled: u(p, "filled")?,
                max_spend: u(p, "max_spend")?,
                spent: u(p, "spent")?,
                fee_bps: u32::try_from(u(p, "fee_bps")?).map_err(|_| bad("fee_bps"))?,
                amm: p["amm"].as_bool().ok_or(bad("pending.amm"))?,
                created_ms: i(p, "created_ms")?,
                created_slot: u(p, "created_slot")?,
                version: u(p, "version")?,
                attempt: u32::try_from(u(p, "attempt")?).map_err(|_| bad("attempt"))?,
                snap_t_dec_ms: i(p, "snap_t_dec_ms")?,
                price_limit_bits: match &p["price_limit_bits"] {
                    Value::Null => None,
                    x => Some(x.as_u64().ok_or(bad("price_limit_bits"))?),
                },
                snap_price_bits: u(p, "snap_price_bits")?,
                lane_index: u8::try_from(u(p, "lane_index")?).map_err(|_| bad("p.lane_index"))?,
                discovery_lane_index: u8::try_from(u(p, "discovery_lane_index")?)
                    .map_err(|_| bad("p.discovery_lane_index"))?,
            });
        }
        let mut decision = DecisionState::default();
        if let Some(d) = v.get("decision").filter(|d| d.is_object()) {
            let pair = |x: &Value| -> Option<([u8; 32], i64)> {
                Some((unhex(x[0].as_str()?)?, x[1].as_i64()?))
            };
            for x in d["last_ask"].as_array().ok_or(bad("decision.last_ask"))? {
                decision
                    .last_ask
                    .push(pair(x).ok_or(bad("decision.last_ask"))?);
            }
            for x in d["dirty"].as_array().ok_or(bad("decision.dirty"))? {
                decision.dirty.push(pair(x).ok_or(bad("decision.dirty"))?);
            }
            for x in d["mgmt"].as_array().ok_or(bad("decision.mgmt"))? {
                let m = x[0].as_str().and_then(unhex).ok_or(bad("decision.mgmt"))?;
                let a = match &x[1] {
                    Value::Null => None,
                    y => Some(y.as_i64().ok_or(bad("decision.mgmt"))?),
                };
                decision
                    .mgmt
                    .push((m, a, x[2].as_i64().ok_or(bad("decision.mgmt"))?));
            }
        }
        if let Some(d) = v.get("decision").filter(|d| d.is_object()) {
            decision.through_ms = d["through_ms"].as_i64().ok_or(bad("decision.through_ms"))?;
        }
        let mut orders = Vec::new();
        let mut seen_ids = std::collections::BTreeSet::new();
        for o in v["orders"].as_array().ok_or(bad("orders"))? {
            let rec = HeldOrder {
                id: u(o, "id")?,
                mint: o["mint"]
                    .as_str()
                    .and_then(unhex)
                    .ok_or(bad("orders.mint"))?,
                attempt: u32::try_from(u(o, "attempt")?).map_err(|_| bad("orders.attempt"))?,
                clip_lamports: u(o, "clip")?,
                filled_clip_lamports: u(o, "filled_clip")?,
                state: u8::try_from(u(o, "state")?).map_err(|_| bad("orders.state"))?,
                terminal: outcome_from(&o["terminal"]).map_err(|()| bad("orders.terminal"))?,
            };
            if !(2..=4).contains(&rec.state) || !seen_ids.insert(rec.id) {
                return Err(bad("orders.state_or_duplicate"));
            }
            orders.push(rec);
        }
        let mut faults = Vec::new();
        for f in v["faults"].as_array().ok_or(bad("faults"))? {
            let src = f["first_source"]
                .as_str()
                .ok_or(bad("faults.first_source"))?;
            if !FAULT_SOURCES.contains(&src) {
                return Err(bad("faults.first_source"));
            }
            let mut contradicting = Vec::new();
            for c in f["contradicting"]
                .as_array()
                .ok_or(bad("faults.contradicting"))?
            {
                contradicting.push(
                    outcome_from(c)
                        .ok()
                        .flatten()
                        .ok_or(bad("faults.contradicting"))?,
                );
            }
            faults.push(HeldFault {
                order_id: u(f, "order_id")?,
                mint: f["mint"]
                    .as_str()
                    .and_then(unhex)
                    .ok_or(bad("faults.mint"))?,
                first: outcome_from(&f["first"]).map_err(|()| bad("faults.first"))?,
                first_source: src.to_string(),
                contradicting,
            });
        }
        let mut sells = Vec::new();
        let mut sell_ids = std::collections::BTreeSet::new();
        for o in v["sells"].as_array().ok_or(bad("sells"))? {
            let rec = HeldSell {
                id: u(o, "id")?,
                mint: o["mint"]
                    .as_str()
                    .and_then(unhex)
                    .ok_or(bad("sells.mint"))?,
                kind: u8::try_from(u(o, "kind")?).map_err(|_| bad("sells.kind"))?,
                intended: u(o, "intended")?,
                filled: u(o, "filled")?,
                spent: u(o, "spent")?,
                state: u8::try_from(u(o, "state")?).map_err(|_| bad("sells.state"))?,
                last_price_fp: u(o, "px")?,
            };
            if rec.kind > 2
                || !(1..=4).contains(&rec.state)
                || rec.filled > rec.intended
                || rec.id == 0
                || !sell_ids.insert(rec.id)
            {
                return Err(bad("sells.shape_or_duplicate"));
            }
            sells.push(rec);
        }
        let mut sell_faults = Vec::new();
        for f in v["sell_faults"].as_array().ok_or(bad("sell_faults"))? {
            let src = f["source"].as_str().ok_or(bad("sell_faults.source"))?;
            if !SELL_FAULT_SOURCES.contains(&src) {
                return Err(bad("sell_faults.source"));
            }
            let reported = f["reported"]
                .as_array()
                .ok_or(bad("sell_faults.reported"))?
                .iter()
                .map(|x| x.as_u64().ok_or(bad("sell_faults.reported")))
                .collect::<Result<Vec<_>, _>>()?;
            sell_faults.push(HeldSellFault {
                order_id: u(f, "order_id")?,
                mint: f["mint"]
                    .as_str()
                    .and_then(unhex)
                    .ok_or(bad("sell_faults.mint"))?,
                source: src.to_string(),
                books_filled: u(f, "books_filled")?,
                reported,
            });
        }
        let mgmt_seq = u(v, "mgmt_seq")?;
        if sells.iter().any(|x| x.id > mgmt_seq) {
            return Err(bad("sells.id_above_mgmt_seq"));
        }
        Ok(Self {
            sells,
            sell_faults,
            sell_floor: u(v, "sell_floor")?,
            orders,
            faults,
            order_floor: u(v, "order_floor")?,
            decision,
            seed_lamports: u(v, "seed_lamports")?,
            realized_lamports: big(v, "realized_lamports")?,
            model_order_seq: u(v, "model_order_seq")?,
            mgmt_seq: u(v, "mgmt_seq")?,
            written_wall_ms: u(v, "written_wall_ms")?,
            held,
            pending,
        })
    }

    /// Read from disk.
    ///
    /// # Errors
    /// [`LedgerReadError::Absent`] when there is no file; `Untrusted` for anything else wrong.
    pub fn read(path: &Path) -> Result<Self, LedgerReadError> {
        let raw = match fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(LedgerReadError::Absent)
            }
            Err(_) => return Err(LedgerReadError::Untrusted("io")),
        };
        let v: Value =
            serde_json::from_str(&raw).map_err(|_| LedgerReadError::Untrusted("json"))?;
        Self::from_json(&v)
    }

    /// Atomic durable write.
    ///
    /// # Errors
    /// The io error (the caller counts it and fails closed).
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let mut me = self.clone();
        me.written_wall_ms = wall_ms();
        let body = serde_json::to_string_pretty(&me.to_json()).unwrap_or_default();
        let tmp = path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent() {
            if let Ok(d) = fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

/// Why a restore was refused (nothing was applied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreRefusal {
    /// The engine already holds positions or orders: restore is only for a fresh engine.
    EngineNotFresh,
    /// The same market appears twice among held positions.
    DuplicateHeld,
    /// The configured seed bankroll differs from the one the books were kept under.
    SeedMismatch {
        /// Seed in the file.
        file: u64,
        /// Seed this engine runs with.
        engine: u64,
    },
    /// Committed entry spend exceeds the balance (the books do not close).
    CommittedExceedsBalance,
    /// A held position has a zero entry price (cannot be marked).
    ZeroEntryPrice,
    /// A held position's lane index is not a known lane (no renumbering, no guessing).
    UnknownLane,
    /// More positions than the store allows.
    OverCapacity,
    /// A pending sell/ADD order names a market with no held position.
    OrphanPendingOrder,
    /// A held position was not model-managed: restoring it would need the legacy ladder, which is not
    /// restored.
    NotModelManaged,
    /// A pending order has an unknown kind.
    UnknownOrderKind,
    /// One management order id is both settled and pending (the file describes two incompatible states).
    SettledAndPendingSameOrder,
    /// A settled order carries an id the order sequence never issued (the files do not describe one history).
    OrderIdBeyondSequence,
    /// A fault names an order that is in neither the settled records nor the pending list.
    FaultWithoutOrder,
}

/// What a successful restore rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoreReport {
    /// Positions rebuilt.
    pub positions: usize,
    /// Pending orders rebuilt (all as UNCERTAIN: acknowledgement unknown after a restart).
    pub pending_uncertain: usize,
    /// Committed capital restored (lamports).
    pub committed_lamports: u64,
    /// Realized lamports restored.
    pub realized_lamports: i128,
    /// Positions whose inventory was never established by a fill (management will refuse them by name).
    pub inventory_unknown: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HeldLedger {
        HeldLedger {
            seed_lamports: 1_000,
            realized_lamports: -7,
            model_order_seq: 4,
            mgmt_seq: 3,
            written_wall_ms: 0,
            decision: DecisionState::default(),
            orders: vec![HeldOrder {
                id: 3,
                mint: [4; 32],
                attempt: 1,
                clip_lamports: 5,
                filled_clip_lamports: 5,
                state: 4,
                terminal: Some(HeldOutcome::Filled {
                    entry_price_fp: 9,
                    reserve_sol_lamports: 8,
                }),
            }],
            faults: vec![HeldFault {
                order_id: 3,
                mint: [4; 32],
                first: Some(HeldOutcome::NotFilled),
                first_source: "paper_fill".into(),
                contradicting: vec![HeldOutcome::NotFilled],
            }],
            order_floor: 2,
            sells: vec![HeldSell {
                id: 3,
                mint: [7; 32],
                kind: 1,
                intended: 500,
                filled: 500,
                spent: 0,
                state: 1,
                last_price_fp: 22_000,
            }],
            sell_faults: vec![HeldSellFault {
                order_id: 3,
                mint: [7; 32],
                source: "report_contradicts_settled".into(),
                books_filled: 500,
                reported: vec![400],
            }],
            sell_floor: 1,
            held: vec![HeldEntry {
                mint: [9; 32],
                entry_price_fp: 45_085,
                size_lamports: 100,
                cost_lamports: 101,
                remaining_bps: 5_000,
                inventory_tokens: None,
                model_managed: true,
                entry_spend: 60,
                realized_acc: -3,
                lane_index: 3,
                discovery_lane_index: 4,
                amm: false,
                fill_ms: 1_700_000_000_000,
                peak_fp: 50_000,
                trough_fp: 40_000,
                step: 5,
                version: 3,
                position_order: 1,
            }],
            pending: vec![HeldPending {
                kind: "reduce".into(),
                id: 2,
                mint: [9; 32],
                intended: 10,
                filled: 4,
                max_spend: 0,
                spent: 0,
                fee_bps: 0,
                amm: false,
                created_ms: 1_700_000_001_000,
                created_slot: 99,
                version: 3,
                attempt: 1,
                snap_t_dec_ms: 0,
                price_limit_bits: Some(1.25f64.to_bits()),
                snap_price_bits: 2.5f64.to_bits(),
                lane_index: 0,
                discovery_lane_index: 0,
            }],
        }
    }

    #[test]
    fn round_trips_exactly_including_unknown_inventory_and_negative_realized() {
        let d = std::env::temp_dir().join(format!("pq_held_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let p = d.join("h.json");
        let l = sample();
        l.write(&p).unwrap();
        let mut back = HeldLedger::read(&p).unwrap();
        assert!(back.written_wall_ms > 0);
        back.written_wall_ms = 0;
        assert_eq!(back, l);
        assert_eq!(
            back.held[0].inventory_tokens, None,
            "unknown stays unknown, never 0"
        );
    }

    #[test]
    fn untrusted_files_are_refused_not_repaired() {
        let d = std::env::temp_dir().join(format!("pq_held_u_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let p = d.join("h.json");
        assert_eq!(HeldLedger::read(&p), Err(LedgerReadError::Absent));
        fs::write(&p, "not json").unwrap();
        assert!(matches!(
            HeldLedger::read(&p),
            Err(LedgerReadError::Untrusted(_))
        ));
        let mut v = sample().to_json();
        // An OLD schema (1: no settled-order identity) is refused by name, never upgraded by guessing.
        v["schema"] = json!(1);
        fs::write(&p, v.to_string()).unwrap();
        assert!(matches!(
            HeldLedger::read(&p),
            Err(LedgerReadError::Untrusted("schema"))
        ));
        for (field, key) in [
            ("orders", "orders"),
            ("faults", "faults"),
            ("order_floor", "order_floor"),
            ("sells", "sells"),
            ("sell_faults", "sell_faults"),
            ("sell_floor", "sell_floor"),
        ] {
            let mut v = sample().to_json();
            v.as_object_mut().unwrap().remove(field);
            fs::write(&p, v.to_string()).unwrap();
            assert!(HeldLedger::read(&p).is_err(), "missing {key} must refuse");
        }
        let mut v = sample().to_json();
        v["faults"][0]["first_source"] = json!("made_up_source");
        fs::write(&p, v.to_string()).unwrap();
        assert!(
            HeldLedger::read(&p).is_err(),
            "an unknown fault source refuses"
        );
        let mut v = sample().to_json();
        v["orders"][0]["state"] = json!(1);
        fs::write(&p, v.to_string()).unwrap();
        assert!(HeldLedger::read(&p).is_err(), "a non-settled state refuses");
        let mut v = sample().to_json();
        let dup = v["orders"][0].clone();
        v["orders"].as_array_mut().unwrap().push(dup);
        fs::write(&p, v.to_string()).unwrap();
        assert!(
            HeldLedger::read(&p).is_err(),
            "a duplicated order id refuses"
        );
        let mut v = sample().to_json();
        v["held"][0]["peak_fp"] = json!("x");
        fs::write(&p, v.to_string()).unwrap();
        assert!(matches!(
            HeldLedger::read(&p),
            Err(LedgerReadError::Untrusted("peak_fp"))
        ));
    }
}
