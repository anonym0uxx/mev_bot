//! `exit_markout` — exit-side markout / foregone-upside API (constitution §47).
//!
//! §47 asks the frozen evaluator to score every *exit* on a ruler finer than the
//! coarse per-fill horizon carried by [`crate::evaluator_stats::FillRow`]: after a
//! position is closed, where did the price go? For a long that we sold, price that
//! keeps climbing is *foregone upside* — profit the exit rule left on the table —
//! while price that falls is *loss avoided*. This module samples post-exit price
//! paths at mandated sub-second horizons and folds the two-sided markout per
//! [`ExitReason`].
//!
//! Horizons are nanoseconds ([`MarkoutHorizonNs`]) so the 250ms / 1s / 5s marks
//! §47 mandates are representable; `FillRow`'s whole-second horizon cannot express
//! them. Everything is integer / basis-point (§22): no floats, no wall-clock, no
//! RNG. Grouping is a `BTreeMap` over `(reason, horizon_ns)` so output order is a
//! deterministic function of the keys.

use std::collections::BTreeMap;

use crate::evaluator_stats::Side;

// ============================================================================
// Mandated markout horizons (nanoseconds)
// ============================================================================

/// A post-exit sampling horizon, in nanoseconds. A newtype so a horizon can
/// never be silently confused with a price, a bps figure, or a second count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkoutHorizonNs(pub u64);

/// 250 milliseconds, in ns (§47 fastest mandated exit-markout horizon).
pub const H_250MS_NS: u64 = 250_000_000;
/// 1 second, in ns.
pub const H_1S_NS: u64 = 1_000_000_000;
/// 5 seconds, in ns.
pub const H_5S_NS: u64 = 5_000_000_000;
/// 30 seconds, in ns.
pub const H_30S_NS: u64 = 30_000_000_000;
/// 5 minutes, in ns (slow horizon for the moonbag/late tail).
pub const H_5M_NS: u64 = 300_000_000_000;

/// The default mandated horizon ladder (§47): 250ms, 1s, 5s, 30s, 5m.
pub const MANDATED_HORIZONS_NS: [u64; 5] = [H_250MS_NS, H_1S_NS, H_5S_NS, H_30S_NS, H_5M_NS];

// ============================================================================
// Exit reasons
// ============================================================================

/// Why a position was exited. Foregone-upside is bucketed per reason so a
/// systematically-early rule (e.g. an over-eager stop) is distinguishable from a
/// take-profit that correctly harvested the move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitReason {
    /// Take-profit ladder rung hit.
    TakeProfit,
    /// Hard stop-loss.
    StopLoss,
    /// Trailing-stop giveback.
    TrailingStop,
    /// Time-based stop (max hold elapsed).
    TimeStop,
    /// Liquidity / rug-risk abort.
    LiquidityAbort,
    /// Discretionary / manual close.
    Manual,
}

// ============================================================================
// Inputs
// ============================================================================

/// One post-exit price observation: the fill price at which the position was
/// closed and a later price sampled `horizon_ns` after the exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitMarkoutRow {
    /// Why the position was exited.
    pub reason: ExitReason,
    /// Side the position was closed on (a long is closed by a [`Side::Sell`]).
    pub side: Side,
    /// Price at which the exit filled (fixed-point / integer).
    pub exit_price: u64,
    /// Horizon at which `later_price` was observed, ns.
    pub horizon_ns: u64,
    /// Price observed `horizon_ns` after the exit (fixed-point / integer).
    pub later_price: u64,
}

impl ExitMarkoutRow {
    /// Test/golden-vector constructor.
    pub fn test(
        reason: ExitReason,
        side: Side,
        exit_price: u64,
        horizon_ns: u64,
        later: u64,
    ) -> Self {
        ExitMarkoutRow {
            reason,
            side,
            exit_price,
            horizon_ns,
            later_price: later,
        }
    }
}

// ============================================================================
// Integer helpers (no floats)
// ============================================================================

/// Signed bps move `from -> to`: `(to - from) * 10_000 / from`, `i128` interim,
/// truncating toward zero. `from == 0` has no defined relative move -> `0`.
fn signed_bps_move(from: u64, to: u64) -> i64 {
    if from == 0 {
        return 0;
    }
    let num = to as i128 - from as i128;
    ((num * 10_000) / from as i128) as i64
}

/// Post-exit delta favorable to the *foregone-upside* question: how far did the
/// price continue in the direction the position would have profited from, had it
/// stayed open? A closed long (`Side::Sell`) benefits from further upside, so
/// positive == price kept rising after the sell; a closed short from downside.
fn foregone_direction_bps(side: Side, exit_price: u64, later_price: u64) -> i64 {
    let raw = signed_bps_move(exit_price, later_price);
    match side {
        // Closing a long by selling: upside after the sell is foregone.
        Side::Sell => raw,
        // Closing a short by buying: downside after the buy is foregone.
        Side::Buy => -raw,
    }
}

