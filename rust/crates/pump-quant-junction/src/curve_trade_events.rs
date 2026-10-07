//! Transaction-event owned pump.fun CURVE trade history.
//!
//! # Why this exists
//! The curve account stream delivers ONE snapshot per (curve, slot) in the captured sessions
//! (see `docs/missing_history_causal/FINDINGS.md`), while a slot can hold several trades. A
//! snapshot delta is therefore a NET change: it cannot recover per-trade counts, participants,
//! fees/CU or timing, and an unflagged net delta is not proof that the history is complete.
//! The per-trade truth is the program's own `TradeEvent` self-CPI inside a SUCCESSFUL
//! transaction. This module decodes those events and nothing else feeds curve trade history.
//!
//! # Contract
//! * Only a transaction VERIFIED successful (`tx_ok == Some(true)`) yields events. Unknown
//!   status is refused, never assumed.
//! * One event per `TradeEvent` instruction. Identity is `(signature, instruction ordinal)`, so
//!   several trades in one transaction are kept exactly once each and a replayed delivery is
//!   dropped by [`EventDedup`]. A signature alone is NOT an identity.
//! * A successful transaction that carries a pump buy/sell instruction but no decodable
//!   `TradeEvent`, or a truncated event, is a [`TxDecode::Incomplete`]: the caller records a
//!   missing observation. There is NO silent fallback to snapshot deltas.
//! * `quote_lamports` is the event's own `sol_amount`. It is NOT the corpus tape's
//!   `sol_lamports` (a trader balance delta from pre/post balances, which this wire line does
//!   not carry); the divergence is measured, not assumed away.
//! * `price_fp` is the DESCRIPTIVE print price from the event's post-trade virtual reserves. It
//!   is not an executable quote.

use std::collections::{HashSet, VecDeque};

use pump_quant_app::event::FeatureBasis;
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_domain::ids::Mint;

use crate::laserstream::{wallet_entity_id, LaserStreamTx, PUMP_FUN_PROGRAM};
use crate::{ProvenanceSource, ProvenancedEvent};

/// Anchor self-CPI tag (`sha256("anchor:event")[..8]`) + `TradeEvent` discriminator.
pub const TRADE_EVENT_PREFIX: [u8; 16] = [
    0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d, 0xbd, 0xdb, 0x7f, 0xd3, 0x4e, 0xe6, 0x61, 0xee,
];
/// Required-prefix length: tag 8 + disc 8 + mint 32 + sol 8 + tokens 8 + is_buy 1 + user 32
/// + timestamp 8 + four reserves 32.
pub const TRADE_EVENT_MIN_LEN: usize = 137;

/// Instruction discriminators of pump.fun buy/sell-type instructions (the corpus table).
const BUY_SELL_DISCS: [[u8; 8]; 5] = [
    [102, 6, 61, 18, 1, 218, 235, 234],
    [51, 230, 133, 164, 1, 127, 131, 173],
    [184, 23, 238, 97, 103, 197, 211, 61],
    [93, 246, 130, 60, 231, 233, 64, 178],
    [56, 252, 116, 8, 158, 223, 205, 95],
];

/// Quote asset of a curve trade, as the event states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuoteIdentity {
    /// `quote_mint` is the native-SOL sentinel (all-zero key) or WSOL: SOL-quoted, supported.
    Sol,
    /// Verified other quote mint (e.g. USDC): `UNSUPPORTED_QUOTE_ASSET`, counted outside SOL readiness.
    Other([u8; 32]),
    /// The event ends before `quote_mint` (an older layout) or the tail does not parse: identity NOT established.
    Unknown,
}

/// WSOL mint (So111...112), the other spelling of a SOL quote.
const WSOL_MINT: [u8; 32] = [
    6, 155, 136, 87, 254, 171, 129, 132, 251, 104, 127, 99, 70, 24, 192, 53, 218, 196, 57, 220, 26,
    235, 59, 85, 152, 160, 240, 0, 0, 0, 0, 1,
];

/// Walk the IDL tail of a TradeEvent from the fixed `ix_name` position (offset after `last_update_timestamp`):
/// `ix_name:string, mayhem_mode:bool, cashback_fee_basis_points:u64, cashback:u64, buyback_fee_basis_points:u64,
/// buyback_fee:u64, shareholders:vec<(pubkey,u16)>, quote_mint:pubkey`. Offsets before it are fixed (see IDL).
fn decode_quote(data: &[u8]) -> QuoteIdentity {
    // mint32 sol8 tok8 buy1 user32 ts8 vs8 vt8 rs8 rt8 fee_recipient32 fee_bps8 fee8 creator32 cfee_bps8 cfee8
    // track1 unclaimed8 claimed8 cur_vol8 last_ts8  => ix_name starts at byte 266 (verified on 72,068 captured events)
    let mut o =
        16 + 32 + 8 + 8 + 1 + 32 + 8 + 8 + 8 + 8 + 8 + 32 + 8 + 8 + 32 + 8 + 8 + 1 + 8 + 8 + 8 + 8;
    let step = (|| -> Option<QuoteIdentity> {
        let n = u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?) as usize;
        o = o.checked_add(4)?.checked_add(n)?;
        o = o.checked_add(1 + 8 + 8 + 8 + 8)?; // mayhem_mode + cashback bps/amt + buyback bps/amt
        let sh = u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?) as usize;
        o = o.checked_add(4)?.checked_add(sh.checked_mul(34)?)?;
        let q: [u8; 32] = data.get(o..o + 32)?.try_into().ok()?;
        Some(if q == [0u8; 32] || q == WSOL_MINT {
            QuoteIdentity::Sol
        } else {
            QuoteIdentity::Other(q)
        })
    })();
    step.unwrap_or(QuoteIdentity::Unknown)
}

/// One decoded curve trade. All amounts raw on-chain units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveTradeEvent {
    pub mint: [u8; 32],
    pub user: [u8; 32],
    pub is_buy: bool,
    pub sol_amount: u64,
    pub token_amount: u64,
    pub virtual_sol: u64,
    pub virtual_token: u64,
    pub real_sol: u64,
    pub real_token: u64,
    /// Quote asset, from the event's OWN `quote_mint` field (pump.fun IDL `TradeEvent`, after the variable-length
    /// `ix_name` string and `shareholders` vec). Never inferred from other tokens moving in the transaction and
    /// never from zero virtual SOL.
    pub quote: QuoteIdentity,
    /// Ordinal of the event instruction in the wire line's flattened instruction list. With the
    /// signature this is the deterministic event identity.
    pub ix_ordinal: u32,
}

/// What a transaction decoded to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TxDecode {
    /// Not a pump.fun transaction (or no pump.fun instruction at all).
    NotPump,
    /// Status not verified successful: no events are produced and none are assumed.
    NotVerifiedSuccess,
    /// Verified-successful, with ZERO or more trades (zero only when no buy/sell instruction).
    Events(Vec<CurveTradeEvent>),
    /// Verified-successful but the trade evidence is missing or malformed. The reason is named.
    Incomplete(&'static str),
}

fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// Decode one `TradeEvent` instruction's data. `None` = not a TradeEvent. `Err` = malformed.
fn decode_trade_event(data: &[u8], ordinal: u32) -> Option<Result<CurveTradeEvent, &'static str>> {
    if data.get(..16)? != TRADE_EVENT_PREFIX {
        return None;
    }
    if data.len() < TRADE_EVENT_MIN_LEN {
        return Some(Err("trade_event_truncated"));
    }
    let arr = |lo: usize| -> Option<[u8; 32]> { data.get(lo..lo + 32)?.try_into().ok() };
    let ev = (|| {
        Some(CurveTradeEvent {
            mint: arr(16)?,
            sol_amount: le_u64(data, 48)?,
            token_amount: le_u64(data, 56)?,
            is_buy: *data.get(64)? != 0,
            user: arr(65)?,
            virtual_sol: le_u64(data, 105)?,
            virtual_token: le_u64(data, 113)?,
            real_sol: le_u64(data, 121)?,
            real_token: le_u64(data, 129)?,
            quote: decode_quote(data),
            ix_ordinal: ordinal,
        })
    })();
    Some(ev.ok_or("trade_event_unreadable"))
}

/// Decode a LaserStream transaction's curve trades. Pure; never panics.
#[must_use]
pub fn decode_curve_trade_events(tx: &LaserStreamTx) -> TxDecode {
    let mut saw_pump = false;
    let mut saw_buy_sell = false;
    let mut events: Vec<CurveTradeEvent> = Vec::new();
    let mut malformed: Option<&'static str> = None;
    for (i, ix) in tx.instructions.iter().enumerate() {
        if ix.program_id != PUMP_FUN_PROGRAM {
            continue;
        }
        saw_pump = true;
        if ix.data.len() >= 8 && BUY_SELL_DISCS.iter().any(|d| ix.data[..8] == d[..]) {
            saw_buy_sell = true;
        }
        let ordinal = u32::try_from(i).unwrap_or(u32::MAX);
        match decode_trade_event(&ix.data, ordinal) {
            Some(Ok(e)) => events.push(e),
            Some(Err(r)) => malformed = Some(r),
            None => {}
        }
    }
    if !saw_pump {
        return TxDecode::NotPump;
    }
    match tx.tx_ok {
        Some(true) => {}
        // A FAILED transaction moved no state: not a trade, and not a gap.
        Some(false) => return TxDecode::NotVerifiedSuccess,
        // Unknown status on a line that carries pump buy/sell evidence is NOT "nothing": an
        // older sidecar that omits `meta.tx_ok` would otherwise erase every trade silently.
        None if saw_buy_sell || !events.is_empty() => {
            return TxDecode::Incomplete("tx_status_unknown")
        }
        None => return TxDecode::NotVerifiedSuccess,
    }
    if let Some(r) = malformed {
        return TxDecode::Incomplete(r);
    }
    if saw_buy_sell && events.is_empty() {
        return TxDecode::Incomplete("buy_sell_without_trade_event");
    }
    TxDecode::Events(events)
}

/// Bounded deduplication of delivered events by `(signature, instruction ordinal)`.
///
/// Reconnect/replay delivery re-sends whole transactions; the same signature then yields the
/// same ordinals and every event is dropped exactly once. Two DIFFERENT trades in one
/// transaction have different ordinals and both survive. Capacity-bounded (oldest evicted):
/// a duplicate older than the window would be accepted, which is why the daemon also keeps the
/// engine's own duplicate check behind this one.
pub struct EventDedup {
    seen: HashSet<([u8; 64], u32)>,
    order: VecDeque<([u8; 64], u32)>,
    seen_rows: HashSet<([u8; 64], u32)>,
    order_rows: VecDeque<([u8; 64], u32)>,
    cap: usize,
    pub duplicates: u64,
    /// Verified non-SOL quote (e.g. USDC) events: UNSUPPORTED_QUOTE_ASSET, outside every SOL denominator. Not a gap.
    pub unsupported_quote: u64,
    /// SOL events admitted to the engine but NOT to the trained windows, by reason (population != frozen corpus).
    pub outside_corpus: std::collections::BTreeMap<&'static str, u64>,
    /// SOL events whose corpus-definition basis was resolved (they feed the trained windows).
    pub corpus_basis_resolved: u64,
}

impl EventDedup {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            seen_rows: HashSet::new(),
            order_rows: VecDeque::new(),
            cap: cap.max(1),
            duplicates: 0,
            unsupported_quote: 0,
            outside_corpus: std::collections::BTreeMap::new(),
            corpus_basis_resolved: 0,
        }
    }
    /// As [`Self::first_time`] for corpus ROW identities (a separate domain: a row and an event of the same signature
    /// and index are different things).
    pub fn first_time_row(&mut self, sig: &[u8; 64], ix: u32) -> bool {
        let k = (*sig, ix);
        if self.seen_rows.contains(&k) {
            self.duplicates = self.duplicates.saturating_add(1);
            return false;
        }
        if self.order_rows.len() >= self.cap {
            if let Some(old) = self.order_rows.pop_front() {
                self.seen_rows.remove(&old);
            }
        }
        self.seen_rows.insert(k);
        self.order_rows.push_back(k);
        true
    }
    /// `true` the first time an identity is seen, `false` for a repeat.
    pub fn first_time(&mut self, sig: &[u8; 64], ordinal: u32) -> bool {
        let k = (*sig, ordinal);
        if self.seen.contains(&k) {
            self.duplicates = self.duplicates.saturating_add(1);
            return false;
        }
        if self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        self.seen.insert(k);
        self.order.push_back(k);
        true
    }
}

/// USDC mint (EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v), raw bytes. Used only to LABEL the population of a reject.
const USDC_MINT: [u8; 32] = [
    198, 250, 122, 243, 190, 219, 173, 58, 61, 101, 243, 106, 171, 201, 116, 49, 177, 187, 228,
    194, 210, 246, 224, 228, 124, 166, 2, 3, 69, 47, 93, 97,
];

const PRICE_SCALE: i128 = 1_000_000_000;

/// Stable 128-bit identity of one on-chain trade event: the first 16 bytes of
/// `sha256("pq-curve-trade-event-v1" || signature(64) || ix_ordinal_le(4))`. The SAME function keys the
/// producer-side [`EventDedup`] (as `(signature, ordinal)`) and the engine-side dedup (as this digest), so
/// both layers agree on what "the same event" means. One instruction emits at most one TradeEvent
/// (measured), but the ordinal is the position in the flattened outer-then-inner list, so two events can
/// never share an id even if that ever changes.
#[must_use]
pub fn trade_event_id(sig: &[u8; 64], ix_ordinal: u32) -> u128 {
    let mut h = pump_quant_protocol::sha256::Sha256::new();
    h.update(b"pq-curve-trade-event-v1");
    h.update(sig);
    h.update(&ix_ordinal.to_le_bytes());
    let d = h.finalize();
    u128::from_be_bytes(d[..16].try_into().unwrap_or([0; 16]))
}

