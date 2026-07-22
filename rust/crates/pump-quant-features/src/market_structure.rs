//! Deterministic bar-level market-structure feature family (constitution 21.6).
//!
//! Responsibility: compute the *price-structure* states the constitution names in
//! the "Bar and market-structure feature family" — compression/expansion,
//! breakout-and-retest, failed-breakdown/reclaim, and sweep-and-reclaim — over the
//! multi-timeframe [`Bar`] sequences produced by [`crate::bar::BarBuilder`]. These
//! are distinct from the §21.7 AMM order-flow catalog in [`crate::micro`]: they
//! describe how *price* traces prior swing levels and range width, not how swap
//! flow accumulates.
//!
//! Every function here is pure and deterministic over the bar slice it is given
//! (constitution 20 point-in-time: a detector only ever reads bars at or before the
//! decision index — the caller passes the closed bars it is allowed to see). All
//! arithmetic is integer / fixed-point [`i128`] in [`crate::types::PRICE_SCALE`]
//! units with explicit saturating overflow contracts (constitution 22) — no
//! floating point, no wall clock, no RNG, no I/O. State is bounded by the input
//! slice; nothing here retains growing internal state (constitution 57/99).
//!
//! These are *research-gated structural features*, not assumed-predictive signals:
//! this module computes them faithfully; admission lives in other planes.

use crate::bar::Bar;

/// Price range of a bar in [`crate::types::PRICE_SCALE`] units: `high_fp - low_fp`.
///
/// Bars satisfy `high_fp >= low_fp` by construction, so the result is non-negative;
/// `saturating_sub` is the explicit overflow contract (constitution 22).
#[must_use]
pub fn bar_range_fp(bar: &Bar) -> i128 {
    bar.high_fp.saturating_sub(bar.low_fp)
}

/// Highest `high_fp` across `bars`, or `None` if `bars` is empty (constitution 21.6
/// prior-range high). This is the resistance level breakout/sweep detectors test.
#[must_use]
pub fn highest_high_fp(bars: &[Bar]) -> Option<i128> {
    bars.iter().map(|b| b.high_fp).max()
}

/// Lowest `low_fp` across `bars`, or `None` if `bars` is empty (constitution 21.6
/// prior-range low). This is the support level breakdown/sweep detectors test.
#[must_use]
pub fn lowest_low_fp(bars: &[Bar]) -> Option<i128> {
    bars.iter().map(|b| b.low_fp).min()
}

/// Compression vs expansion of range width (constitution 21.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeState {
    /// Recent mean bar range has contracted relative to the baseline (a squeeze).
    Compression,
    /// Recent mean bar range has expanded relative to the baseline (a range break).
    Expansion,
    /// Recent range is neither contracted nor expanded past the thresholds.
    Neutral,
}

/// Classify compression/expansion by comparing the mean bar range over the most
/// recent `recent` bars against the mean over the `baseline` bars immediately
/// preceding them (constitution 21.6 compression/expansion).
///
/// The comparison is done by cross-multiplication so it stays exact integer math
/// with no division rounding: recent is a *compression* if
/// `recent_sum * baseline_n * 10_000 <= baseline_sum * recent_n * contraction_bps`
/// and an *expansion* if the analogous `>= ... * expansion_bps` holds, where the
/// `*_bps` thresholds are the recent/baseline mean-range ratio in basis points
/// (e.g. `contraction_bps = 6_000` means "recent mean range <= 60% of baseline").
///
/// Returns `None` when there are fewer than `recent + baseline` bars, when either
/// window length is zero, or when the baseline range sum is zero (ratio undefined).
#[must_use]
pub fn range_state(
    bars: &[Bar],
    recent: usize,
    baseline: usize,
    contraction_bps: u32,
    expansion_bps: u32,
) -> Option<RangeState> {
    if recent == 0 || baseline == 0 || bars.len() < recent + baseline {
        return None;
    }
    let n = bars.len();
    // Recent = last `recent` bars; baseline = the `baseline` bars just before them.
    let recent_slice = &bars[n - recent..];
    let baseline_slice = &bars[n - recent - baseline..n - recent];

    let recent_sum: i128 = recent_slice
        .iter()
        .fold(0i128, |acc, b| acc.saturating_add(bar_range_fp(b)));
    let baseline_sum: i128 = baseline_slice
        .iter()
        .fold(0i128, |acc, b| acc.saturating_add(bar_range_fp(b)));
    if baseline_sum == 0 {
        return None;
    }

    let recent_n = recent as i128;
    let baseline_n = baseline as i128;
    // lhs = recent_mean scaled; rhs_* = baseline_mean * threshold, both over the
    // common denominator recent_n * baseline_n, so we compare numerators directly.
    let lhs = recent_sum.saturating_mul(baseline_n).saturating_mul(10_000);
    let base = baseline_sum.saturating_mul(recent_n);
    let contraction_rhs = base.saturating_mul(i128::from(contraction_bps));
    let expansion_rhs = base.saturating_mul(i128::from(expansion_bps));

    if lhs <= contraction_rhs {
        Some(RangeState::Compression)
    } else if lhs >= expansion_rhs {
        Some(RangeState::Expansion)
    } else {
        Some(RangeState::Neutral)
    }
}