/// Integer median of an already-sorted slice; even length averages the two
/// central elements (`i128` interim). Empty -> 0.
fn median_sorted(sorted: &[i64]) -> i64 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        let a = sorted[n / 2 - 1] as i128;
        let b = sorted[n / 2] as i128;
        ((a + b) / 2) as i64
    }
}

// ============================================================================
// Markout-cell table
// ============================================================================

/// One markout cell: the `(reason, horizon_ns)` bucket's post-exit distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkoutCellNs {
    /// Exit reason of this bucket.
    pub reason: ExitReason,
    /// Sampling horizon of this bucket, ns.
    pub horizon_ns: u64,
    /// Number of samples in the bucket.
    pub n: u32,
    /// Median directional (continuation) markout, bps. Positive == price kept
    /// moving the way the closed position would have profited from.
    pub delta_bp: i64,
    /// Mean directional markout in fixed-point `bps * 100` (no float in output).
    pub mean_bp_x100: i64,
}

/// Build the `(reason, horizon_ns)` markout-cell table over supplied
/// `(exit_price, later_price)` samples (§47).
///
/// Only samples whose `horizon_ns` is present in `horizons_ns` are bucketed;
/// each bucket's directional markouts are sign-adjusted so positive is the
/// direction the closed position would have profited from, then reduced to a
/// median and a fixed-point mean. Empty buckets are omitted entirely (never
/// emitted as zeros). Output is ordered deterministically by
/// `(reason, horizon_ns)`. Pure, integer bp.
pub fn exit_markout_cells(rows: &[ExitMarkoutRow], horizons_ns: &[u64]) -> Vec<MarkoutCellNs> {
    let mut buckets: BTreeMap<(ExitReason, u64), Vec<i64>> = BTreeMap::new();

    for r in rows {
        if !horizons_ns.contains(&r.horizon_ns) {
            continue;
        }
        let bps = foregone_direction_bps(r.side, r.exit_price, r.later_price);
        buckets
            .entry((r.reason, r.horizon_ns))
            .or_default()
            .push(bps);
    }

    let mut cells: Vec<MarkoutCellNs> = Vec::with_capacity(buckets.len());
    for ((reason, horizon_ns), mut values) in buckets {
        if values.is_empty() {
            continue;
        }
        values.sort_unstable();
        let n = values.len();
        let delta_bp = median_sorted(&values);
        let sum: i128 = values.iter().map(|&v| v as i128).sum();
        let mean_bp_x100 = ((sum * 100) / n as i128) as i64;
        cells.push(MarkoutCellNs {
            reason,
            horizon_ns,
            n: n as u32,
            delta_bp,
            mean_bp_x100,
        });
    }
    cells
}

// ============================================================================
// Foregone-upside aggregate
// ============================================================================

/// Per-`(reason, horizon)` foregone-upside aggregate: the sum of continuation
/// upside the exit rule left on the table at that horizon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForegoneUpside {
    /// Exit reason.
    pub reason: ExitReason,
    /// Horizon at which foregone upside was measured, ns.
    pub horizon_ns: u64,
    /// Number of exits contributing.
    pub n: u32,
    /// Σ max(0, directional markout) — upside foregone, bps.
    pub foregone_bp_sum: i128,
    /// Σ max(0, −directional markout) — loss the exit *avoided*, bps.
    pub loss_avoided_bp_sum: i128,
}

impl ForegoneUpside {
    /// Net two-sided contribution: loss avoided minus upside foregone (bps). A
    /// positive value means the exit dodged more downside than it gave up.
    pub fn net_bp(&self) -> i128 {
        self.loss_avoided_bp_sum - self.foregone_bp_sum
    }
}