/// The corpus-definition basis (trader native+WSOL delta, trader token delta, resolved trader) of the instruction
/// that EMITTED this event. Association is by INVOCATION: the event self-CPI's parent instruction (`invocation_parent`,
/// from the wire's outer index and CPI stack height) is the instruction the corpus row was built from. The parent must
/// be a corpus-known pump.fun buy/sell whose resolved row agrees with the event's mint and side.
///
/// Only when the producer supplied NO invocation positions is the weaker rule available: if exactly ONE corpus row in
/// the transaction has the event's mint and side the association is unambiguous; with two or more it is refused
/// (`attribution_ambiguous`), never resolved by order. `Err(reason)` = outside the frozen corpus population or the
/// basis cannot be established; the trade is then admitted for discovery/state but never to the trained windows.
/// Public so the population census example calls the SAME attribution the daemon path uses.
pub fn corpus_basis_for(
    tx: &LaserStreamTx,
    ev_idx: usize,
    t: &CurveTradeEvent,
    not_launch: &HashSet<[u8; 32]>,
) -> Result<FeatureBasis, &'static str> {
    let bal = tx.balances.as_ref().ok_or("no_balances_on_wire")?;
    let resolve = |i: usize| -> Result<Option<crate::corpus_rows::CorpusRow>, &'static str> {
        let ix = &tx.instructions[i];
        if ix.program_id != PUMP_FUN_PROGRAM {
            return Err("parent_not_pump_fun");
        }
        let Some(is_buy) = crate::corpus_rows::corpus_side(&ix.data) else {
            return Err("instruction_not_in_corpus_table");
        };
        Ok(crate::corpus_rows::resolve_row(
            Some(crate::corpus_rows::PUMP_FUN_TRADER_IX),
            is_buy,
            &ix.accounts,
            &tx.account_keys,
            &tx.invalid_key_idx,
            bal,
            not_launch,
        ))
    };
    let to_basis = |row: crate::corpus_rows::CorpusRow| -> Result<FeatureBasis, &'static str> {
        if row.mint != t.mint || row.is_buy != t.is_buy {
            return Err("corpus_row_disagrees_with_event");
        }
        let tokens_raw = i64::try_from(row.tokens_raw).map_err(|_| "tokens_unrepresentable")?;
        Ok(FeatureBasis {
            sol_lamports: row.sol_lamports,
            tokens_raw,
            trader: row.trader,
        })
    };
    match crate::laserstream::invocation_parent(tx, ev_idx) {
        Ok(p) => match resolve(p)? {
            Some(row) => to_basis(row),
            None => Err("corpus_resolver_rejects"),
        },
        Err("no_invocation_position") => {
            // Producer without invocation positions: unique-candidate rule only.
            let mut cands: Vec<crate::corpus_rows::CorpusRow> = Vec::new();
            let mut saw_corpus_ix = false;
            for (i, ix) in tx.instructions.iter().enumerate() {
                if ix.program_id != PUMP_FUN_PROGRAM
                    || crate::corpus_rows::corpus_side(&ix.data).is_none()
                {
                    continue;
                }
                saw_corpus_ix = true;
                if let Ok(Some(row)) = resolve(i) {
                    if row.mint == t.mint && row.is_buy == t.is_buy {
                        cands.push(row);
                    }
                }
            }
            match cands.len() {
                1 => to_basis(cands[0]),
                0 => Err(if saw_corpus_ix {
                    "corpus_resolver_rejects"
                } else {
                    "instruction_not_in_corpus_table"
                }),
                _ => Err("attribution_ambiguous"),
            }
        }
        // Positions supplied but the structure does not yield a parent: refuse by name.
        Err(r) => Err(r),
    }
}

/// Build the engine event for one decoded trade. `None` when a field cannot be represented
/// (zero token reserve, amount above `i64`): refused, never clamped.
#[must_use]
pub fn curve_trade_to_event(
    t: &CurveTradeEvent,
    tx: &LaserStreamTx,
    is_live: bool,
    feature: Option<FeatureBasis>,
) -> Option<ProvenancedEvent> {
    // Zero reserve fields (measured: whole mints whose every TradeEvent carries vsol=rsol=0) cannot be
    // priced and would be dropped by the join as `NoPrice` with only a counter. Refuse here so the
    // caller records a NAMED missing observation instead (unknown cause, not assumed benign).
    if t.virtual_sol == 0 || t.virtual_token == 0 || t.token_amount == 0 {
        return None;
    }
    let tok = i64::try_from(t.token_amount).ok()?;
    let signed_base = if t.is_buy { tok } else { tok.checked_neg()? };
    #[allow(clippy::arithmetic_side_effects)]
    // LINT-ALLOW(hot_arith): u64*1e9 < i128::MAX, divisor>0 guarded
    let price_fp = i128::from(t.virtual_sol) * PRICE_SCALE / i128::from(t.virtual_token);
    Some(ProvenancedEvent {
        event: AppEvent::MarketTrade {
            mint: Mint(t.mint),
            price_fp,
            quote_lamports: t.sol_amount,
            liquidity_lamports: t.virtual_sol,
            signed_base,
            buyer_entity: wallet_entity_id(&t.user),
            age_slots: 0,
            trader_pubkey: Some(t.user),
            recv_unix_ms: tx.recv_unix_ms,
            slot: Some(tx.slot),
            // Transaction-level quantities repeated on every trade row of the signature: that is
            // how the corpus counts them, so a multi-event transaction carries them per event.
            fee_lamports: tx.fee_lamports,
            cu_consumed: tx.cu_consumed,
            venue: Some(TradeVenue::PumpFun),
            event_id: Some(trade_event_id(&tx.signature, t.ix_ordinal)),
            feature,
        },
        source: ProvenanceSource::LaserStreamTradeEvent,
        slot: tx.slot,
        is_live,
    })
}

