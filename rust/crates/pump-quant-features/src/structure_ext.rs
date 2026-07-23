//! Extended bar / market-structure detector family (constitution 21.6).
//!
//! Responsibility: complete the §21.6 "bar and market-structure feature family"
//! with the members that [`crate::market_structure`] does not yet cover —
//! drawdown/retrace state, a *graded* volatility regime (with an explicit high
//! tail, distinct from that module's squeeze-only [`RangeState`]), per-bar
//! wick/rejection microstructure, and info-time conditioning (token age and
//! time-of-day). It also exposes a [`realized_vol_bps`] helper the execution
//! engine consumes in Wave 2 to scale stops.
//!
//! This module is *additive*: it never touches [`crate::market_structure`]
//! (dossier-adjacent). Every function is pure and deterministic over the input it
//! is given (constitution 20 point-in-time / 57 / 99): detectors read only the
//! bars at or before the decision index the caller passes, and the time helpers
//! take caller-supplied *information-time* nanoseconds — there is no wall clock,
//! no RNG, and no I/O anywhere here. All arithmetic is integer / fixed-point
//! [`i128`]/[`u64`] with explicit saturating overflow contracts (constitution 22);
//! no floating point appears in any outcome-controlling path. Every threshold is a
//! named const with a citation (constitution 102), surfaced through a small params
//! struct so callers tune behaviour without magic numbers.
//!
//! These are *research-gated structural features*, not assumed-predictive signals:
//! this module computes them faithfully; admission lives in other planes.

use crate::bar::Bar;

/// Basis-point scale: `10_000 bp == 100%` (constitution 22 fixed-point ratios).
/// Ratios are carried as integer basis points so no floating point is required.
pub const BPS_SCALE: i128 = 10_000;

/// Nanoseconds in one 24-hour day: `86_400 * 1_000_000_000` (constitution 102).
/// Used only for *information-time* modulo arithmetic in [`time_of_day_bucket`] —
/// this is a fold of the caller-supplied info-time, never a wall-clock read.
pub const NS_PER_DAY: u64 = 86_400 * 1_000_000_000;

// ---------------------------------------------------------------------------
// 1. Drawdown / retrace state
// ---------------------------------------------------------------------------

/// Default lower edge of the "golden" retrace pocket, in basis points of the
/// prior swing (constitution 102): `3_820 bp == 38.2%`, the 0.382 Fibonacci level.
pub const RETRACE_GOLDEN_LO_BPS: i128 = 3_820;

/// Default upper edge of the "golden" retrace pocket, in basis points of the prior
/// swing (constitution 102): `6_180 bp == 61.8%`, the 0.618 Fibonacci level.
pub const RETRACE_GOLDEN_HI_BPS: i128 = 6_180;

/// Default full-reversal edge, in basis points of the prior swing (constitution
/// 102): `10_000 bp == 100%` — the pullback erased the entire swing.
pub const RETRACE_FULL_BPS: i128 = 10_000;

/// Threshold parameters for [`retrace_state`] (constitution 102: named, tunable,
/// no magic numbers). All values are basis points of the prior swing; sane use
/// requires `golden_lo_bps <= golden_hi_bps <= full_bps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetraceParams {
    /// Below this, the pullback is [`RetraceState::Shallow`].
    pub golden_lo_bps: i128,
    /// Inclusive upper edge of the [`RetraceState::Golden`] pocket.
    pub golden_hi_bps: i128,
    /// At or above this the pullback is a [`RetraceState::FullReversal`]; between
    /// `golden_hi_bps` (exclusive) and here it is [`RetraceState::Deep`].
    pub full_bps: i128,
}

impl Default for RetraceParams {
    fn default() -> Self {
        Self {
            golden_lo_bps: RETRACE_GOLDEN_LO_BPS,
            golden_hi_bps: RETRACE_GOLDEN_HI_BPS,
            full_bps: RETRACE_FULL_BPS,
        }
    }
}

/// Classified depth of a peak-to-trough pullback (constitution 21.6 drawdown/retrace).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetraceState {
    /// Pullback is shallower than the golden pocket (strong trend continuation).
    Shallow,
    /// Pullback sits inside the golden pocket (the classic continuation zone).
    Golden,
    /// Pullback is past the golden pocket but has not erased the swing.
    Deep,
    /// Pullback met or exceeded the whole swing — a full reversal.
    FullReversal,
}

/// Peak-to-trough retrace over `bars`, in basis points of the prior up-swing
/// (constitution 21.6). Returns `None` when it is undefined.
///
/// Construction (all point-in-time over the slice): `peak_idx` is the index of the
/// first bar carrying the maximum `high_fp`; `swing_low` is the minimum `low_fp`
/// over `bars[..=peak_idx]` (the run-up base); `trough` is the minimum `low_fp`
/// over `bars[peak_idx..]` (the pullback low). With `swing = peak_high -
/// swing_low` and `retrace = peak_high - trough`, the result is
/// `retrace * BPS_SCALE / swing` (a single integer division; `retrace >= 0` always
/// because `peak_high` is the global max high). `retrace` can exceed `swing`, so
/// the result can exceed `10_000 bp` on a full reversal.
///
/// `None` when `bars.len() < 2` (a single bar has no prior swing) or when
/// `swing <= 0` (a flat / all-equal slice has no swing to retrace).
#[must_use]
pub fn retrace_bps(bars: &[Bar]) -> Option<i128> {
    if bars.len() < 2 {
        return None;
    }
    let mut peak_idx = 0usize;
    let mut peak_high = bars[0].high_fp;
    for (i, b) in bars.iter().enumerate() {
        if b.high_fp > peak_high {
            peak_high = b.high_fp;
            peak_idx = i;
        }
    }
    let swing_low = bars[..=peak_idx]
        .iter()
        .map(|b| b.low_fp)
        .min()
        .unwrap_or(peak_high);
    let trough = bars[peak_idx..]
        .iter()
        .map(|b| b.low_fp)
        .min()
        .unwrap_or(peak_high);
    let swing = peak_high.saturating_sub(swing_low);
    if swing <= 0 {
        return None;
    }
    let retrace = peak_high.saturating_sub(trough);
    Some(retrace.saturating_mul(BPS_SCALE) / swing)
}