/// Fold per-`(reason, horizon)` foregone-upside over supplied post-exit samples
/// (§47).
///
/// Every exit is scored on BOTH sides of the ruler: `foregone_bp_sum`
/// accumulates continuation upside the rule discarded and `loss_avoided_bp_sum`
/// the downside it dodged — an exit is never scored on avoided loss alone. Only
/// samples whose horizon is in `horizons_ns` are counted. Output is ordered by
/// `(reason, horizon_ns)`; deterministic; integer bp.
pub fn foregone_upside(rows: &[ExitMarkoutRow], horizons_ns: &[u64]) -> Vec<ForegoneUpside> {
    let mut acc: BTreeMap<(ExitReason, u64), ForegoneUpside> = BTreeMap::new();

    for r in rows {
        if !horizons_ns.contains(&r.horizon_ns) {
            continue;
        }
        let bps = foregone_direction_bps(r.side, r.exit_price, r.later_price);
        let e = acc
            .entry((r.reason, r.horizon_ns))
            .or_insert(ForegoneUpside {
                reason: r.reason,
                horizon_ns: r.horizon_ns,
                n: 0,
                foregone_bp_sum: 0,
                loss_avoided_bp_sum: 0,
            });
        e.n += 1;
        if bps > 0 {
            e.foregone_bp_sum = e
                .foregone_bp_sum
                .checked_add(bps as i128)
                .expect("foregone_upside: foregone overflow");
        } else {
            e.loss_avoided_bp_sum = e
                .loss_avoided_bp_sum
                .checked_add((-(bps as i128)).max(0))
                .expect("foregone_upside: loss_avoided overflow");
        }
    }

    acc.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_group_by_reason_and_horizon() {
        let rows = vec![
            ExitMarkoutRow::test(ExitReason::TakeProfit, Side::Sell, 1_000, H_1S_NS, 1_100),
            ExitMarkoutRow::test(ExitReason::TakeProfit, Side::Sell, 1_000, H_1S_NS, 1_200),
            ExitMarkoutRow::test(ExitReason::StopLoss, Side::Sell, 1_000, H_1S_NS, 900),
        ];
        let cells = exit_markout_cells(&rows, &MANDATED_HORIZONS_NS);
        assert_eq!(cells.len(), 2);
        // TakeProfit < StopLoss in enum order.
        assert_eq!(cells[0].reason, ExitReason::TakeProfit);
        assert_eq!(cells[0].n, 2);
        // median of +1000, +2000 bps = 1500.
        assert_eq!(cells[0].delta_bp, 1_500);
        assert_eq!(cells[0].mean_bp_x100, 150_000);
        // StopLoss sold at 1000, price fell to 900 -> -1000 bps continuation.
        assert_eq!(cells[1].delta_bp, -1_000);
    }

    #[test]
    fn short_side_inverts_direction() {
        // Closed a short by buying at 1000; price fell to 900 -> foregone downside
        // is favorable (+1000 bps directional).
        let rows = vec![ExitMarkoutRow::test(
            ExitReason::TakeProfit,
            Side::Buy,
            1_000,
            H_250MS_NS,
            900,
        )];
        let cells = exit_markout_cells(&rows, &[H_250MS_NS]);
        assert_eq!(cells[0].delta_bp, 1_000);
    }

    #[test]
    fn foregone_is_two_sided() {
        let rows = vec![
            // sold at 1000, ran to 1500 -> +5000 bps foregone upside.
            ExitMarkoutRow::test(ExitReason::StopLoss, Side::Sell, 1_000, H_5S_NS, 1_500),
            // sold at 1000, fell to 500 -> +5000 bps loss avoided.
            ExitMarkoutRow::test(ExitReason::StopLoss, Side::Sell, 1_000, H_5S_NS, 500),
        ];
        let f = foregone_upside(&rows, &[H_5S_NS]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].n, 2);
        assert_eq!(f[0].foregone_bp_sum, 5_000);
        assert_eq!(f[0].loss_avoided_bp_sum, 5_000);
        assert_eq!(f[0].net_bp(), 0);
    }

    #[test]
    fn horizon_filter_excludes_unrequested() {
        let rows = vec![ExitMarkoutRow::test(
            ExitReason::TimeStop,
            Side::Sell,
            1_000,
            H_5M_NS,
            2_000,
        )];
        // Only ask for 1s: the 5m sample is excluded, no cells.
        assert!(exit_markout_cells(&rows, &[H_1S_NS]).is_empty());
        assert!(foregone_upside(&rows, &[H_1S_NS]).is_empty());
    }

    #[test]
    fn deterministic_repeat() {
        let rows = vec![
            ExitMarkoutRow::test(ExitReason::TrailingStop, Side::Sell, 1_000, H_1S_NS, 1_050),
            ExitMarkoutRow::test(ExitReason::TrailingStop, Side::Sell, 1_000, H_1S_NS, 980),
            ExitMarkoutRow::test(ExitReason::Manual, Side::Sell, 2_000, H_30S_NS, 2_400),
        ];
        let a = exit_markout_cells(&rows, &MANDATED_HORIZONS_NS);
        let b = exit_markout_cells(&rows, &MANDATED_HORIZONS_NS);
        assert_eq!(a, b);
        let fa = foregone_upside(&rows, &MANDATED_HORIZONS_NS);
        let fb = foregone_upside(&rows, &MANDATED_HORIZONS_NS);
        assert_eq!(fa, fb);
    }

    #[test]
    fn zero_exit_price_is_neutral() {
        let rows = vec![ExitMarkoutRow::test(
            ExitReason::LiquidityAbort,
            Side::Sell,
            0,
            H_1S_NS,
            1_000,
        )];
        let cells = exit_markout_cells(&rows, &[H_1S_NS]);
        assert_eq!(cells[0].delta_bp, 0);
    }

    #[test]
    fn horizon_newtype_holds_ns() {
        assert_eq!(MarkoutHorizonNs(H_250MS_NS).0, 250_000_000);
        assert_eq!(MANDATED_HORIZONS_NS.len(), 5);
    }
}