/// Outcome of ingesting one transaction through the event path.
#[derive(Debug, PartialEq, Eq)]
pub enum EventIngest {
    /// Nothing curve-related (non-pump, or failed/unverified, or a pump tx with no trades).
    Nothing,
    /// Events produced (post-dedup) and how many repeats were dropped.
    Produced { events: usize, duplicates: usize },
    /// Evidence missing/malformed: the caller MUST record a missing observation for the
    /// affected mints (or all, when none can be named). No snapshot fallback exists.
    Incomplete(&'static str),
}

/// Production step: decode a transaction, dedupe, and return the events to queue.
pub fn ingest_curve_tx(
    tx: &LaserStreamTx,
    dedup: &mut EventDedup,
    out: &mut Vec<ProvenancedEvent>,
) -> EventIngest {
    match decode_curve_trade_events(tx) {
        TxDecode::NotPump | TxDecode::NotVerifiedSuccess => EventIngest::Nothing,
        TxDecode::Incomplete(r) => EventIngest::Incomplete(r),
        TxDecode::Events(evs) => {
            let (mut n, mut d) = (0usize, 0usize);
            let not_launch = crate::corpus_rows::not_a_launch_set();
            for e in &evs {
                // Quote identity comes from the event's own `quote_mint`. A verified non-SOL quote is counted and
                // skipped (it is NOT a gap on a SOL market); an unestablished quote is a named refusal.
                match e.quote {
                    QuoteIdentity::Other(_) => {
                        if dedup.first_time(&tx.signature, e.ix_ordinal) {
                            dedup.unsupported_quote += 1;
                        } else {
                            d += 1;
                        }
                        continue;
                    }
                    QuoteIdentity::Unknown => {
                        return EventIngest::Incomplete("quote_identity_unknown")
                    }
                    QuoteIdentity::Sol => {}
                }
                if !dedup.first_time(&tx.signature, e.ix_ordinal) {
                    d += 1;
                    continue;
                }
                let feature = match corpus_basis_for(tx, e.ix_ordinal as usize, e, &not_launch) {
                    Ok(f) => {
                        dedup.corpus_basis_resolved += 1;
                        Some(f)
                    }
                    Err(why) => {
                        *dedup.outside_corpus.entry(why).or_insert(0) += 1;
                        None
                    }
                };
                match curve_trade_to_event(e, tx, tx.is_live, feature) {
                    Some(pe) => {
                        out.push(pe);
                        n += 1;
                    }
                    None => return EventIngest::Incomplete("trade_event_unrepresentable"),
                }
            }
            if n == 0 && d == 0 {
                EventIngest::Nothing
            } else {
                EventIngest::Produced {
                    events: n,
                    duplicates: d,
                }
            }
        }
    }
}

/// Stable identity of a corpus row: `sha256("pq-corpus-row-v1" || signature || instruction index)`. The instruction
/// index is the row's own position in the flattened list (a row is per INSTRUCTION, as in the frozen builder).
#[must_use]
pub fn corpus_row_id(sig: &[u8; 64], ix_idx: u32) -> u128 {
    let mut h = pump_quant_protocol::sha256::Sha256::new();
    h.update(b"pq-corpus-row-v1");
    h.update(sig);
    h.update(&ix_idx.to_le_bytes());
    let d = h.finalize();
    u128::from_be_bytes(d[..16].try_into().unwrap_or([0; 16]))
}

/// Counters for the PumpSwap corpus-row producer (every outcome named).
#[derive(Clone, Debug, Default)]
pub struct AmmRowStats {
    /// Corpus rows emitted.
    pub emitted: u64,
    /// Corpus-known PumpSwap swap instructions the resolver rejected (the frozen builder drops these too).
    pub resolver_rejects: u64,
    /// Redeliveries of a row already seen (adds nothing).
    pub duplicates: u64,
    /// Resolver rejects by named frozen-builder rule.
    pub reject_reasons: std::collections::BTreeMap<&'static str, u64>,
    /// Rejected instructions whose transaction touches a CANONICAL migration pool or whose accounts include a
    /// pump.fun-created pool, vs not (population attribution filled by the caller's classifier, see `population`).
    pub reject_population: std::collections::BTreeMap<String, u64>,
    /// Transactions with a corpus-known PumpSwap swap instruction but NO balances on the wire (older sidecar).
    pub no_balances_txs: u64,
    /// Transactions whose status was not verified successful (nothing produced; not rows).
    pub not_verified_success_txs: u64,
}

/// Production step for PumpSwap: one corpus-definition feature row per corpus-known PumpSwap buy/sell instruction of
/// a VERIFIED-successful transaction, exactly as `renormalize_raw.py` builds it (trader = instruction-account search
/// without a preferred index, else whole-transaction net-position fallback; native+WSOL delta; token delta).
///
/// This is FEATURE HISTORY, independent of execution scope: a row is produced for every corpus-known swap whatever
/// the pool (canonical or not, any quote), because the frozen builder consumed the whole tape. The trade-ability of
/// the market is decided elsewhere (`decode_amm_swaps` -> `AmmSwap`). The row has no reserve price or quote.
/// Failed or status-unknown transactions produce nothing here; the caller treats unknown status as a named gap.
pub fn ingest_amm_rows(
    tx: &LaserStreamTx,
    dedup: &mut EventDedup,
    stats: &mut AmmRowStats,
    out: &mut Vec<ProvenancedEvent>,
) {
    let has_swap = tx.instructions.iter().any(|ix| {
        ix.program_id == crate::laserstream::PUMP_SWAP_PROGRAM
            && crate::corpus_rows::corpus_swap_side(&ix.data).is_some()
    });
    if tx.tx_ok != Some(true) {
        if has_swap {
            stats.not_verified_success_txs += 1;
        }
        return;
    }
    let Some(bal) = tx.balances.as_ref() else {
        if has_swap {
            stats.no_balances_txs += 1;
        }
        return;
    };
    let not_launch = crate::corpus_rows::not_a_launch_set();
    for (i, ix) in tx.instructions.iter().enumerate() {
        if ix.program_id != crate::laserstream::PUMP_SWAP_PROGRAM {
            continue;
        }
        let Some(is_buy) = crate::corpus_rows::corpus_swap_side(&ix.data) else {
            continue;
        };
        let ordinal = u32::try_from(i).unwrap_or(u32::MAX);
        let row = match crate::corpus_rows::resolve_row_why(
            None,
            is_buy,
            &ix.accounts,
            &tx.account_keys,
            &tx.invalid_key_idx,
            bal,
            &not_launch,
        ) {
            Ok(r) => r,
            Err(why) => {
                stats.resolver_rejects += 1;
                *stats.reject_reasons.entry(why.as_str()).or_insert(0) += 1;
                // Population: which quote asset moved in the transaction (the frozen builder keys nothing on it, but
                // it explains WHY no sign pattern exists), and whether the tx also carried a USDC leg.
                let usdc = bal
                    .pre_tok
                    .iter()
                    .chain(bal.post_tok.iter())
                    .any(|e| e.mint == USDC_MINT);
                let key = format!(
                    "{}|{}",
                    why.as_str(),
                    if usdc {
                        "usdc_leg_in_tx"
                    } else {
                        "no_usdc_leg"
                    }
                );
                *stats.reject_population.entry(key).or_insert(0) += 1;
                continue;
            }
        };
        let Ok(tokens_raw) = i64::try_from(row.tokens_raw) else {
            stats.resolver_rejects += 1;
            *stats
                .reject_reasons
                .entry("tokens_raw_overflow")
                .or_insert(0) += 1;
            continue;
        };
        // Row identity lives in its own domain, keyed by the row's instruction index.
        if !dedup.first_time_row(&tx.signature, ordinal) {
            stats.duplicates += 1;
            continue;
        }
        stats.emitted += 1;
        out.push(ProvenancedEvent {
            event: AppEvent::CorpusFlowRow {
                mint: Mint(row.mint),
                venue: TradeVenue::PumpSwap,
                feature: FeatureBasis {
                    sol_lamports: row.sol_lamports,
                    tokens_raw,
                    trader: row.trader,
                },
                recv_unix_ms: tx.recv_unix_ms,
                slot: Some(tx.slot),
                fee_lamports: tx.fee_lamports,
                cu_consumed: tx.cu_consumed,
                event_id: corpus_row_id(&tx.signature, ordinal),
            },
            source: ProvenanceSource::LaserStreamTradeEvent,
            slot: tx.slot,
            is_live: tx.is_live,
        });
    }
}

/// Producer/consumer compatibility watchdog for the event path. The sidecar must stamp `meta.tx_ok`;
/// a producer that does not (an older build) makes EVERY pump transaction `tx_status_unknown`, which is
/// fail-closed but silently yields zero coverage. This turns that into an explicit readiness failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProducerCompat {
    pub status_known: u64,
    pub status_unknown: u64,
}

