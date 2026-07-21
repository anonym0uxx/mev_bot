//! Signed price velocity (ported from legacy `momentum::velocity`).
//!
//! The legacy module computed sliding-window velocity over a sample buffer in
//! milli-bps per tick. This crate exposes the entry-signal form used by the
//! scorer: a **two-point** signed velocity in **basis points per second**,
//! derived from a previous and current fixed-point price plus the elapsed time.
//!
//! # Constitution constraints (§22)
//!
//! Pure, stateless, deterministic, and integer-only. Intermediate products use
//! `i128` to avoid overflow, and the result is saturated into `i64` -- there is
//! no floating point and no silent wrapping.

/// Signed price velocity in basis points per second.
///
/// Computes the fractional price change from `prev_price_fp` to `cur_price_fp`,
/// expresses it in basis points, and scales it to a per-second rate using
/// `dt_ms` milliseconds of elapsed time:
///
/// ```text
/// velocity_bps_per_s = (cur - prev) * 10_000 * 1_000 / (prev * dt_ms)
/// ```
///
/// The `10_000` factor converts the fraction to bps; the `1_000 / dt_ms`
/// factor converts a per-`dt_ms` change into a per-second rate. Positive means
/// the price is rising, negative means falling.
///
/// Returns `0` when `prev_price_fp == 0` (no baseline) or `dt_ms == 0`
/// (no elapsed time), matching the legacy "insufficient data -> 0" convention.
///
/// Ported leaf `sg_velocity`.
///
/// Responsibility: turn two fixed-point price samples + a time delta into the
/// signed bps/sec velocity consumed by pre-entry momentum scoring.
/// Constitution §22: integer-only, `i128` intermediates, saturating into `i64`.
#[inline]
pub fn velocity_bps_per_s(prev_price_fp: u64, cur_price_fp: u64, dt_ms: u64) -> i64 {
    if prev_price_fp == 0 || dt_ms == 0 {
        return 0;
    }
    let delta = cur_price_fp as i128 - prev_price_fp as i128;
    // (delta / prev) is the fractional change; * 10_000 -> bps; * 1_000 / dt_ms
    // -> per second. Combine numerator to preserve integer precision.
    let numerator = delta * 10_000 * 1_000;
    let denominator = prev_price_fp as i128 * dt_ms as i128;
    let v = numerator / denominator;
    // Saturate the i128 result into i64 (explicit, never wrapping).
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}