/// Classify the peak-to-trough pullback over `bars` into a [`RetraceState`] using
/// `params` thresholds (constitution 21.6). Returns `None` exactly when
/// [`retrace_bps`] does. Boundaries are exact: a value equal to `golden_lo_bps`
/// is [`RetraceState::Golden`], one equal to `golden_hi_bps` is still `Golden`,
/// and one equal to `full_bps` is [`RetraceState::FullReversal`].
#[must_use]
pub fn retrace_state(bars: &[Bar], params: &RetraceParams) -> Option<RetraceState> {
    let bps = retrace_bps(bars)?;
    let state = if bps < params.golden_lo_bps {
        RetraceState::Shallow
    } else if bps <= params.golden_hi_bps {
        RetraceState::Golden
    } else if bps < params.full_bps {
        RetraceState::Deep
    } else {
        RetraceState::FullReversal
    };
    Some(state)
}

// ---------------------------------------------------------------------------
// 2. Volatility regime (graded, with high tail)
// ---------------------------------------------------------------------------

/// Default upper edge of the compressed regime, in basis points of the baseline
/// mean range (constitution 102): `7_000 bp == 70%`.
pub const VOL_COMPRESSED_BPS: i128 = 7_000;

/// Default lower edge of the expanded regime, in basis points of the baseline mean
/// range (constitution 102): `15_000 bp == 150%`.
pub const VOL_EXPANDED_BPS: i128 = 15_000;

/// Default lower edge of the explosive (high-tail) regime, in basis points of the
/// baseline mean range (constitution 102): `30_000 bp == 300%`.
pub const VOL_EXPLOSIVE_BPS: i128 = 30_000;

/// Threshold parameters for [`vol_regime`] (constitution 102). All values are
/// basis points of the baseline mean range; sane use requires
/// `compressed_bps < expanded_bps < explosive_bps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolRegimeParams {
    /// At or below this ratio the regime is [`VolRegime::Compressed`].
    pub compressed_bps: i128,
    /// At or above this ratio (but below `explosive_bps`) the regime is
    /// [`VolRegime::Expanded`].
    pub expanded_bps: i128,
    /// At or above this ratio the regime is [`VolRegime::Explosive`] (high tail).
    pub explosive_bps: i128,
}

impl Default for VolRegimeParams {
    fn default() -> Self {
        Self {
            compressed_bps: VOL_COMPRESSED_BPS,
            expanded_bps: VOL_EXPANDED_BPS,
            explosive_bps: VOL_EXPLOSIVE_BPS,
        }
    }
}

/// Graded volatility regime from bar-range dispersion (constitution 21.6).
///
/// Distinct from [`crate::market_structure::RangeState`], which is a squeeze-only
/// compression/expansion/neutral triple: this is a four-way graded regime with an
/// explicit high tail ([`VolRegime::Explosive`]) for the violent-expansion state
/// memecoin risk sizing must treat separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolRegime {
    /// Recent mean range is contracted vs the baseline.
    Compressed,
    /// Recent mean range is in the ordinary band around the baseline.
    Normal,
    /// Recent mean range is materially wider than the baseline.
    Expanded,
    /// Recent mean range is in the violent high tail vs the baseline.
    Explosive,
}

/// Sum of `high_fp - low_fp` over `slice`, saturating (constitution 22). Private
/// helper shared by the volatility-regime computations.
fn range_sum(slice: &[Bar]) -> i128 {
    slice.iter().fold(0i128, |acc, b| {
        acc.saturating_add(b.high_fp.saturating_sub(b.low_fp))
    })
}

/// Ratio of the recent-window mean bar range to the baseline-window mean bar range,
/// in basis points (constitution 21.6). The recent window is the last `recent`
/// bars; the baseline window is the `baseline` bars immediately preceding them.
///
/// Computed as `recent_sum * baseline_n * BPS_SCALE / (baseline_sum * recent_n)`
/// (a single integer division). Returns `None` when either window length is zero,
/// when `bars.len() < recent + baseline`, or when the baseline range sum is zero
/// (ratio undefined). Exposed for downstream conditioning; [`vol_regime`] itself
/// classifies without this intermediate truncation.
#[must_use]
pub fn mean_range_ratio_bps(bars: &[Bar], recent: usize, baseline: usize) -> Option<i128> {
    if recent == 0 || baseline == 0 || bars.len() < recent + baseline {
        return None;
    }
    let n = bars.len();
    let recent_sum = range_sum(&bars[n - recent..]);
    let baseline_sum = range_sum(&bars[n - recent - baseline..n - recent]);
    if baseline_sum <= 0 {
        return None;
    }
    let num = recent_sum
        .saturating_mul(baseline as i128)
        .saturating_mul(BPS_SCALE);
    let den = baseline_sum.saturating_mul(recent as i128);
    Some(num / den)
}