/// Indices of swing-high pivots in `bars` (constitution 21.6 swing structure).
///
/// A bar at index `i` is a swing high when it has at least `left` bars before it
/// and `right` bars after it, and its `high_fp` is *strictly* greater than the
/// `high_fp` of every one of those neighbours. Returned indices are ascending.
#[must_use]
pub fn swing_highs(bars: &[Bar], left: usize, right: usize) -> Vec<usize> {
    pivots(bars, left, right, |a, b| a.high_fp > b.high_fp)
}

/// Indices of swing-low pivots in `bars` (constitution 21.6 swing structure).
///
/// A bar at index `i` is a swing low when it has at least `left` bars before and
/// `right` after, and its `low_fp` is *strictly* less than every neighbour's
/// `low_fp`. Returned indices are ascending.
#[must_use]
pub fn swing_lows(bars: &[Bar], left: usize, right: usize) -> Vec<usize> {
    pivots(bars, left, right, |a, b| a.low_fp < b.low_fp)
}

/// Shared pivot scan: `dominates(pivot, neighbour)` must hold against every bar in
/// the `left`/`right` neighbourhood for the index to qualify.
fn pivots(
    bars: &[Bar],
    left: usize,
    right: usize,
    dominates: impl Fn(&Bar, &Bar) -> bool,
) -> Vec<usize> {
    let mut out = Vec::new();
    let n = bars.len();
    for i in 0..n {
        if i < left || i + right >= n {
            continue;
        }
        let p = &bars[i];
        let lo = i - left;
        let hi = i + right;
        let ok = (lo..i).chain(i + 1..=hi).all(|j| dominates(p, &bars[j]));
        if ok {
            out.push(i);
        }
    }
    out
}

/// Swing-structure trend classification (constitution 21.6 higher-high/lower-low).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrendStructure {
    /// Higher high and higher low: the last two swing highs and lows both rose.
    Uptrend,
    /// Lower high and lower low: the last two swing highs and lows both fell.
    Downtrend,
    /// Mixed / equal swings that satisfy neither the up nor the down pattern.
    Range,
    /// Fewer than two swing highs or two swing lows exist — undefined.
    Undefined,
}

/// Classify trend structure from the last two swing highs and last two swing lows
/// (constitution 21.6). Uptrend requires a higher high *and* a higher low;
/// downtrend a lower high *and* a lower low; otherwise `Range`. With fewer than two
/// pivots of either kind the structure is `Undefined`.
#[must_use]
pub fn swing_structure(bars: &[Bar], left: usize, right: usize) -> TrendStructure {
    let highs = swing_highs(bars, left, right);
    let lows = swing_lows(bars, left, right);
    if highs.len() < 2 || lows.len() < 2 {
        return TrendStructure::Undefined;
    }
    let hn = highs.len();
    let ln = lows.len();
    let prev_high = bars[highs[hn - 2]].high_fp;
    let last_high = bars[highs[hn - 1]].high_fp;
    let prev_low = bars[lows[ln - 2]].low_fp;
    let last_low = bars[lows[ln - 1]].low_fp;

    let higher_high = last_high > prev_high;
    let higher_low = last_low > prev_low;
    let lower_high = last_high < prev_high;
    let lower_low = last_low < prev_low;

    if higher_high && higher_low {
        TrendStructure::Uptrend
    } else if lower_high && lower_low {
        TrendStructure::Downtrend
    } else {
        TrendStructure::Range
    }
}

