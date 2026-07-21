//! fixedpoint — exact integer/fixed-point AMM math (§18.2, §22). No floats anywhere.
//! All outcome-controlling arithmetic is integer; overflow is explicit, never silent.

/// 128×128→256 full product, returned as (hi, lo) with `a*b = hi·2^128 + lo`.
#[inline]
fn mul_full(a: u128, b: u128) -> (u128, u128) {
    let (a_lo, a_hi) = (a & u64::MAX as u128, a >> 64);
    let (b_lo, b_hi) = (b & u64::MAX as u128, b >> 64);
    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;
    let mut lo = ll;
    let mut hi = hh;
    let (s1, c1) = lo.overflowing_add(lh << 64);
    lo = s1; hi += (lh >> 64) + c1 as u128;
    let (s2, c2) = lo.overflowing_add(hl << 64);
    lo = s2; hi += (hl >> 64) + c2 as u128;
    (hi, lo)
}

/// Divide the 256-bit value (hi·2^128 + lo) by `d`. `None` if the exact quotient exceeds
/// u128 (i.e. hi >= d). Overflow-safe binary long division; remainder invariant rem < d.
#[inline]
fn div_256_by_128(hi: u128, lo: u128, d: u128) -> Option<u128> {
    if d == 0 || hi >= d {
        return None; // hi >= d => quotient >= 2^128, does not fit u128
    }
    let mut rem = hi;
    let mut quo = 0u128;
    let mut i = 128;
    while i > 0 {
        i -= 1;
        let carry = rem >> 127; // MSB shifted out becomes the 2^128 carry bit
        rem = (rem << 1) | ((lo >> i) & 1);
        quo <<= 1;
        // true remainder is (carry*2^128 + rem); it is < 2d, so at most one subtraction
        if carry == 1 || rem >= d {
            rem = rem.wrapping_sub(d);
            quo |= 1;
        }
    }
    Some(quo)
}

/// mul_div_u128 — exact `(a * b) / c`, truncating toward zero, with a full 256-bit
/// intermediate so no precision is lost when `a*b` exceeds u128. Returns `None` if `c == 0`
/// or the exact quotient does not fit in u128.
pub fn mul_div_u128(a: u128, b: u128, c: u128) -> Option<u128> {
    if c == 0 {
        return None;
    }
    match a.checked_mul(b) {
        Some(prod) => Some(prod / c), // fast path: product fits u128
        None => {
            let (hi, lo) = mul_full(a, b);
            div_256_by_128(hi, lo, c)
        }
    }
}

/// amount_out — constant-product exact-in swap output with a `fee_num/fee_den` fee removed
/// from the input first. Integer-exact via [`mul_div_u128`]. `None` on zero reserves/input,
/// a degenerate fee, or a fee that consumes the entire input.
pub fn amount_out(
    reserve_in: u128,
    reserve_out: u128,
    amount_in: u128,
    fee_num: u64,
    fee_den: u64,
) -> Option<u128> {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return None;
    }
    if fee_den == 0 || fee_num as u128 >= fee_den as u128 {
        return None;
    }
    let (fn_, fd) = (fee_num as u128, fee_den as u128);
    let dx_eff = mul_div_u128(amount_in, fd - fn_, fd)?;
    if dx_eff == 0 {
        return None;
    }
    let denom = reserve_in.checked_add(dx_eff)?;
    mul_div_u128(reserve_out, dx_eff, denom)
}
