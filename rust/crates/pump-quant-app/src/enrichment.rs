//! The enriched candidate state — the corpus's `ENRICHED CANDIDATE STATE` block, derived
//! live.
//!
//! Source of truth: `/training/v2/code/src/v2/rl/build_c9_enrichment_full.py` lines 119-169, the
//! script that produced the `enriched` block of every c-series row. Its snapshot is a
//! strict as-of-t reduction over the gold tape's per-mint events (cutoff `recv_unix_ms <= t_dec`,
//! inclusive), which is the same tape, the same clock and the same window law the state ledger
//! uses — so the two belong to the same family and are kept in one place:
//!
//! ```text
//! bal[trader] += tokens_raw      (signed; a sell is negative)     -> holders, shares, HHI
//! vol         += |sol_lamports|                                    -> volume_sol_at_t
//! rt[trader]  += 1 buy / -1 sell                                   -> wash_ratio
//! slots[slot] += {trader}                                          -> bundle_slots / bundle_wallets
//! ```
//!
//! These are the numbers the entry prompt renders as `ENRICHED CANDIDATE STATE`, and they are
//! *not* the flow reducer's features: `FlowReducer` is windowed and rolling, this is cumulative
//! to the decision clock. Both are needed; neither substitutes for the other.
//!
//! WHAT IS DELIBERATELY NOT HERE. `mcap_sol_at_t` and `mcap_source` are not computed from trades:
//! the corpus reads them from the row's curve/AMM state (`msrc`), and the live equivalent is the
//! curve-state plane ([`crate::curve_state`]). They are passed in by the caller rather than
//! guessed here.
//!
//! PARITY STATUS. The arithmetic below is a faithful port of the corpus expressions and is
//! unit-pinned against hand-computed values. Byte parity against a real c12 row still requires
//! the fixture generator that drives both implementations over the same trades (the
//! `gen_state_ledger_fixture.py` pattern) — until that runs, this module is *ported*, not
//! *proven*, and the C8 gate stays shut on that basis.

use std::collections::{BTreeMap, BTreeSet};

/// One tape event, as the enrichment window consumes it.
///
/// The trader is the WALLET, not the engine's hashed entity: the corpus's `bal[trader]` is keyed
/// by address, and hashing first would merge distinct wallets exactly where concentration
/// (`top1_float_share`, `holder_hhi`) is the quantity being measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnrichmentTrade {
    /// The producer's wire receive time — the same clock the corpus windows on.
    pub recv_unix_ms: i64,
    /// The trader's wallet.
    pub trader: [u8; 32],
    /// The signed token leg: positive for a buy, negative for a sell.
    pub tokens_raw: i128,
    /// The SOL leg, unsigned (the corpus takes `abs`).
    pub sol_lamports: u64,
    /// The slot the trade landed in; `None` when the tape had no slot evidence.
    pub slot: Option<u64>,
}

/// Why the enrichment could not be produced for a clock. Every arm corresponds to a case the
/// corpus itself drops the row for, so refusing here is reproducible rather than conservative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichmentGap {
    /// Fewer than two events at or before the decision clock. The corpus requires two
    /// (`len(cutoff) < 2`), and a single trade cannot describe a distribution.
    InsufficientHistory,
    /// The shares did not form a distribution in `[0, 1]`. The corpus rejects these rows
    /// outright; a served prompt cannot contain one.
    InconsistentShares,
}

/// The corpus's `enriched` record, minus the two caller-supplied market-cap fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnrichedSnapshot {
    /// `holders_at_t`: wallets with a strictly positive net token balance.
    pub holders_at_t: u64,
    /// `top1_float_share`: largest positive balance / total positive balance.
    pub top1_float_share: f64,
    /// `top5_float_share`: five largest / total positive balance.
    pub top5_float_share: f64,
    /// `holder_hhi`: sum of squared positive-balance shares.
    pub holder_hhi: f64,
    /// `bundle_slots`: slots in which >= 2 distinct wallets traded.
    pub bundle_slots: u64,
    /// `bundle_wallets`: distinct wallets across those slots.
    pub bundle_wallets: u64,
    /// `n_buys_at_t`.
    pub n_buys_at_t: u64,
    /// `n_sells_at_t`.
    pub n_sells_at_t: u64,
    /// `volume_sol_at_t`: summed absolute SOL leg, in SOL.
    pub volume_sol_at_t: f64,
    /// `round_trip_wallets`: wallets whose buy count equals their sell count.
    pub round_trip_wallets: u64,
    /// `wash_ratio`: round-trip wallets / every wallet that traded, at or before the clock.
    pub wash_ratio: f64,
}