/// Breakout-and-retest state of `action` bars against a resistance `level_fp`
/// (constitution 21.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakoutState {
    /// No action bar closed strictly above the level.
    None,
    /// Price closed above the level but has not (yet) retested it.
    Broken,
    /// Price broke out, retested the level (a bar dipped to/through it) and held —
    /// the bar that retested still closed at or above the level.
    RetestHeld,
    /// Price broke out but a later bar closed back below the level (breakout failed).
    Failed,
}

/// Detect breakout-and-retest structure over `action` bars against `level_fp`
/// (constitution 21.6). `level_fp` is typically [`highest_high_fp`] of the leading
/// reference bars.
///
/// The scan is left-to-right and terminal-priority: the first bar closing strictly
/// above the level arms `Broken`; after that a bar closing strictly below the level
/// makes the result `Failed` immediately, whereas a bar whose `low_fp <= level_fp`
/// (retested) but whose `close_fp >= level_fp` (held) upgrades to `RetestHeld`. A
/// `Failed` outcome dominates — an invalidated breakout is reported even if an
/// earlier retest held.
#[must_use]
pub fn breakout_retest_state(action: &[Bar], level_fp: i128) -> BreakoutState {
    let mut broke = false;
    let mut retested = false;
    for b in action {
        if !broke {
            if b.close_fp > level_fp {
                broke = true;
            }
            continue;
        }
        // Already broken out: check for invalidation or a held retest.
        if b.close_fp < level_fp {
            return BreakoutState::Failed;
        }
        if b.low_fp <= level_fp && b.close_fp >= level_fp {
            retested = true;
        }
    }
    if !broke {
        BreakoutState::None
    } else if retested {
        BreakoutState::RetestHeld
    } else {
        BreakoutState::Broken
    }
}

/// Failed-breakdown / reclaim state of `action` bars against a support `level_fp`
/// (constitution 21.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakdownState {
    /// No action bar's `low_fp` pierced below the level.
    None,
    /// A bar pierced below the level and the final action bar still closed below it
    /// — a genuine breakdown, not reclaimed.
    Broken,
    /// A bar pierced below the level but the final action bar closed back at or
    /// above it — a failed breakdown / reclaim (bullish trap).
    Reclaimed,
}

/// Detect failed-breakdown/reclaim structure over `action` bars against `level_fp`
/// (constitution 21.6). `level_fp` is typically [`lowest_low_fp`] of the leading
/// reference bars.
///
/// A breakdown is *attempted* when some bar's `low_fp` pierces strictly below the
/// level. Whether it *failed* is decided by the final action bar's close: if the
/// last bar closes at or above the level after a pierce, the breakdown was
/// reclaimed; if it closes below, the breakdown stands. No pierce at all is `None`.
#[must_use]
pub fn failed_breakdown_state(action: &[Bar], level_fp: i128) -> BreakdownState {
    let pierced = action.iter().any(|b| b.low_fp < level_fp);
    if !pierced {
        return BreakdownState::None;
    }
    match action.last() {
        Some(last) if last.close_fp >= level_fp => BreakdownState::Reclaimed,
        _ => BreakdownState::Broken,
    }
}

/// True when `bar` swept liquidity below `level_fp` and reclaimed it within the same
/// bar: `low_fp < level_fp` (stops run) but `close_fp > level_fp` (constitution 21.6
/// sweep-and-reclaim of a support level).
#[must_use]
pub fn is_bullish_sweep(bar: &Bar, level_fp: i128) -> bool {
    bar.low_fp < level_fp && bar.close_fp > level_fp
}

/// True when `bar` swept liquidity above `level_fp` and was rejected within the same
/// bar: `high_fp > level_fp` but `close_fp < level_fp` (constitution 21.6
/// sweep-and-reject of a resistance level).
#[must_use]
pub fn is_bearish_sweep(bar: &Bar, level_fp: i128) -> bool {
    bar.high_fp > level_fp && bar.close_fp < level_fp
}

/// Kind of single-bar liquidity sweep detected (constitution 21.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepKind {
    /// Swept below support and reclaimed it (bullish stop run).
    SweptLowReclaimed,
    /// Swept above resistance and was rejected (bearish stop run).
    SweptHighRejected,
}