/// Classify the graded volatility regime by comparing the mean bar range over the
/// last `recent` bars against the mean over the `baseline` bars immediately before
/// them (constitution 21.6). Uses cross-multiplication so every boundary is exact
/// integer math with no division rounding.
///
/// Returns `None` under the same conditions as [`mean_range_ratio_bps`] (zero
/// window, too few bars, or zero baseline range). Boundaries are inclusive on the
/// tail side: a ratio exactly equal to `compressed_bps` is
/// [`VolRegime::Compressed`], one exactly equal to `expanded_bps` is
/// [`VolRegime::Expanded`], and one exactly equal to `explosive_bps` is
/// [`VolRegime::Explosive`].
#[must_use]
pub fn vol_regime(
    bars: &[Bar],
    recent: usize,
    baseline: usize,
    params: &VolRegimeParams,
) -> Option<VolRegime> {
    if recent == 0 || baseline == 0 || bars.len() < recent + baseline {
        return None;
    }
    let n = bars.len();
    let recent_sum = range_sum(&bars[n - recent..]);
    let baseline_sum = range_sum(&bars[n - recent - baseline..n - recent]);
    if baseline_sum <= 0 {
        return None;
    }
    // lhs = recent_mean * BPS_SCALE, base = baseline_mean, over the common
    // denominator recent_n * baseline_n; compare numerators directly.
    let lhs = recent_sum
        .saturating_mul(baseline as i128)
        .saturating_mul(BPS_SCALE);
    let base = baseline_sum.saturating_mul(recent as i128);
    let regime = if lhs >= base.saturating_mul(params.explosive_bps) {
        VolRegime::Explosive
    } else if lhs >= base.saturating_mul(params.expanded_bps) {
        VolRegime::Expanded
    } else if lhs <= base.saturating_mul(params.compressed_bps) {
        VolRegime::Compressed
    } else {
        VolRegime::Normal
    };
    Some(regime)
}

// ---------------------------------------------------------------------------
// 3. Wick / rejection microstructure
// ---------------------------------------------------------------------------

/// Default maximum body ratio for a [`WickShape::Doji`], in basis points of the
/// bar range (constitution 102): `1_000 bp == 10%`.
pub const WICK_DOJI_BODY_BPS: i128 = 1_000;

/// Default minimum body ratio for a [`WickShape::Marubozu`], in basis points of the
/// bar range (constitution 102): `8_000 bp == 80%`.
pub const WICK_MARUBOZU_BODY_BPS: i128 = 8_000;

/// Default minimum wick ratio for a rejection classification, in basis points of the
/// bar range (constitution 102): `4_000 bp == 40%`.
pub const WICK_REJECTION_BPS: i128 = 4_000;

/// Threshold parameters for [`wick_shape`] (constitution 102). All values are basis
/// points of the bar range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WickParams {
    /// Body at or below this ratio classifies the bar as [`WickShape::Doji`].
    pub doji_body_bps: i128,
    /// Body at or above this ratio classifies the bar as [`WickShape::Marubozu`].
    pub marubozu_body_bps: i128,
    /// Dominant wick at or above this ratio classifies a rejection.
    pub rejection_wick_bps: i128,
}

impl Default for WickParams {
    fn default() -> Self {
        Self {
            doji_body_bps: WICK_DOJI_BODY_BPS,
            marubozu_body_bps: WICK_MARUBOZU_BODY_BPS,
            rejection_wick_bps: WICK_REJECTION_BPS,
        }
    }
}

/// Decomposition of one bar into wick and body ratios (constitution 21.6). Each
/// field is basis points of the bar range; the three sum to `BPS_SCALE` up to
/// integer-division truncation, since `upper_wick + lower_wick + body == range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WickMetrics {
    /// Upper wick (`high - max(open, close)`) as bp of range.
    pub upper_wick_bps: i128,
    /// Lower wick (`min(open, close) - low`) as bp of range.
    pub lower_wick_bps: i128,
    /// Body (`|close - open|`) as bp of range.
    pub body_bps: i128,
}

/// Single-bar candle shape classification (constitution 21.6 wick microstructure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WickShape {
    /// Body is tiny relative to range (indecision).
    Doji,
    /// Dominant upper wick — sellers rejected higher prices.
    UpperRejection,
    /// Dominant lower wick — buyers rejected lower prices.
    LowerRejection,
    /// Body dominates with negligible wicks (one-sided drive).
    Marubozu,
    /// None of the above thresholds is met.
    Neutral,
}

/// Absolute difference of two `i128` values with a saturating contract
/// (constitution 22), avoiding the `i128::MIN.abs()` panic edge.
fn abs_diff(a: i128, b: i128) -> i128 {
    if a >= b {
        a.saturating_sub(b)
    } else {
        b.saturating_sub(a)
    }
}

/// Decompose `bar` into wick/body ratios (constitution 21.6). Returns `None` when
/// the bar range is zero (`high_fp == low_fp`): the ratios are undefined and a
/// degenerate flat bar has no wick structure. Never panics.
#[must_use]
pub fn wick_metrics(bar: &Bar) -> Option<WickMetrics> {
    let range = bar.high_fp.saturating_sub(bar.low_fp);
    if range <= 0 {
        return None;
    }
    let body_top = bar.open_fp.max(bar.close_fp);
    let body_bot = bar.open_fp.min(bar.close_fp);
    let upper = bar.high_fp.saturating_sub(body_top);
    let lower = body_bot.saturating_sub(bar.low_fp);
    let body = abs_diff(bar.close_fp, bar.open_fp);
    Some(WickMetrics {
        upper_wick_bps: upper.saturating_mul(BPS_SCALE) / range,
        lower_wick_bps: lower.saturating_mul(BPS_SCALE) / range,
        body_bps: body.saturating_mul(BPS_SCALE) / range,
    })
}