/// Lamports per SOL. The tape's SOL legs are lamports; the corpus reports SOL.
const LAMPORTS_PER_SOL: f64 = 1e9;

/// Reduce the trades at or before `t_dec_ms` into the corpus's snapshot.
///
/// Strictly causal and inclusive at the clock, matching `[e for e in ev if e[0] <= t_dec]`.
pub fn enrich(
    trades: &[EnrichmentTrade],
    t_dec_ms: i64,
) -> Result<EnrichedSnapshot, EnrichmentGap> {
    let mut balances: BTreeMap<[u8; 32], i128> = BTreeMap::new();
    let mut round_trips: BTreeMap<[u8; 32], i64> = BTreeMap::new();
    let mut slots: BTreeMap<u64, BTreeSet<[u8; 32]>> = BTreeMap::new();
    let mut volume_lamports: u128 = 0;
    let mut n_buys = 0u64;
    let mut n_sells = 0u64;
    let mut cutoff_len = 0usize;

    for t in trades.iter().filter(|t| t.recv_unix_ms <= t_dec_ms) {
        cutoff_len += 1;
        *balances.entry(t.trader).or_insert(0) += t.tokens_raw;
        volume_lamports += u128::from(t.sol_lamports);
        if t.tokens_raw > 0 {
            n_buys += 1;
            *round_trips.entry(t.trader).or_insert(0) += 1;
        } else {
            n_sells += 1;
            *round_trips.entry(t.trader).or_insert(0) -= 1;
        }
        if let Some(slot) = t.slot {
            slots.entry(slot).or_default().insert(t.trader);
        }
    }

    if cutoff_len < 2 {
        return Err(EnrichmentGap::InsufficientHistory);
    }

    // Only strictly positive balances are holders; a wallet that sold out is not one.
    let mut held: Vec<f64> = balances
        .values()
        .filter(|b| **b > 0)
        .map(|b| *b as f64)
        .collect();
    let holders_at_t = held.len() as u64;
    let total: f64 = held.iter().sum();
    let total = if total == 0.0 { 1.0 } else { total };

    held.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let top1 = held.first().copied().unwrap_or(0.0) / total;
    let top5 = held.iter().take(5).sum::<f64>() / total;
    let hhi: f64 = held.iter().map(|x| (x / total).powi(2)).sum();

    let in_unit = |x: f64| (0.0..=1.0).contains(&x);
    if !(in_unit(top1) && in_unit(top5) && in_unit(hhi)) {
        return Err(EnrichmentGap::InconsistentShares);
    }

    let bundled: Vec<&BTreeSet<[u8; 32]>> = slots
        .values()
        .filter(|wallets| wallets.len() >= 2)
        .collect();
    let bundle_slots = bundled.len() as u64;
    let bundle_wallets = bundled.iter().map(|w| w.len() as u64).sum();

    let round_trip_wallets = round_trips.values().filter(|n| **n == 0).count() as u64;
    // The denominator is EVERY wallet that traded, not just the survivors: a wallet that bought
    // and sold out is precisely the one a wash is made of.
    let wash_ratio = round_trip_wallets as f64 / (balances.len().max(1) as f64);

    Ok(EnrichedSnapshot {
        holders_at_t,
        top1_float_share: top1,
        top5_float_share: top5,
        holder_hhi: hhi,
        bundle_slots,
        bundle_wallets,
        n_buys_at_t: n_buys,
        n_sells_at_t: n_sells,
        volume_sol_at_t: volume_lamports as f64 / LAMPORTS_PER_SOL,
        round_trip_wallets,
        wash_ratio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(t: i64, who: u8, tokens: i128, sol: u64, slot: Option<u64>) -> EnrichmentTrade {
        EnrichmentTrade {
            recv_unix_ms: t,
            trader: [who; 32],
            tokens_raw: tokens,
            sol_lamports: sol,
            slot,
        }
    }

    /// Three wallets, hand-computed: A holds 60, B 30, C 10 of a 100 float; A and B traded in
    /// the same slot, which is what a bundle is (>= 2 distinct wallets in one slot), and C only
    /// ever bought once, so nothing is bundled with it.
    #[test]
    fn shares_and_bundles_match_hand_computation() {
        let trades = vec![
            trade(100, 1, 60, 600_000_000, Some(7)),
            trade(101, 2, 30, 300_000_000, Some(7)),
            trade(102, 3, 10, 100_000_000, Some(9)),
        ];
        let s = enrich(&trades, 102).expect("three trades is history");
        assert_eq!(s.holders_at_t, 3);
        assert!((s.top1_float_share - 0.6).abs() < 1e-12, "{s:?}");
        assert!((s.top5_float_share - 1.0).abs() < 1e-12, "{s:?}");
        assert!((s.holder_hhi - (0.36 + 0.09 + 0.01)).abs() < 1e-12, "{s:?}");
        assert_eq!(s.bundle_slots, 1, "only slot 7 held two wallets");
        assert_eq!(s.bundle_wallets, 2);
        assert_eq!(s.n_buys_at_t, 3);
        assert_eq!(s.n_sells_at_t, 0);
        assert!((s.volume_sol_at_t - 1.0).abs() < 1e-12, "{s:?}");
    }

    /// Strict causality: an event exactly at the clock counts, one millisecond later does not.
    /// This is the boundary the whole corpus hangs on (`<= t_dec`).
    #[test]
    fn the_window_is_inclusive_at_the_clock_and_closed_after_it() {
        let trades = vec![
            trade(100, 1, 10, 1_000, None),
            trade(200, 2, 10, 1_000, None),
            trade(201, 3, 999, 999_999, None),
        ];
        let s = enrich(&trades, 200).unwrap();
        assert_eq!(s.n_buys_at_t, 2, "the 201 print must not be visible");
        assert_eq!(s.holders_at_t, 2);
        assert!((s.top1_float_share - 0.5).abs() < 1e-12);
    }

    /// A wallet that bought and sold out round-trips and is a wash; a wallet that bought and
    /// stayed is not. The denominator counts both, so a pure-wash tape is 1.0.
    #[test]
    fn wash_is_round_trips_over_every_wallet_that_traded() {
        let trades = vec![
            trade(10, 1, 100, 5_000, Some(1)),
            trade(11, 1, -100, 6_000, Some(1)),
            trade(12, 2, 100, 5_000, None),
        ];
        let s = enrich(&trades, 12).unwrap();
        assert_eq!(s.round_trip_wallets, 1);
        assert_eq!(s.holders_at_t, 1, "wallet 1 sold out, so it does not hold");
        assert!((s.wash_ratio - 0.5).abs() < 1e-12, "{s:?}");
        assert!((s.top1_float_share - 1.0).abs() < 1e-12);

        let pure_wash = vec![
            trade(10, 1, 100, 5_000, Some(1)),
            trade(11, 1, -100, 6_000, Some(1)),
        ];
        let s = enrich(&pure_wash, 11).unwrap();
        assert!((s.wash_ratio - 1.0).abs() < 1e-12, "{s:?}");
        assert_eq!(s.holders_at_t, 0);
    }

    /// A wallet that sells without ever buying goes negative, and a negative balance is not a
    /// holder — the corpus's `v > 0` filter. It still counts in the wash denominator.
    #[test]
    fn a_net_short_wallet_is_not_a_holder_but_still_counts_for_wash() {
        let trades = vec![
            trade(10, 1, 100, 5_000, None),
            trade(11, 2, -50, 9_000, None),
        ];
        let s = enrich(&trades, 11).unwrap();
        assert_eq!(s.holders_at_t, 1);
        assert!((s.top1_float_share - 1.0).abs() < 1e-12);
        assert!(
            (s.wash_ratio - 0.0).abs() < 1e-12,
            "1 buy / 1 sell is not balanced"
        );
    }

    #[test]
    fn a_single_trade_is_not_a_distribution() {
        let trades = vec![trade(10, 1, 100, 5_000, None)];
        assert_eq!(enrich(&trades, 10), Err(EnrichmentGap::InsufficientHistory));
        assert_eq!(enrich(&[], 10), Err(EnrichmentGap::InsufficientHistory));
        // Two events, but only one is inside the window.
        let trades = vec![
            trade(10, 1, 100, 5_000, None),
            trade(99, 2, 100, 5_000, None),
        ];
        assert_eq!(enrich(&trades, 10), Err(EnrichmentGap::InsufficientHistory));
    }
}