/// Scan `action` bars for single-bar sweep-and-reclaim structure against a support
/// `low_level_fp` and resistance `high_level_fp` (constitution 21.6). Returns each
/// hit as `(index_in_action, kind)` in ascending index order. A bar that is both a
/// low reclaim and a high rejection (which requires `low < low_level` and
/// `close > low_level` and `high > high_level` and `close < high_level`, hence
/// `low_level > high_level`) reports the low reclaim first.
#[must_use]
pub fn sweep_scan(
    action: &[Bar],
    low_level_fp: i128,
    high_level_fp: i128,
) -> Vec<(usize, SweepKind)> {
    let mut out = Vec::new();
    for (i, b) in action.iter().enumerate() {
        if is_bullish_sweep(b, low_level_fp) {
            out.push((i, SweepKind::SweptLowReclaimed));
        }
        if is_bearish_sweep(b, high_level_fp) {
            out.push((i, SweepKind::SweptHighRejected));
        }
    }
    out
}

/// The full §21.6 market-structure feature bundle computed over one bar sequence.
///
/// The sequence is split at `ref_len`: `bars[..ref_len]` establish the reference
/// range (its highest high and lowest low), and `bars[ref_len..]` are the *action*
/// bars whose price traces those levels. Every field is a deterministic detector
/// output over that split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureState {
    /// Resistance level = highest high of the reference bars.
    pub ref_high_fp: i128,
    /// Support level = lowest low of the reference bars.
    pub ref_low_fp: i128,
    /// Breakout-and-retest state of the action bars vs `ref_high_fp`.
    pub breakout: BreakoutState,
    /// Failed-breakdown/reclaim state of the action bars vs `ref_low_fp`.
    pub breakdown: BreakdownState,
    /// Single-bar sweeps found in the action bars (indices are action-relative).
    pub sweeps: Vec<(usize, SweepKind)>,
    /// Swing-trend structure over the whole sequence.
    pub trend: TrendStructure,
}