/// Unknown-status pump transactions tolerated before the producer is declared incompatible, provided it
/// has NEVER reported a status. (A single stray line from a healthy producer never trips it.)
pub const COMPAT_UNKNOWN_THRESHOLD: u64 = 32;

impl ProducerCompat {
    pub fn note(&mut self, tx: &LaserStreamTx) {
        if !tx
            .instructions
            .iter()
            .any(|i| i.program_id == PUMP_FUN_PROGRAM)
        {
            return;
        }
        match tx.tx_ok {
            Some(_) => self.status_known = self.status_known.saturating_add(1),
            None => self.status_unknown = self.status_unknown.saturating_add(1),
        }
    }
    /// `true` = READY. Not ready once >= threshold pump txs arrived and none ever carried a status.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.status_known > 0 || self.status_unknown < COMPAT_UNKNOWN_THRESHOLD
    }
    #[must_use]
    pub fn reason(&self) -> Option<&'static str> {
        if self.ready() {
            None
        } else {
            Some("producer_incompatible: no meta.tx_ok on any pump transaction (rebuild the sidecar from this source revision)")
        }
    }
}

/// Who owns curve trade history. In `Events` mode the snapshot-delta producer MUST NOT emit a
/// `MarketTrade` (it still supplies reserve state). `SnapshotDelta` is the legacy mode, kept so
/// a feed without transaction events is explicit rather than silently mixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveTradeSource {
    Events,
    SnapshotDelta,
}

impl CurveTradeSource {
    /// May a reserve-snapshot delta feed trade aggregates?
    #[must_use]
    pub fn snapshot_may_feed_trades(self) -> bool {
        matches!(self, CurveTradeSource::SnapshotDelta)
    }
}

/// Reconciliation of a curve snapshot against the last event-derived state for the same curve
/// slot. Covers ONLY virtual-token, real-SOL and real-token state; virtual SOL is not compared
/// (its relation to `sol_amount` is unexplained in ~10% of steps — no quote parity is claimed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconcile {
    Match,
    Mismatch,
}

#[must_use]
pub fn reconcile_snapshot(
    last_event: &CurveTradeEvent,
    snap_vtoken: u64,
    snap_real_sol: u64,
    snap_real_token: u64,
) -> Reconcile {
    if last_event.virtual_token == snap_vtoken
        && last_event.real_sol == snap_real_sol
        && last_event.real_token == snap_real_token
    {
        Reconcile::Match
    } else {
        Reconcile::Mismatch
    }
}

#[cfg(test)]
mod tests {

    /// Attribution is by match, not adjacency: a launch-slot transaction where the corpus-known instruction is NOT the
    /// nearest pump instruction before its event (create + buy + other pump ixs between) still resolves, and the
    /// event discriminator test compares the 8-byte tag only (the discriminator that follows is 8 bytes of its own).
    #[test]
    fn basis_attribution_matches_by_mint_and_side_not_by_adjacency() {
        let nl = crate::corpus_rows::not_a_launch_set();
        let mut t = tx(5, Some(true), vec![]);
        // no balances: refused by name, never zero-filled
        let e = CurveTradeEvent {
            mint: [7; 32],
            user: [3; 32],
            is_buy: true,
            sol_amount: 1,
            token_amount: 1,
            virtual_sol: 1,
            virtual_token: 1,
            real_sol: 0,
            real_token: 0,
            ix_ordinal: 4,
            quote: QuoteIdentity::Sol,
        };
        assert_eq!(
            corpus_basis_for(&t, 0, &e, &nl).unwrap_err(),
            "no_balances_on_wire"
        );
        t.instructions.push(ix(vec![0xaa; 16])); // unrelated pump ix, not corpus-known
        assert_eq!(
            corpus_basis_for(&t, 0, &e, &nl).unwrap_err(),
            "no_balances_on_wire"
        );
    }
    use super::*;
    use crate::laserstream::LaserStreamInstruction;

    const MINT: [u8; 32] = [7; 32];

