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
    cap: usize,
    pub duplicates: u64,
}

impl EventDedup {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
            duplicates: 0,
        }
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

/// Build the engine event for one decoded trade. `None` when a field cannot be represented
/// (zero token reserve, amount above `i64`): refused, never clamped.
#[must_use]
pub fn curve_trade_to_event(
    t: &CurveTradeEvent,
    tx: &LaserStreamTx,
    is_live: bool,
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
            for e in &evs {
                if !dedup.first_time(&tx.signature, e.ix_ordinal) {
                    d += 1;
                    continue;
                }
                match curve_trade_to_event(e, tx, tx.is_live) {
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
        d
    }
    fn ix(data: Vec<u8>) -> LaserStreamInstruction {
        LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data,
            accounts: vec![],
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
}