/// Compute the full market-structure bundle for `bars`, splitting at `ref_len`
/// reference bars (constitution 21.6). `swing_left`/`swing_right` parametrise the
/// pivot neighbourhood used for the swing-trend classification.
///
/// Returns `None` when `ref_len == 0`, when `ref_len >= bars.len()` (no action bars
/// remain), so the reference range and at least one action bar are both defined.
#[must_use]
pub fn classify_structure(
    bars: &[Bar],
    ref_len: usize,
    swing_left: usize,
    swing_right: usize,
) -> Option<StructureState> {
    if ref_len == 0 || ref_len >= bars.len() {
        return None;
    }
    let reference = &bars[..ref_len];
    let action = &bars[ref_len..];
    let ref_high_fp = highest_high_fp(reference)?;
    let ref_low_fp = lowest_low_fp(reference)?;
    Some(StructureState {
        ref_high_fp,
        ref_low_fp,
        breakout: breakout_retest_state(action, ref_high_fp),
        breakdown: failed_breakdown_state(action, ref_low_fp),
        sweeps: sweep_scan(action, ref_low_fp, ref_high_fp),
        trend: swing_structure(bars, swing_left, swing_right),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EventId;

    /// Build a bar from OHLC (fixed-point) with dummy volumes/provenance. Only the
    /// price fields drive market-structure logic.
    fn bar(open: i128, high: i128, low: i128, close: i128) -> Bar {
        Bar {
            open_time_ns: 0,
            close_time_ns: 0,
            open_fp: open,
            high_fp: high,
            low_fp: low,
            close_fp: close,
            base_volume: 1,
            quote_volume: 1,
            buy_base_volume: 1,
            sell_base_volume: 0,
            trade_count: 1,
            first_event_id: 0 as EventId,
            last_event_id: 0 as EventId,
        }
    }

    #[test]
    fn range_and_extremes() {
        let b = bar(100, 130, 90, 110);
        assert_eq!(bar_range_fp(&b), 40);
        let bars = [bar(100, 130, 90, 110), bar(110, 140, 105, 120)];
        assert_eq!(highest_high_fp(&bars), Some(140));
        assert_eq!(lowest_low_fp(&bars), Some(90));
        assert_eq!(highest_high_fp(&[]), None);
        assert_eq!(lowest_low_fp(&[]), None);
    }

    #[test]
    fn compression_expansion_and_neutral() {
        // Baseline ranges 100 each, recent ranges 20 each -> ratio 20% -> compression.
        let bars = vec![
            bar(0, 100, 0, 50),
            bar(0, 100, 0, 50),
            bar(0, 20, 0, 10),
            bar(0, 20, 0, 10),
        ];
        assert_eq!(
            range_state(&bars, 2, 2, 6_000, 15_000),
            Some(RangeState::Compression)
        );

        // Recent ranges 200 vs baseline 100 -> 200% -> expansion.
        let bars2 = vec![
            bar(0, 100, 0, 50),
            bar(0, 100, 0, 50),
            bar(0, 200, 0, 100),
            bar(0, 200, 0, 100),
        ];
        assert_eq!(
            range_state(&bars2, 2, 2, 6_000, 15_000),
            Some(RangeState::Expansion)
        );

        // Recent == baseline -> neutral.
        let bars3 = vec![
            bar(0, 100, 0, 50),
            bar(0, 100, 0, 50),
            bar(0, 100, 0, 50),
            bar(0, 100, 0, 50),
        ];
        assert_eq!(
            range_state(&bars3, 2, 2, 6_000, 15_000),
            Some(RangeState::Neutral)
        );
    }

    #[test]
    fn range_state_rejections() {
        let bars = vec![bar(0, 100, 0, 50)];
        // Not enough bars.
        assert_eq!(range_state(&bars, 2, 2, 6_000, 15_000), None);
        // Zero window.
        let bars4 = vec![bar(0, 100, 0, 50); 4];
        assert_eq!(range_state(&bars4, 0, 2, 6_000, 15_000), None);
        // Zero baseline range -> undefined ratio.
        let flat = vec![
            bar(50, 50, 50, 50),
            bar(50, 50, 50, 50),
            bar(0, 20, 0, 10),
            bar(0, 20, 0, 10),
        ];
        assert_eq!(range_state(&flat, 2, 2, 6_000, 15_000), None);
    }

    #[test]
    fn swing_pivots_and_trend() {
        // Highs: 10 20 15 30 25 -> index 1 (20>10,20>15) and index 3 (30>15,30>25).
        // Lows:  mirror to force higher lows for an uptrend.
        let bars = vec![
            bar(0, 10, 1, 5),
            bar(0, 20, 3, 5),
            bar(0, 15, 2, 5),
            bar(0, 30, 8, 5),
            bar(0, 25, 6, 5),
        ];
        assert_eq!(swing_highs(&bars, 1, 1), vec![1, 3]);
        // Swing lows (strictly less than neighbours): index 2 (low 2 < 3 and < 8).
        assert_eq!(swing_lows(&bars, 1, 1), vec![2]);
        // Only one swing low -> Undefined.
        assert_eq!(swing_structure(&bars, 1, 1), TrendStructure::Undefined);
    }

    #[test]
    fn uptrend_and_downtrend_structure() {
        // Clean HH/HL zig-zag: highs 15 then 20 (higher high), lows 4 then 6
        // (higher low). Pivots computed with left=right=1.
        let up = vec![
            bar(0, 8, 3, 5),    // 0
            bar(0, 15, 5, 12),  // 1 swing high (15 > 8, 9)
            bar(0, 9, 4, 6),    // 2 swing low  (4 < 5, 10)
            bar(0, 20, 10, 18), // 3 swing high (20 > 9, 12)
            bar(0, 12, 6, 9),   // 4 swing low  (6 < 10, 9)
            bar(0, 14, 9, 11),  // 5
        ];
        assert_eq!(swing_highs(&up, 1, 1), vec![1, 3]);
        assert_eq!(swing_lows(&up, 1, 1), vec![2, 4]);
        assert_eq!(swing_structure(&up, 1, 1), TrendStructure::Uptrend);

        // Now a downtrend: LH and LL with two pivots each.
        let down = vec![
            bar(0, 30, 20, 25), // 0
            bar(0, 35, 22, 28), // 1 swing high (35>30,25)
            bar(0, 24, 15, 20), // 2 swing low (15<22,18)
            bar(0, 28, 18, 22), // 3 swing high (28>24,20)
            bar(0, 20, 10, 12), // 4 swing low (10<18,?) needs right neighbour
            bar(0, 22, 14, 16), // 5
        ];
        let dh = swing_highs(&down, 1, 1);
        let dl = swing_lows(&down, 1, 1);
        assert_eq!(dh, vec![1, 3]); // 35 then 28 -> lower high
        assert_eq!(dl, vec![2, 4]); // 15 then 10 -> lower low
        assert_eq!(swing_structure(&down, 1, 1), TrendStructure::Downtrend);
    }

    #[test]
    fn breakout_states() {
        let level = 100;
        // None: never closes above.
        let none = [bar(90, 95, 85, 92), bar(92, 99, 90, 95)];
        assert_eq!(breakout_retest_state(&none, level), BreakoutState::None);

        // Broken only: closes above, no retest touch.
        let broke = [bar(95, 105, 94, 104), bar(104, 110, 103, 108)];
        assert_eq!(breakout_retest_state(&broke, level), BreakoutState::Broken);

        // RetestHeld: break out, then dip to level (low<=100) closing back above.
        let held = [
            bar(95, 106, 94, 104),  // breakout close 104 > 100
            bar(104, 105, 98, 101), // low 98 <= 100, close 101 >= 100 -> retest held
        ];
        assert_eq!(
            breakout_retest_state(&held, level),
            BreakoutState::RetestHeld
        );

        // Failed: break out then close below.
        let failed = [
            bar(95, 106, 94, 104), // breakout
            bar(104, 105, 90, 95), // close 95 < 100 -> failed
        ];
        assert_eq!(breakout_retest_state(&failed, level), BreakoutState::Failed);

        // Failed dominates an earlier held retest.
        let held_then_fail = [
            bar(95, 106, 94, 104),  // breakout
            bar(104, 105, 98, 101), // retest held
            bar(101, 102, 88, 90),  // close below -> failed
        ];
        assert_eq!(
            breakout_retest_state(&held_then_fail, level),
            BreakoutState::Failed
        );
    }

    #[test]
    fn breakdown_states() {
        let level = 100;
        // None: low never pierces below.
        let none = [bar(105, 110, 101, 108)];
        assert_eq!(failed_breakdown_state(&none, level), BreakdownState::None);

        // Broken: pierce and final close below.
        let broke = [bar(105, 106, 95, 98), bar(98, 99, 90, 92)];
        assert_eq!(
            failed_breakdown_state(&broke, level),
            BreakdownState::Broken
        );

        // Reclaimed: pierce then final close back above.
        let reclaim = [
            bar(105, 106, 94, 97), // pierce low 94 < 100, close 97 below
            bar(97, 108, 96, 103), // final close 103 >= 100 -> reclaim
        ];
        assert_eq!(
            failed_breakdown_state(&reclaim, level),
            BreakdownState::Reclaimed
        );
    }

    #[test]
    fn sweep_detection() {
        // Bullish sweep: low pierces support, closes above.
        let sup = 100;
        let res = 200;
        let bull = bar(101, 105, 95, 103);
        assert!(is_bullish_sweep(&bull, sup));
        assert!(!is_bearish_sweep(&bull, res));

        // Bearish sweep: high pierces resistance, closes below.
        let bear = bar(198, 210, 195, 197);
        assert!(is_bearish_sweep(&bear, res));
        assert!(!is_bullish_sweep(&bear, sup));

        let action = [
            bar(101, 105, 95, 103),  // idx0 bullish sweep of support
            bar(150, 160, 145, 155), // idx1 nothing
            bar(198, 210, 195, 197), // idx2 bearish sweep of resistance
        ];
        assert_eq!(
            sweep_scan(&action, sup, res),
            vec![
                (0, SweepKind::SweptLowReclaimed),
                (2, SweepKind::SweptHighRejected)
            ]
        );
    }

    #[test]
    fn classify_bundle() {
        // Reference bars 0..2 establish range high 130, low 90.
        // Action bars break out above 130, retest, and hold.
        let bars = vec![
            bar(100, 120, 90, 110),  // ref
            bar(110, 130, 100, 125), // ref -> high 130
            bar(125, 145, 124, 140), // action: breakout close 140 > 130
            bar(140, 141, 128, 132), // action: retest low 128 <= 130, close 132 >= 130
        ];
        let s = classify_structure(&bars, 2, 1, 1).unwrap();
        assert_eq!(s.ref_high_fp, 130);
        assert_eq!(s.ref_low_fp, 90);
        assert_eq!(s.breakout, BreakoutState::RetestHeld);
        assert_eq!(s.breakdown, BreakdownState::None);
        assert!(s.sweeps.is_empty());

        // Rejections.
        assert!(classify_structure(&bars, 0, 1, 1).is_none());
        assert!(classify_structure(&bars, 4, 1, 1).is_none());
    }
}