/// Classify the candle shape of `bar` under `params` (constitution 21.6). A
/// zero-range bar has no structure and is reported as [`WickShape::Doji`] (a body
/// of zero is the limiting doji), so this never panics.
///
/// Priority: doji (small body) first, then marubozu (large body), then a wick
/// rejection whose dominant wick meets `rejection_wick_bps` and strictly exceeds
/// the opposite wick, otherwise [`WickShape::Neutral`].
#[must_use]
pub fn wick_shape(bar: &Bar, params: &WickParams) -> WickShape {
    match wick_metrics(bar) {
        None => WickShape::Doji,
        Some(m) => {
            if m.body_bps <= params.doji_body_bps {
                WickShape::Doji
            } else if m.body_bps >= params.marubozu_body_bps {
                WickShape::Marubozu
            } else if m.upper_wick_bps >= params.rejection_wick_bps
                && m.upper_wick_bps > m.lower_wick_bps
            {
                WickShape::UpperRejection
            } else if m.lower_wick_bps >= params.rejection_wick_bps
                && m.lower_wick_bps > m.upper_wick_bps
            {
                WickShape::LowerRejection
            } else {
                WickShape::Neutral
            }
        }
    }
}

/// Aggregate upper-wick ("sell-wick") pressure over `bars`, in basis points of
/// total range (constitution 21.6): the fraction of cumulative bar range that is
/// upper wick, i.e. `sum(upper_wick) * BPS_SCALE / sum(range)`.
///
/// A high value means price repeatedly probed higher and was sold back down — a
/// distribution / supply signature. Bars with zero range contribute nothing.
/// Returns `None` when `bars` is empty or every bar has zero range (denominator
/// zero). Single integer division; never panics.
#[must_use]
pub fn sell_wick_pressure_bps(bars: &[Bar]) -> Option<i128> {
    let mut upper_sum = 0i128;
    let mut range_sum = 0i128;
    for b in bars {
        let range = b.high_fp.saturating_sub(b.low_fp);
        if range <= 0 {
            continue;
        }
        let body_top = b.open_fp.max(b.close_fp);
        let upper = b.high_fp.saturating_sub(body_top);
        upper_sum = upper_sum.saturating_add(upper);
        range_sum = range_sum.saturating_add(range);
    }
    if range_sum <= 0 {
        return None;
    }
    Some(upper_sum.saturating_mul(BPS_SCALE) / range_sum)
}

// ---------------------------------------------------------------------------
// 4. Info-time conditioning: token age & time-of-day
// ---------------------------------------------------------------------------

/// Default upper edge (exclusive) of the newborn age bucket, in seconds
/// (constitution 102): `300 s == 5 min`.
pub const AGE_NEWBORN_MAX_SECS: u64 = 300;

/// Default upper edge (exclusive) of the young age bucket, in seconds
/// (constitution 102): `3_600 s == 1 h`.
pub const AGE_YOUNG_MAX_SECS: u64 = 3_600;

/// Default upper edge (exclusive) of the mature age bucket, in seconds
/// (constitution 102): `86_400 s == 24 h`; at or beyond it the token is old.
pub const AGE_MATURE_MAX_SECS: u64 = 86_400;

/// Coarse token-lifecycle bucket (constitution 21.6 token-age conditioning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAgeBucket {
    /// Below `newborn_max_units` (fresh launch — highest reflexivity).
    Newborn,
    /// Below `young_max_units`.
    Young,
    /// Below `mature_max_units`.
    Mature,
    /// At or beyond `mature_max_units`.
    Old,
}

/// Threshold parameters for [`token_age_bucket`] (constitution 102). Edges are in
/// the same *unit* the age is expressed in (see [`token_age_in_units`]); sane use
/// requires `newborn_max_units <= young_max_units <= mature_max_units`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgeBucketParams {
    /// Exclusive upper edge of [`TokenAgeBucket::Newborn`].
    pub newborn_max_units: u64,
    /// Exclusive upper edge of [`TokenAgeBucket::Young`].
    pub young_max_units: u64,
    /// Exclusive upper edge of [`TokenAgeBucket::Mature`].
    pub mature_max_units: u64,
}

impl Default for AgeBucketParams {
    /// Defaults assume the age is expressed in **seconds** (unit_ns = 1e9).
    fn default() -> Self {
        Self {
            newborn_max_units: AGE_NEWBORN_MAX_SECS,
            young_max_units: AGE_YOUNG_MAX_SECS,
            mature_max_units: AGE_MATURE_MAX_SECS,
        }
    }
}

/// Info-time age of a token in nanoseconds: `info_time_ns - creation_ns`
/// (constitution 20/21.6). Both arguments are caller-supplied *information time* —
/// there is no wall clock. Returns `None` when `info_time_ns < creation_ns` (the
/// decision instant precedes creation, which has no non-negative age) rather than
/// wrapping.
#[must_use]
pub fn info_age_ns(info_time_ns: u64, creation_ns: u64) -> Option<u64> {
    if info_time_ns < creation_ns {
        None
    } else {
        Some(info_time_ns - creation_ns)
    }
}

/// Token age in caller-chosen units — slots or seconds — via a supplied `unit_ns`
/// scale (constitution 21.6): `info_age_ns / unit_ns` (integer, truncating). For
/// seconds pass `1_000_000_000`; for a slot pass the slot duration in ns. Returns
/// `None` when `unit_ns == 0` (undefined scale) or when [`info_age_ns`] is `None`.
#[must_use]
pub fn token_age_in_units(info_time_ns: u64, creation_ns: u64, unit_ns: u64) -> Option<u64> {
    if unit_ns == 0 {
        return None;
    }
    let age = info_age_ns(info_time_ns, creation_ns)?;
    Some(age / unit_ns)
}

