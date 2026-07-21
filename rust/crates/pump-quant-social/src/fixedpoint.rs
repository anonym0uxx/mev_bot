//! Deterministic integer fixed-point primitives (constitution §22).
//!
//! # Responsibility
//! Provide the basis-point (`bps`, scale [`BPS_SCALE`] = 10_000) arithmetic used by
//! every determinant scorer: reconciled markout computation, exponential time decay,
//! sample-size confidence, and decayed weighted means. All operations are pure
//! integer and overflow-explicit — accumulation widens to `i128` and results are
//! clamped back into `i64` rather than being allowed to wrap.

/// Basis-point fixed-point scale. `10_000 bps == 100% == 1.0`.
///
/// §22: money and ratios are integer/fixed-point, never `f32`/`f64`.
pub const BPS_SCALE: i64 = 10_000;

/// Saturating clamp of a widened `i128` accumulator back into `i64`.
///
/// Explicit overflow contract (§22): out-of-range values saturate to the `i64`
/// bounds instead of wrapping.
#[must_use]
pub fn clamp_i128_to_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

/// Clamp a signed bps value into the canonical `[-BPS_SCALE, +BPS_SCALE]` band.
///
/// Used where a determinant is a bounded ratio (e.g. an authenticity fraction);
/// raw markouts are intentionally left unclamped since a token can 10× (>10_000 bps).
#[must_use]
pub fn clamp_bps(v: i64) -> i64 {
    v.clamp(-BPS_SCALE, BPS_SCALE)
}

/// Forward executable return in bps: `(after - before) / before * 10_000`.
///
/// D1 ground truth (§29.8). Computed against reconstructed market state, so a call
/// on a token that doubled returns `+10_000`. A zero or missing `price_before`
/// yields `0` (no divide-by-zero, deterministic). Widened through `i128`.
#[must_use]
pub fn markout_bps(price_before: u64, price_after: u64) -> i64 {
    if price_before == 0 {
        return 0;
    }
    let before = price_before as i128;
    let after = price_after as i128;
    let diff = after - before;
    let scaled = diff.saturating_mul(BPS_SCALE as i128) / before;
    clamp_i128_to_i64(scaled)
}

/// Exponential time-decay weight in bps for a sample of the given age.
///
/// Deterministic integer approximation of `2^(-age/half_life)` scaled to
/// [`BPS_SCALE`]: an age of zero weighs `10_000`, an age of exactly one half-life
/// weighs `5_000`, and the fractional part is linearly interpolated between
/// successive halvings. `half_life_ns == 0` disables decay (returns `BPS_SCALE`).
/// Beyond 63 half-lives the weight is exactly `0` (fully decayed).
///
/// This is the time-decay component §29.8 requires every determinant to carry.
#[must_use]
pub fn decay_weight_bps(age_ns: u64, half_life_ns: u64) -> i64 {
    if half_life_ns == 0 {
        return BPS_SCALE;
    }
    let whole = age_ns / half_life_ns;
    if whole >= 63 {
        return 0;
    }
    let base = BPS_SCALE >> whole;
    let frac = (age_ns % half_life_ns) as i128;
    let half = (base >> 1) as i128;
    let reduce = half.saturating_mul(frac) / (half_life_ns as i128);
    let w = base as i128 - reduce;
    if w < 0 {
        0
    } else {
        w as i64
    }
}

/// Confidence in bps from sample size via a saturating Bayesian shrinkage curve:
/// `10_000 * n / (n + half_saturation)`.
///
/// §29.8: every determinant is stored with a confidence. Zero samples give zero
/// confidence; confidence rises monotonically toward `10_000` and equals `5_000`
/// when `n == half_saturation`. `half_saturation` encodes how many reconciled calls
/// are needed before the determinant is half-trusted.
#[must_use]
pub fn confidence_bps(sample_size: u32, half_saturation: u32) -> u16 {
    if sample_size == 0 {
        return 0;
    }
    let n = sample_size as u64;
    let k = half_saturation as u64;
    let denom = n + k;
    let v = (BPS_SCALE as u64) * n / denom;
    v.min(BPS_SCALE as u64) as u16
}

/// Decayed / weighted mean of `(value_bps, weight_bps)` pairs.
///
/// Returns `sum(value*weight) / sum(weight)`, widened through `i128`. An empty slice
/// or an all-zero-weight slice returns `0`. This is the aggregation kernel shared by
/// every determinant that folds per-call samples down to one decomposed score.
#[must_use]
pub fn weighted_mean_bps(samples: &[(i64, i64)]) -> i64 {
    let mut sum_wv: i128 = 0;
    let mut sum_w: i128 = 0;
    for &(v, w) in samples {
        sum_wv += (v as i128) * (w as i128);
        sum_w += w as i128;
    }
    if sum_w == 0 {
        return 0;
    }
    clamp_i128_to_i64(sum_wv / sum_w)
}

/// Saturating conversion of a `u64` count-ratio to bps: `numer / denom * 10_000`.
///
/// Convenience for the many "fraction of calls that were X" determinant inputs.
/// `denom == 0` yields `0`.
#[must_use]
pub fn ratio_bps(numer: u64, denom: u64) -> i64 {
    if denom == 0 {
        return 0;
    }
    let v = (numer as i128) * (BPS_SCALE as i128) / (denom as i128);
    clamp_i128_to_i64(v)
}