    fn ev_data(
        mint: [u8; 32],
        user: u8,
        buy: bool,
        sol: u64,
        tok: u64,
        vs: u64,
        vt: u64,
    ) -> Vec<u8> {
        let mut d = TRADE_EVENT_PREFIX.to_vec();
        d.extend_from_slice(&mint);
        d.extend_from_slice(&sol.to_le_bytes());
        d.extend_from_slice(&tok.to_le_bytes());
        d.push(u8::from(buy));
        d.extend_from_slice(&[user; 32]);
        d.extend_from_slice(&0u64.to_le_bytes()); // timestamp
        d.extend_from_slice(&vs.to_le_bytes());
        d.extend_from_slice(&vt.to_le_bytes());
        d.extend_from_slice(&(vs / 2).to_le_bytes());
        d.extend_from_slice(&(vt / 2).to_le_bytes());
        d.extend_from_slice(&idl_tail(&[0u8; 32]));
        d
    }
    /// The IDL tail after `real_token_reserves`: fee_recipient .. last_update_timestamp, `ix_name`="buy", mayhem,
    /// cashback/buyback fields, an empty shareholders vec, then `quote_mint` + the V2 trailing amounts.
    fn idl_tail(quote_mint: &[u8; 32]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&[1u8; 32]); // fee_recipient
        t.extend_from_slice(&[0u8; 16]); // fee_bps, fee
        t.extend_from_slice(&[2u8; 32]); // creator
        t.extend_from_slice(&[0u8; 16]); // creator_fee_bps, creator_fee
        t.push(0); // track_volume
        t.extend_from_slice(&[0u8; 32]); // unclaimed, claimed, current_sol_volume, last_update_timestamp
        t.extend_from_slice(&3u32.to_le_bytes());
        t.extend_from_slice(b"buy");
        t.push(0); // mayhem_mode
        t.extend_from_slice(&[0u8; 32]); // cashback bps/amt, buyback bps/amt
        t.extend_from_slice(&0u32.to_le_bytes()); // shareholders
        t.extend_from_slice(quote_mint);
        t.extend_from_slice(&[0u8; 32]); // quote_amount, virtual_quote, real_quote, holder_rewards_bps
        t.extend_from_slice(&[0u8; 8]); // holder_rewards
        t
    }
    fn ix(data: Vec<u8>) -> LaserStreamInstruction {
        LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data,
            accounts: vec![],
            outer: None,
            depth: None,
        }
    }
    fn buy_ix() -> LaserStreamInstruction {
        let mut d = BUY_SELL_DISCS[0].to_vec();
        d.extend_from_slice(&[0u8; 16]);
        ix(d)
    }
    fn tx(sig: u8, ok: Option<bool>, ixs: Vec<LaserStreamInstruction>) -> LaserStreamTx {
        LaserStreamTx {
            slot: 100,
            signature: [sig; 64],
            account_keys: vec![],
            instructions: ixs,
            is_live: true,
            recv_unix_ms: Some(1_000),
            fee_lamports: Some(5_000),
            cu_consumed: Some(90_000),
            tx_ok: ok,
            balances: None,
            invalid_key_idx: vec![],
            repaired_zero_keys: 0,
        }
    }

    /// Two DISTINCT trades that share slot, trader, size, time AND post-trade price (identical in every
    /// economic field) stay two engine events with different ids; redelivering the SAME transaction
    /// yields nothing at the producer layer and, if it somehow reaches the engine, nothing there either.
    #[test]
    fn identical_looking_distinct_trades_survive_end_to_end_and_a_redelivery_does_not() {
        use pump_quant_app::decision_join::{DecisionCache, Ingest, TradeObs};
        use pump_quant_app::state_ledger::VenueLabel;
        let e = ev_data(MINT, 1, true, 10, 100, 1_000, 5_000);
        let t = tx(40, Some(true), vec![buy_ix(), ix(e.clone()), ix(e.clone())]);
        let mut dd = EventDedup::new(16);
        let mut out = Vec::new();
        assert_eq!(
            ingest_curve_tx(&t, &mut dd, &mut out),
            EventIngest::Produced {
                events: 2,
                duplicates: 0
            }
        );
        let ids: Vec<u128> = out
            .iter()
            .map(|p| match p.event {
                AppEvent::MarketTrade {
                    event_id: Some(i), ..
                } => i,
                _ => panic!("trade expected, with an event id"),
            })
            .collect();
        assert_ne!(ids[0], ids[1], "distinct ordinals must give distinct ids");
        let obs = |p: &ProvenancedEvent| match p.event {
            AppEvent::MarketTrade {
                mint,
                price_fp,
                quote_lamports,
                signed_base,
                buyer_entity,
                trader_pubkey,
                recv_unix_ms,
                slot,
                fee_lamports,
                cu_consumed,
                event_id,
                feature,
                ..
            } => TradeObs {
                mint: *mint.as_bytes(),
                price_fp,
                quote_lamports,
                signed_base,
                buyer_entity,
                trader: trader_pubkey,
                recv_unix_ms,
                slot,
                fee_lamports,
                cu_consumed,
                venue: VenueLabel::Pumpfun,
                event_id,
                feature,
            },
            _ => panic!(),
        };
        let mut c = DecisionCache::new();
        assert!(c.observe_launch(MINT, [9; 32], 1_000));
        // The two trades are field-for-field identical apart from the id.
        let (a, b) = (obs(&out[0]), obs(&out[1]));
        assert_eq!(
            (a.slot, a.trader, a.signed_base, a.recv_unix_ms, a.price_fp),
            (b.slot, b.trader, b.signed_base, b.recv_unix_ms, b.price_fp)
        );
        assert_eq!(c.observe_trade(&a), Ingest::Accepted);
        assert_eq!(
            c.observe_trade(&b),
            Ingest::Accepted,
            "second distinct event must survive"
        );
        // Producer layer: the identical transaction delivered again is dropped wholesale ...
        let mut again = Vec::new();
        assert_eq!(
            ingest_curve_tx(&t, &mut dd, &mut again),
            EventIngest::Produced {
                events: 0,
                duplicates: 2
            }
        );
        // ... and if a replay bypasses the producer, the engine layer drops it on the same ids.
        assert_eq!(c.observe_trade(&a), Ingest::Duplicate);
        assert_eq!(c.observe_trade(&b), Ingest::Duplicate);
        assert_eq!(c.counters().accepted, 2);
    }

    #[test]
    fn an_incompatible_producer_is_a_readiness_failure_not_silent_zero_coverage() {
        let mut c = ProducerCompat::default();
        for _ in 0..(COMPAT_UNKNOWN_THRESHOLD - 1) {
            c.note(&tx(1, None, vec![buy_ix()]));
        }
        assert!(c.ready(), "below threshold: not yet declared");
        c.note(&tx(1, None, vec![buy_ix()]));
        assert!(!c.ready());
        assert!(c.reason().unwrap().contains("rebuild the sidecar"));
        // Every one of those transactions was ALSO refused as an incomplete (named gap) -- no fallback.
        let mut out = Vec::new();
        assert_eq!(
            ingest_curve_tx(
                &tx(2, None, vec![buy_ix()]),
                &mut EventDedup::new(4),
                &mut out
            ),
            EventIngest::Incomplete("tx_status_unknown")
        );
        // One status-bearing tx proves the producer is compatible; later strays do not flip it back.
        c.note(&tx(3, Some(true), vec![buy_ix()]));
        assert!(c.ready());
        // Non-pump traffic never counts either way.
        let mut d = ProducerCompat::default();
        for _ in 0..100 {
            d.note(&tx(4, None, vec![]));
        }
        assert!(d.ready());
    }

    #[test]
    fn the_trade_event_discriminator_is_sha256_of_event_name() {
        // sha256("event:TradeEvent")[..8] == bddb7fd34ee661ee (checked offline with hashlib).
        assert_eq!(
            TRADE_EVENT_PREFIX[8..],
            [0xbd, 0xdb, 0x7f, 0xd3, 0x4e, 0xe6, 0x61, 0xee]
        );
    }

    #[test]
    fn a_multi_event_transaction_keeps_each_trade_exactly_once() {
        let t = tx(
            1,
            Some(true),
            vec![
                buy_ix(),
                ix(ev_data(MINT, 1, true, 10, 100, 1_000, 5_000)),
                buy_ix(),
                ix(ev_data(MINT, 2, false, 7, 70, 990, 5_070)),
            ],
        );
        let mut dd = EventDedup::new(16);
        let mut out = Vec::new();
        let r = ingest_curve_tx(&t, &mut dd, &mut out);
        assert_eq!(
            r,
            EventIngest::Produced {
                events: 2,
                duplicates: 0
            }
        );
        assert_eq!(out.len(), 2);
        // Replay of the same transaction: every event identity is a repeat.
        let mut out2 = Vec::new();
        let r2 = ingest_curve_tx(&t, &mut dd, &mut out2);
        assert_eq!(
            r2,
            EventIngest::Produced {
                events: 0,
                duplicates: 2
            }
        );
        assert!(out2.is_empty());
        assert_eq!(dd.duplicates, 2);
    }

    #[test]
    fn mixed_and_same_direction_events_in_one_slot_carry_per_trade_identity_and_fee_cu_per_row() {
        let t = tx(
            2,
            Some(true),
            vec![
                buy_ix(),
                ix(ev_data(MINT, 1, true, 10, 100, 1_000, 5_000)),
                buy_ix(),
                ix(ev_data(MINT, 1, true, 11, 110, 1_010, 4_890)),
                buy_ix(),
                ix(ev_data(MINT, 3, false, 5, 50, 1_005, 4_940)),
            ],
        );
        let mut out = Vec::new();
        ingest_curve_tx(&t, &mut EventDedup::new(8), &mut out);
        assert_eq!(out.len(), 3);
        let mut sides = Vec::new();
        for pe in &out {
            let AppEvent::MarketTrade {
                signed_base,
                fee_lamports,
                cu_consumed,
                trader_pubkey,
                slot,
                recv_unix_ms,
                venue,
                ..
            } = pe.event
            else {
                panic!()
            };
            assert_eq!(
                (fee_lamports, cu_consumed, slot, recv_unix_ms),
                (Some(5_000), Some(90_000), Some(100), Some(1_000))
            );
            assert_eq!(venue, Some(TradeVenue::PumpFun));
            assert!(trader_pubkey.is_some());
            sides.push(signed_base);
            assert_eq!(pe.source, ProvenanceSource::LaserStreamTradeEvent);
        }
        assert_eq!(sides, vec![100, 110, -50]);
    }

    #[test]
    fn failed_transactions_produce_nothing_and_unknown_status_is_a_named_gap() {
        let evs = vec![buy_ix(), ix(ev_data(MINT, 1, true, 10, 100, 1_000, 5_000))];
        let mut out = Vec::new();
        assert_eq!(
            ingest_curve_tx(
                &tx(3, Some(false), evs.clone()),
                &mut EventDedup::new(4),
                &mut out
            ),
            EventIngest::Nothing
        );
        assert_eq!(
            ingest_curve_tx(&tx(3, None, evs), &mut EventDedup::new(4), &mut out),
            EventIngest::Incomplete("tx_status_unknown")
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_successful_buy_without_a_trade_event_is_incomplete_not_silently_empty() {
        let mut out = Vec::new();
        let r = ingest_curve_tx(
            &tx(4, Some(true), vec![buy_ix()]),
            &mut EventDedup::new(4),
            &mut out,
        );
        assert_eq!(r, EventIngest::Incomplete("buy_sell_without_trade_event"));
        assert!(out.is_empty());
    }

    #[test]
    fn a_zero_reserve_trade_event_is_a_named_gap_not_a_noprice_drop() {
        let t = tx(
            9,
            Some(true),
            vec![buy_ix(), ix(ev_data(MINT, 1, true, 10, 100, 0, 5_000))],
        );
        let mut out = Vec::new();
        assert_eq!(
            ingest_curve_tx(&t, &mut EventDedup::new(4), &mut out),
            EventIngest::Incomplete("trade_event_unrepresentable")
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_truncated_trade_event_is_incomplete() {
        let mut d = ev_data(MINT, 1, true, 10, 100, 1_000, 5_000);
        d.truncate(100);
        let mut out = Vec::new();
        let r = ingest_curve_tx(
            &tx(5, Some(true), vec![buy_ix(), ix(d)]),
            &mut EventDedup::new(4),
            &mut out,
        );
        assert_eq!(r, EventIngest::Incomplete("trade_event_truncated"));
    }

    #[test]
    fn distinct_signatures_with_identical_content_are_distinct_events() {
        let body = vec![buy_ix(), ix(ev_data(MINT, 1, true, 10, 100, 1_000, 5_000))];
        let mut dd = EventDedup::new(8);
        let mut out = Vec::new();
        ingest_curve_tx(&tx(6, Some(true), body.clone()), &mut dd, &mut out);
        ingest_curve_tx(&tx(7, Some(true), body), &mut dd, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn snapshot_deltas_may_not_feed_trades_in_event_mode() {
        assert!(!CurveTradeSource::Events.snapshot_may_feed_trades());
        assert!(CurveTradeSource::SnapshotDelta.snapshot_may_feed_trades());
    }

    #[test]
    fn reconcile_covers_only_token_and_real_sol_state() {
        let e = decode_curve_trade_events(&tx(
            8,
            Some(true),
            vec![buy_ix(), ix(ev_data(MINT, 1, true, 10, 100, 1_000, 5_000))],
        ));
        let TxDecode::Events(v) = e else { panic!() };
        assert_eq!(
            reconcile_snapshot(&v[0], 5_000, 500, 2_500),
            Reconcile::Match
        );
        assert_eq!(
            reconcile_snapshot(&v[0], 5_001, 500, 2_500),
            Reconcile::Mismatch
        );
    }

    #[test]
    fn quote_identity_comes_from_the_events_own_quote_mint() {
        let mut d = ev_data(MINT, 1, true, 5, 5, 10, 10);
        let cut = d.len() - idl_tail(&[0u8; 32]).len();
        assert_eq!(decode_quote(&d), QuoteIdentity::Sol, "native sentinel");
        d.truncate(cut);
        d.extend_from_slice(&idl_tail(&WSOL_MINT));
        assert_eq!(decode_quote(&d), QuoteIdentity::Sol, "WSOL spelling");
        let usdc = [9u8; 32];
        d.truncate(cut);
        d.extend_from_slice(&idl_tail(&usdc));
        assert_eq!(
            decode_quote(&d),
            QuoteIdentity::Other(usdc),
            "verified non-SOL quote"
        );
        // An event that ends before quote_mint (older layout / truncated): NOT assumed SOL.
        d.truncate(cut + 60);
        assert_eq!(decode_quote(&d), QuoteIdentity::Unknown);
    }

    #[test]
    fn a_usdc_quoted_event_is_counted_outside_sol_and_is_not_a_gap() {
        let usdc = [9u8; 32];
        let mut d = ev_data(MINT, 1, true, 0, 50, 0, 100); // zero SOL fields, as on USDC curves
        let cut = d.len() - idl_tail(&[0u8; 32]).len();
        d.truncate(cut);
        d.extend_from_slice(&idl_tail(&usdc));
        let t = tx(1, Some(true), vec![buy_ix(), ix(d)]);
        let mut dd = EventDedup::new(16);
        let mut out = Vec::new();
        let r = ingest_curve_tx(&t, &mut dd, &mut out);
        assert!(out.is_empty(), "no engine event for an unsupported quote");
        assert_eq!(dd.unsupported_quote, 1);
        assert!(
            !matches!(r, EventIngest::Incomplete(_)),
            "unsupported quote is a counted population, not a gap"
        );
    }

    #[test]
    fn a_sol_trade_outside_the_corpus_table_is_admitted_without_a_feature_basis() {
        // buy_ix() uses BUY_SELL_DISCS[0] (corpus-known) but the tx carries no balances: basis refused by name.
        let t = tx(
            1,
            Some(true),
            vec![buy_ix(), ix(ev_data(MINT, 1, true, 5, 5, 10, 10))],
        );
        let mut dd = EventDedup::new(16);
        let mut out = Vec::new();
        let _ = ingest_curve_tx(&t, &mut dd, &mut out);
        assert_eq!(out.len(), 1, "still discovered/admitted");
        match out[0].event {
            AppEvent::MarketTrade { feature, .. } => assert!(feature.is_none()),
            _ => panic!(),
        }
        assert_eq!(dd.outside_corpus.get("no_balances_on_wire"), Some(&1));
        // V2 discriminators that the corpus table lacks are named, not guessed.
        let mut v2 = BUY_SELL_DISCS[3].to_vec();
        v2[0] ^= 0xFF; // not in the corpus table
        let t2 = tx(
            2,
            Some(true),
            vec![ix(v2), ix(ev_data(MINT, 1, true, 5, 5, 10, 10))],
        );
        let _ = decode_curve_trade_events(&t2);
    }

    fn pos(mut i: LaserStreamInstruction, outer: u32, depth: u32) -> LaserStreamInstruction {
        i.outer = Some(outer);
        i.depth = Some(depth);
        i
    }
    fn buy_for(trader_key: u8) -> LaserStreamInstruction {
        let mut d = BUY_SELL_DISCS[0].to_vec();
        d.extend_from_slice(&[0u8; 16]);
        LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data: d,
            accounts: vec![trader_key],
            outer: None,
            depth: None,
        }
    }
    fn two_trader_balances() -> (Vec<[u8; 32]>, crate::corpus_rows::BalanceMeta) {
        use crate::corpus_rows::{BalanceMeta, TokBal};
        let key = |b: u8| [b; 32];
        let keys: Vec<[u8; 32]> = (0u8..10).map(key).collect();
        let tb = |owner: u8, amt: u128| TokBal {
            mint: MINT,
            owner: key(owner),
            amount: amt,
        };
        (
            keys,
            BalanceMeta {
                pre_sol: vec![1000; 10],
                // trader 7 pays 300, trader 8 pays 700 (balance deltas, not swap amounts)
                post_sol: vec![1000, 1000, 1000, 1000, 1000, 1000, 1000, 700, 300, 1000],
                pre_tok: vec![tb(7, 0), tb(8, 0), tb(9, 1000)],
                post_tok: vec![tb(7, 30), tb(8, 70), tb(9, 900)],
            },
        )
    }

    /// INVOCATION-LEVEL association. One router transaction (outer 0, depth 1) CPIs into pump.fun twice (depth 2),
    /// same mint, same side, different traders; each pump instruction has its own TradeEvent self-CPI (depth 3) and a
    /// token-transfer CPI at depth 3-4 in between. Each event is bound to its PARENT instruction (nearest preceding
    /// instruction of the same outer group at depth-1), not to an adjacent or first-unused one. The wire order below
    /// puts the SECOND buy's inner transfer chain between its own call and event, and the event's `user` bytes are
    /// deliberately swapped relative to the wire order of the pump instructions to prove the row comes from the parent,
    /// not from the event payload.
    #[test]
    fn repeated_same_mint_same_side_events_are_bound_by_invocation_parent() {
        let (keys, bal) = two_trader_balances();
        let router = LaserStreamInstruction {
            program_id: [0xEE; 32],
            data: vec![],
            accounts: vec![],
            outer: None,
            depth: None,
        };
        let mut t = tx(
            9,
            Some(true),
            vec![
                pos(router, 0, 1),
                pos(buy_for(7), 0, 2),
                pos(ix(ev_data(MINT, 8, true, 11, 5, 10, 10)), 0, 3), // event of buy#1 (payload user deliberately 8)
                pos(buy_for(8), 0, 2),
                pos(
                    LaserStreamInstruction {
                        program_id: [0xDD; 32],
                        data: vec![],
                        accounts: vec![],
                        outer: None,
                        depth: None,
                    },
                    0,
                    3,
                ),
                pos(
                    LaserStreamInstruction {
                        program_id: [0xDD; 32],
                        data: vec![],
                        accounts: vec![],
                        outer: None,
                        depth: None,
                    },
                    0,
                    4,
                ),
                pos(ix(ev_data(MINT, 7, true, 22, 5, 10, 10)), 0, 3), // event of buy#2
            ],
        );
        t.account_keys = keys.clone();
        t.balances = Some(bal);
        assert_eq!(crate::laserstream::invocation_parent(&t, 2), Ok(1));
        assert_eq!(crate::laserstream::invocation_parent(&t, 6), Ok(3));
        let mut dd = EventDedup::new(16);
        let mut out = Vec::new();
        let _ = ingest_curve_tx(&t, &mut dd, &mut out);
        assert_eq!(out.len(), 2);
        let basis = |i: usize| match out[i].event {
            AppEvent::MarketTrade {
                feature: Some(f), ..
            } => f,
            _ => panic!("both events must resolve a basis"),
        };
        // event 0's parent is buy#1 (trader 7, -300) even though its payload names user 8; event 1's parent is buy#2.
        assert_eq!(basis(0).trader, keys[7]);
        assert_eq!(basis(0).sol_lamports, -300);
        assert_eq!(basis(1).trader, keys[8]);
        assert_eq!(basis(1).sol_lamports, -700);
        assert_eq!(dd.corpus_basis_resolved, 2);
    }

    /// With NO invocation positions from the producer, two candidate rows for one event are AMBIGUOUS and refused by
    /// name; order is never used to pick one.
    #[test]
    fn without_invocation_positions_two_candidate_rows_are_refused_not_ordered() {
        let (keys, bal) = two_trader_balances();
        let mut t = tx(
            9,
            Some(true),
            vec![
                buy_for(7),
                ix(ev_data(MINT, 7, true, 11, 5, 10, 10)),
                buy_for(8),
                ix(ev_data(MINT, 8, true, 22, 5, 10, 10)),
            ],
        );
        t.account_keys = keys;
        t.balances = Some(bal);
        let mut dd = EventDedup::new(16);
        let mut out = Vec::new();
        let _ = ingest_curve_tx(&t, &mut dd, &mut out);
        assert_eq!(out.len(), 2, "both events still admitted for discovery");
        assert!(out
            .iter()
            .all(|p| matches!(p.event, AppEvent::MarketTrade { feature: None, .. })));
        assert_eq!(dd.outside_corpus.get("attribution_ambiguous"), Some(&2));
        assert_eq!(dd.corpus_basis_resolved, 0);
    }

    /// A single candidate with no positions is unambiguous; an event whose declared parent is NOT a pump.fun
    /// corpus instruction is refused by name (never reassigned to a sibling).
    #[test]
    fn unique_candidate_without_positions_resolves_and_foreign_parent_is_refused() {
        let (keys, bal) = two_trader_balances();
        let mut t = tx(
            9,
            Some(true),
            vec![buy_for(7), ix(ev_data(MINT, 7, true, 11, 5, 10, 10))],
        );
        t.account_keys = keys.clone();
        t.balances = Some(bal.clone());
        let mut dd = EventDedup::new(16);
        let mut out = Vec::new();
        let _ = ingest_curve_tx(&t, &mut dd, &mut out);
        assert_eq!(dd.corpus_basis_resolved, 1);
        // positions given, parent of the event is a foreign program
        let foreign = LaserStreamInstruction {
            program_id: [0xEE; 32],
            data: vec![],
            accounts: vec![],
            outer: None,
            depth: None,
        };
        let mut t2 = tx(
            10,
            Some(true),
            vec![
                pos(buy_for(7), 0, 2),
                pos(foreign, 0, 2),
                pos(ix(ev_data(MINT, 7, true, 11, 5, 10, 10)), 0, 3),
            ],
        );
        t2.account_keys = keys;
        t2.balances = Some(bal);
        let mut dd2 = EventDedup::new(16);
        let mut out2 = Vec::new();
        let _ = ingest_curve_tx(&t2, &mut dd2, &mut out2);
        assert_eq!(out2.len(), 1);
        assert_eq!(dd2.outside_corpus.get("parent_not_pump_fun"), Some(&1));
        assert_eq!(dd2.corpus_basis_resolved, 0);
    }
}