/// Bucket a token age (already in the params' unit) into a [`TokenAgeBucket`]
/// under `params` (constitution 21.6). Boundaries are exclusive on the low side:
/// an age exactly equal to `newborn_max_units` is [`TokenAgeBucket::Young`], and
/// one exactly equal to `mature_max_units` is [`TokenAgeBucket::Old`].
#[must_use]
pub fn token_age_bucket(age_units: u64, params: &AgeBucketParams) -> TokenAgeBucket {
    if age_units < params.newborn_max_units {
        TokenAgeBucket::Newborn
    } else if age_units < params.young_max_units {
        TokenAgeBucket::Young
    } else if age_units < params.mature_max_units {
        TokenAgeBucket::Mature
    } else {
        TokenAgeBucket::Old
    }
}

/// Nanoseconds-of-day of an information-time instant: `info_time_ns % NS_PER_DAY`
/// (constitution 21.6). This is a pure integer modulo of the caller-supplied
/// info-time — explicitly **not** a wall-clock read and carrying no calendar-date
/// meaning; it exists only to condition features on intra-day phase.
#[must_use]
pub fn ns_of_day(info_time_ns: u64) -> u64 {
    info_time_ns % NS_PER_DAY
}

/// Time-of-day bucket index in `[0, num_buckets)` derived from the info-time
/// nanoseconds-of-day (constitution 21.6). The day is partitioned into
/// `num_buckets` equal slices of width `NS_PER_DAY / num_buckets`; the returned
/// index is `ns_of_day / width`, clamped to `num_buckets - 1` so integer-division
/// remainder at the very end of the day never yields an out-of-range index.
///
/// Returns `None` only when `num_buckets == 0`. For any non-zero `u32` count the
/// slice width `NS_PER_DAY / num_buckets` is at least one nanosecond (`u32::MAX`
/// is far below `NS_PER_DAY`), so the division is always well-defined. Info-time
/// only; no wall clock.
#[must_use]
pub fn time_of_day_bucket(info_time_ns: u64, num_buckets: u32) -> Option<u32> {
    if num_buckets == 0 {
        return None;
    }
    let buckets = u64::from(num_buckets);
    // width >= 1 for every non-zero u32: u32::MAX << NS_PER_DAY, so no div-by-zero.
    let width = NS_PER_DAY / buckets;
    let idx = (ns_of_day(info_time_ns) / width).min(buckets - 1);
    Some(idx as u32)
}

// ---------------------------------------------------------------------------
// 5. Realized-volatility helper (Wave 2 stop scaling)
// ---------------------------------------------------------------------------

