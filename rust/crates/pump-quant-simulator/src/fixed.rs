//! Integer / fixed-point arithmetic primitives.
//!
//! Responsibility: provide the only sanctioned way to do proportional (basis-point)
//! math in this crate so that no outcome-controlling code touches floating point
//! (constitution §22). Every routine documents its overflow contract explicitly:
//! widening is done through `u128`/`i128`, and any narrowing back to the native
//! width is saturating-by-contract (never a silent wrap).

/// One hundred percent expressed in basis points (`10_000` bps == `100%`).
pub const BPS_ONE: u32 = 10_000;

/// `floor(amount * bps / BPS_ONE)`.
///
/// Computed with a `u128` intermediate so the multiplication never overflows for
/// any `u64` × `u32`. When `bps <= BPS_ONE` the result is `<= amount` and always
/// fits `u64`; for `bps > BPS_ONE` (a caller passing an amplification factor) the
/// result is saturated to `u64::MAX` by contract rather than wrapping.
#[must_use]
pub fn mul_bps(amount: u64, bps: u32) -> u64 {
    let product = (amount as u128) * (bps as u128) / (BPS_ONE as u128);
    if product > u64::MAX as u128 {
        u64::MAX
    } else {
        product as u64
    }
}

/// `amount` reduced by `bps` (a fee or slippage haircut), i.e.
/// `amount - floor(amount * min(bps, BPS_ONE) / BPS_ONE)`.
///
/// `bps` is clamped to `BPS_ONE` so a reduction can never exceed the amount and
/// the result is always in `[0, amount]` — the subtraction cannot underflow.
#[must_use]
pub fn reduce_bps(amount: u64, bps: u32) -> u64 {
    let capped = bps.min(BPS_ONE);
    // `mul_bps(amount, capped) <= amount` because `capped <= BPS_ONE`.
    amount - mul_bps(amount, capped)
}

/// `value` scaled by a *signed* move: `floor(value * (BPS_ONE + signed_bps) / BPS_ONE)`.
///
/// Used to apply a recorded price move to a position value. Returns `i128` because
/// the scaled value can exceed `u64` for large positive moves. A move at or below
/// `-100%` (`signed_bps <= -BPS_ONE`) clamps the result to `0`: a position value
/// can never be driven negative by price alone (losses beyond the basis are an
/// accounting concern of the caller, not of price scaling).
#[must_use]
pub fn scale_signed_bps(value: u64, signed_bps: i32) -> i128 {
    let factor = BPS_ONE as i128 + signed_bps as i128;
    if factor <= 0 {
        return 0;
    }
    (value as i128) * factor / (BPS_ONE as i128)
}

/// Saturating cast of a non-negative `i128` down to `u64`.
///
/// Contract: callers pass values that are logically `>= 0`; a negative input maps
/// to `0` and an over-large input saturates to `u64::MAX` rather than wrapping.
#[must_use]
pub fn i128_to_u64_saturating(value: i128) -> u64 {
    if value <= 0 {
        0
    } else if value > u64::MAX as i128 {
        u64::MAX
    } else {
        value as u64
    }
}
