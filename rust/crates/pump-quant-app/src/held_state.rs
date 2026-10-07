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
pub const HELD_SCHEMA: u64 = 1;

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
        Ok(Self {
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
            mgmt_seq: 2,
            written_wall_ms: 0,
            decision: DecisionState::default(),
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
        v["schema"] = json!(2);
        fs::write(&p, v.to_string()).unwrap();
        assert!(matches!(
            HeldLedger::read(&p),
            Err(LedgerReadError::Untrusted("schema"))
        ));
        let mut v = sample().to_json();
        v["held"][0]["peak_fp"] = json!("x");
        fs::write(&p, v.to_string()).unwrap();
        assert!(matches!(
            HeldLedger::read(&p),
            Err(LedgerReadError::Untrusted("peak_fp"))
        ));
    }
}