/// Realized volatility over `bars`, in basis points of the mean close price
/// (constitution 21.6). This is the helper the execution engine consumes in Wave 2
/// to scale stops.
///
/// **Definition (integer-exact, chosen and documented):** the sum of absolute
/// close-to-close price changes normalised by the mean close, in basis points:
///
/// ```text
///   sum_abs   = Σ_{i=1..n-1} |close_i − close_{i−1}|      (PRICE_SCALE units)
///   mean_close = (Σ_{i=0..n-1} close_i) / n
///   rv_bps    = sum_abs · BPS_SCALE / mean_close
///             = sum_abs · BPS_SCALE · n / Σ close_i       (single division)
/// ```
///
/// The right-hand rearrangement is what is computed: folding `n` into the numerator
/// so there is exactly **one** integer division (no compounding truncation from a
/// separately-rounded mean). This "sum of absolute returns" (total path length) is
/// preferred over a high−low true-range sum because it (a) is float-free and needs
/// no logarithm, (b) reuses the `close_fp` values already in [`crate::types::PRICE_SCALE`]
/// units, and (c) normalising by the mean close makes it scale-free (bp), so a stop
/// multiple derived from it transfers across tokens of very different absolute price.
/// It grows with the window length, so callers scale stops from a *fixed*-length
/// window for comparability.
///
/// Returns `None` when `bars.len() < 2` (no close-to-close return exists) or when
/// the close sum is non-positive (mean undefined). All arithmetic is saturating
/// [`i128`] (constitution 22).
#[must_use]
pub fn realized_vol_bps(bars: &[Bar]) -> Option<i128> {
    let n = bars.len();
    if n < 2 {
        return None;
    }
    let mut sum_abs = 0i128;
    let mut sum_close = 0i128;
    sum_close = sum_close.saturating_add(bars[0].close_fp);
    for w in bars.windows(2) {
        sum_abs = sum_abs.saturating_add(abs_diff(w[1].close_fp, w[0].close_fp));
        sum_close = sum_close.saturating_add(w[1].close_fp);
    }
    if sum_close <= 0 {
        return None;
    }
    let num = sum_abs.saturating_mul(BPS_SCALE).saturating_mul(n as i128);
    Some(num / sum_close)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EventId;

    /// Build a bar from OHLC (fixed-point) with dummy volumes/provenance. Only the
    /// price fields drive the detectors under test.
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

    // --- retrace ---------------------------------------------------------

    /// Three-bar shape: rally base low 0, peak high 10_000 at idx 1 (its own low
    /// 8_000), then a pullback bar whose low is `trough`. swing = 10_000.
    fn retrace_bars(trough: i128) -> Vec<Bar> {
        vec![
            bar(0, 5_000, 0, 4_000),
            bar(4_000, 10_000, 8_000, 9_000),
            bar(9_000, 9_500, trough, trough + 100),
        ]
    }

    #[test]
    fn retrace_bps_basic_value() {
        // trough 6_180 -> retrace 3_820 -> 3_820 bp.
        assert_eq!(retrace_bps(&retrace_bars(6_180)), Some(3_820));
    }

    #[test]
    fn retrace_state_monotonic_deeper() {
        let p = RetraceParams::default();
        // Shallow -> Golden -> Deep -> FullReversal as trough falls.
        assert_eq!(
            retrace_state(&retrace_bars(8_000), &p),
            Some(RetraceState::Shallow)
        );
        assert_eq!(
            retrace_state(&retrace_bars(6_180), &p),
            Some(RetraceState::Golden)
        );
        assert_eq!(
            retrace_state(&retrace_bars(3_000), &p),
            Some(RetraceState::Deep)
        );
        assert_eq!(
            retrace_state(&retrace_bars(0), &p),
            Some(RetraceState::FullReversal)
        );
    }

    #[test]
    fn retrace_state_boundary_golden_lo_is_golden() {
        // trough 6_180 -> 3_820 bp == golden_lo -> Golden (inclusive).
        let p = RetraceParams::default();
        assert_eq!(
            retrace_state(&retrace_bars(6_180), &p),
            Some(RetraceState::Golden)
        );
        // One bp shallower -> Shallow.
        assert_eq!(retrace_bps(&retrace_bars(6_181)), Some(3_819));
        assert_eq!(
            retrace_state(&retrace_bars(6_181), &p),
            Some(RetraceState::Shallow)
        );
    }

    #[test]
    fn retrace_state_boundary_golden_hi_and_full() {
        let p = RetraceParams::default();
        // trough 3_820 -> retrace 6_180 == golden_hi -> Golden (inclusive).
        assert_eq!(retrace_bps(&retrace_bars(3_820)), Some(6_180));
        assert_eq!(
            retrace_state(&retrace_bars(3_820), &p),
            Some(RetraceState::Golden)
        );
        // trough 3_819 -> 6_181 bp -> Deep.
        assert_eq!(
            retrace_state(&retrace_bars(3_819), &p),
            Some(RetraceState::Deep)
        );
        // trough 0 -> 10_000 bp == full -> FullReversal (inclusive).
        assert_eq!(retrace_bps(&retrace_bars(0)), Some(10_000));
        assert_eq!(
            retrace_state(&retrace_bars(0), &p),
            Some(RetraceState::FullReversal)
        );
    }

    #[test]
    fn retrace_empty_and_single_are_none() {
        let p = RetraceParams::default();
        assert_eq!(retrace_bps(&[]), None);
        assert_eq!(retrace_state(&[], &p), None);
        let one = [bar(0, 10, 0, 5)];
        assert_eq!(retrace_bps(&one), None);
        assert_eq!(retrace_state(&one, &p), None);
    }

    #[test]
    fn retrace_flat_all_equal_is_none() {
        // All-equal bars: swing == 0 -> undefined.
        let flat = vec![bar(5, 5, 5, 5), bar(5, 5, 5, 5), bar(5, 5, 5, 5)];
        assert_eq!(retrace_bps(&flat), None);
        assert_eq!(retrace_state(&flat, &RetraceParams::default()), None);
    }

    // --- volatility regime ----------------------------------------------

    /// Two bars: baseline range 10_000 (idx 0), recent range `r` (idx 1), lows 0.
    fn vol_bars(r: i128) -> Vec<Bar> {
        vec![bar(0, 10_000, 0, 5_000), bar(0, r, 0, r / 2)]
    }

    #[test]
    fn vol_regime_monotonic_grades() {
        let p = VolRegimeParams::default();
        assert_eq!(
            vol_regime(&vol_bars(5_000), 1, 1, &p),
            Some(VolRegime::Compressed)
        );
        assert_eq!(
            vol_regime(&vol_bars(10_000), 1, 1, &p),
            Some(VolRegime::Normal)
        );
        assert_eq!(
            vol_regime(&vol_bars(20_000), 1, 1, &p),
            Some(VolRegime::Expanded)
        );
        assert_eq!(
            vol_regime(&vol_bars(40_000), 1, 1, &p),
            Some(VolRegime::Explosive)
        );
    }

    #[test]
    fn vol_regime_boundary_compressed() {
        let p = VolRegimeParams::default();
        // r 7_000 -> ratio 7_000 bp == compressed_bps -> Compressed (inclusive).
        assert_eq!(mean_range_ratio_bps(&vol_bars(7_000), 1, 1), Some(7_000));
        assert_eq!(
            vol_regime(&vol_bars(7_000), 1, 1, &p),
            Some(VolRegime::Compressed)
        );
        // r 7_001 -> just above -> Normal.
        assert_eq!(
            vol_regime(&vol_bars(7_001), 1, 1, &p),
            Some(VolRegime::Normal)
        );
    }

    #[test]
    fn vol_regime_boundary_expanded() {
        let p = VolRegimeParams::default();
        // r 15_000 -> 15_000 bp == expanded_bps -> Expanded (inclusive).
        assert_eq!(
            vol_regime(&vol_bars(15_000), 1, 1, &p),
            Some(VolRegime::Expanded)
        );
        // r 14_999 -> just below -> Normal.
        assert_eq!(
            vol_regime(&vol_bars(14_999), 1, 1, &p),
            Some(VolRegime::Normal)
        );
    }

    #[test]
    fn vol_regime_boundary_explosive() {
        let p = VolRegimeParams::default();
        // r 30_000 -> 30_000 bp == explosive_bps -> Explosive (inclusive).
        assert_eq!(
            vol_regime(&vol_bars(30_000), 1, 1, &p),
            Some(VolRegime::Explosive)
        );
        // r 29_999 -> just below -> Expanded.
        assert_eq!(
            vol_regime(&vol_bars(29_999), 1, 1, &p),
            Some(VolRegime::Expanded)
        );
    }

    #[test]
    fn vol_regime_insufficient_and_zero_baseline() {
        let p = VolRegimeParams::default();
        // Too few bars.
        assert_eq!(vol_regime(&vol_bars(10_000)[..1], 1, 1, &p), None);
        // Zero window.
        assert_eq!(vol_regime(&vol_bars(10_000), 0, 1, &p), None);
        // Zero baseline range -> undefined ratio.
        let flat_base = vec![bar(5, 5, 5, 5), bar(0, 100, 0, 50)];
        assert_eq!(vol_regime(&flat_base, 1, 1, &p), None);
        assert_eq!(mean_range_ratio_bps(&flat_base, 1, 1), None);
    }

    #[test]
    fn vol_regime_multi_bar_windows() {
        // recent=2 ranges {20_000,20_000} mean 20_000; baseline=2 ranges
        // {10_000,10_000} mean 10_000 -> ratio 20_000 bp -> Expanded.
        let bars = vec![
            bar(0, 10_000, 0, 5_000),
            bar(0, 10_000, 0, 5_000),
            bar(0, 20_000, 0, 10_000),
            bar(0, 20_000, 0, 10_000),
        ];
        assert_eq!(mean_range_ratio_bps(&bars, 2, 2), Some(20_000));
        assert_eq!(
            vol_regime(&bars, 2, 2, &VolRegimeParams::default()),
            Some(VolRegime::Expanded)
        );
    }

    // --- wick microstructure --------------------------------------------

    #[test]
    fn wick_metrics_marubozu_exact() {
        // open==low, close==high: body == range, no wicks.
        let b = bar(0, 100, 0, 100);
        let m = wick_metrics(&b).unwrap();
        assert_eq!(m.upper_wick_bps, 0);
        assert_eq!(m.lower_wick_bps, 0);
        assert_eq!(m.body_bps, 10_000);
    }

    #[test]
    fn wick_metrics_upper_rejection_exact() {
        // range 100, body from 0..40 (open 0 close 40), upper wick 60, no lower.
        let b = bar(0, 100, 0, 40);
        let m = wick_metrics(&b).unwrap();
        assert_eq!(m.upper_wick_bps, 6_000);
        assert_eq!(m.lower_wick_bps, 0);
        assert_eq!(m.body_bps, 4_000);
    }

    #[test]
    fn wick_metrics_lower_rejection_exact() {
        // range 100, low 0, body 60..100, lower wick 60, no upper.
        let b = bar(100, 100, 0, 60);
        let m = wick_metrics(&b).unwrap();
        assert_eq!(m.upper_wick_bps, 0);
        assert_eq!(m.lower_wick_bps, 6_000);
        assert_eq!(m.body_bps, 4_000);
    }

    #[test]
    fn wick_metrics_zero_range_is_none() {
        assert_eq!(wick_metrics(&bar(5, 5, 5, 5)), None);
    }

    #[test]
    fn wick_shape_all_variants() {
        let p = WickParams::default();
        // Doji: body 5% (<= 10%). range 100, body 5.
        assert_eq!(wick_shape(&bar(0, 100, 0, 5), &p), WickShape::Doji);
        // Marubozu: body 90% (>= 80%). open 0 close 90, range 100.
        assert_eq!(wick_shape(&bar(0, 100, 0, 90), &p), WickShape::Marubozu);
        // Upper rejection: upper wick 60% (>= 40%), body 40%.
        assert_eq!(
            wick_shape(&bar(0, 100, 0, 40), &p),
            WickShape::UpperRejection
        );
        // Lower rejection: lower wick 60%, body 40%.
        assert_eq!(
            wick_shape(&bar(100, 100, 0, 60), &p),
            WickShape::LowerRejection
        );
        // Neutral: body 30%, wicks 35% each (below rejection threshold 40%).
        // range 100, body 30 (open 35 close 65 -> body 30), upper 35, lower 35.
        assert_eq!(wick_shape(&bar(35, 100, 0, 65), &p), WickShape::Neutral);
        // Zero-range degenerate -> Doji, no panic.
        assert_eq!(wick_shape(&bar(5, 5, 5, 5), &p), WickShape::Doji);
    }

    #[test]
    fn wick_shape_doji_boundary_inclusive() {
        let p = WickParams::default();
        // body exactly 10% == doji_body_bps -> Doji (inclusive).
        assert_eq!(wick_shape(&bar(0, 100, 0, 10), &p), WickShape::Doji);
        // body 11% -> not doji; upper wick 89% -> UpperRejection.
        assert_eq!(
            wick_shape(&bar(0, 100, 0, 11), &p),
            WickShape::UpperRejection
        );
    }

    #[test]
    fn wick_shape_rejection_requires_dominance() {
        // Symmetric wicks 45% each, body 10% -> doji wins (body <= 10%).
        let p = WickParams::default();
        let m = wick_metrics(&bar(45, 100, 0, 55)).unwrap();
        assert_eq!(m.upper_wick_bps, 4_500);
        assert_eq!(m.lower_wick_bps, 4_500);
        assert_eq!(wick_shape(&bar(45, 100, 0, 55), &p), WickShape::Doji);
    }

    #[test]
    fn sell_wick_pressure_aggregate() {
        // Two bars: upper wicks 60 and 0, ranges 100 and 100 -> 60/200 = 3_000 bp.
        let bars = [bar(0, 100, 0, 40), bar(0, 100, 0, 100)];
        assert_eq!(sell_wick_pressure_bps(&bars), Some(3_000));
    }

    #[test]
    fn sell_wick_pressure_empty_and_flat_none() {
        assert_eq!(sell_wick_pressure_bps(&[]), None);
        let flat = [bar(5, 5, 5, 5), bar(9, 9, 9, 9)];
        assert_eq!(sell_wick_pressure_bps(&flat), None);
    }

    #[test]
    fn sell_wick_pressure_skips_zero_range_bars() {
        // Flat bar contributes nothing; only the 60/100 upper-wick bar counts.
        let bars = [bar(9, 9, 9, 9), bar(0, 100, 0, 40)];
        assert_eq!(sell_wick_pressure_bps(&bars), Some(6_000));
    }

    // --- token age & time-of-day ----------------------------------------

    const NS_PER_SEC: u64 = 1_000_000_000;

    #[test]
    fn token_age_in_units_seconds() {
        // 42 s after creation, unit = 1 s.
        let created = 1_000 * NS_PER_SEC;
        let now = created + 42 * NS_PER_SEC;
        assert_eq!(token_age_in_units(now, created, NS_PER_SEC), Some(42));
    }

    #[test]
    fn token_age_before_creation_is_none() {
        assert_eq!(info_age_ns(500, 1_000), None);
        assert_eq!(token_age_in_units(500, 1_000, NS_PER_SEC), None);
    }

    #[test]
    fn token_age_zero_unit_is_none() {
        assert_eq!(token_age_in_units(2_000, 1_000, 0), None);
    }

    #[test]
    fn token_age_bucket_variants_and_boundaries() {
        let p = AgeBucketParams::default(); // 300 / 3_600 / 86_400 secs
        assert_eq!(token_age_bucket(0, &p), TokenAgeBucket::Newborn);
        assert_eq!(token_age_bucket(299, &p), TokenAgeBucket::Newborn);
        // Boundary: exactly 300 -> Young (low edge exclusive).
        assert_eq!(token_age_bucket(300, &p), TokenAgeBucket::Young);
        assert_eq!(token_age_bucket(3_599, &p), TokenAgeBucket::Young);
        assert_eq!(token_age_bucket(3_600, &p), TokenAgeBucket::Mature);
        assert_eq!(token_age_bucket(86_399, &p), TokenAgeBucket::Mature);
        // Boundary: exactly 86_400 -> Old.
        assert_eq!(token_age_bucket(86_400, &p), TokenAgeBucket::Old);
        assert_eq!(token_age_bucket(1_000_000, &p), TokenAgeBucket::Old);
    }

    #[test]
    fn ns_of_day_wraps() {
        assert_eq!(ns_of_day(0), 0);
        assert_eq!(ns_of_day(NS_PER_DAY), 0);
        assert_eq!(ns_of_day(NS_PER_DAY + 123), 123);
        // 2.5 days -> half a day of ns.
        assert_eq!(ns_of_day(2 * NS_PER_DAY + NS_PER_DAY / 2), NS_PER_DAY / 2);
    }

    #[test]
    fn time_of_day_bucket_quarters() {
        // 4 buckets of 6h each.
        let six_h = NS_PER_DAY / 4;
        assert_eq!(time_of_day_bucket(0, 4), Some(0));
        assert_eq!(time_of_day_bucket(six_h - 1, 4), Some(0));
        assert_eq!(time_of_day_bucket(six_h, 4), Some(1));
        assert_eq!(time_of_day_bucket(3 * six_h, 4), Some(3));
        // End-of-day clamps to last bucket, never 4.
        assert_eq!(time_of_day_bucket(NS_PER_DAY - 1, 4), Some(3));
        // Wrap: next day start is bucket 0 again.
        assert_eq!(time_of_day_bucket(NS_PER_DAY, 4), Some(0));
    }

    #[test]
    fn time_of_day_bucket_degenerate() {
        // Zero buckets is the only undefined case.
        assert_eq!(time_of_day_bucket(123, 0), None);
        // A huge (but valid) bucket count still resolves and stays in range:
        // width = NS_PER_DAY / u32::MAX >= 1, so no div-by-zero and idx < count.
        let idx = time_of_day_bucket(NS_PER_DAY - 1, u32::MAX).unwrap();
        assert!(idx < u32::MAX);
        assert_eq!(time_of_day_bucket(0, u32::MAX), Some(0));
    }

    // --- realized volatility --------------------------------------------

    /// Bars carrying only close prices (OHLC set to close so ranges are irrelevant).
    fn close_bars(closes: &[i128]) -> Vec<Bar> {
        closes.iter().map(|&c| bar(c, c, c, c)).collect()
    }

    #[test]
    fn realized_vol_bps_known_value() {
        // closes 100,110,90,130: sum_abs = 10+20+40 = 70; sum_close = 430; n = 4.
        // rv = 70 * 10_000 * 4 / 430 = 2_800_000 / 430 = 6_511.
        assert_eq!(
            realized_vol_bps(&close_bars(&[100, 110, 90, 130])),
            Some(6_511)
        );
    }

    #[test]
    fn realized_vol_bps_too_few_bars_none() {
        assert_eq!(realized_vol_bps(&[]), None);
        assert_eq!(realized_vol_bps(&close_bars(&[100])), None);
    }

    #[test]
    fn realized_vol_bps_flat_is_zero() {
        // No close-to-close movement -> 0 bp, but sum_close > 0 so Some(0).
        assert_eq!(realized_vol_bps(&close_bars(&[100, 100, 100])), Some(0));
    }

    #[test]
    fn realized_vol_bps_monotonic_in_movement() {
        let calm = realized_vol_bps(&close_bars(&[100, 101, 100, 101])).unwrap();
        let wild = realized_vol_bps(&close_bars(&[100, 140, 60, 140])).unwrap();
        assert!(wild > calm, "wild {wild} should exceed calm {calm}");
    }

    #[test]
    fn realized_vol_bps_nonpositive_close_sum_none() {
        // Degenerate: zero close prices -> sum_close == 0 -> None (no wrap/panic).
        assert_eq!(realized_vol_bps(&close_bars(&[0, 0, 0])), None);
    }
}
