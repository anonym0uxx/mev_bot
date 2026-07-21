//! Integer bonding-curve and constant-product output math.
//!
//! # Responsibility
//! Compute swap *outputs* deterministically using only integer arithmetic,
//! ported faithfully from `bonding-curve-sim.ts`:
//!
//! * [`pump_amount_out`] — tokens received for a SOL buy against a pump.fun
//!   bonding curve (1% buy fee, constant-product `k = vSol * vToken`).
//! * [`pumpswap_amount_out`] — output of a generic constant-product AMM swap
//!   with a basis-point fee, computed full-width in `u128` with no precision
//!   loss.
//!
//! # Constitution
//! * §22 — no `f64`. The legacy `priceImpactPct` (the only float in the source)
//!   is intentionally NOT ported: it never controlled an outcome. All math is
//!   integer; intermediate products are widened to `u128` so `k` cannot
//!   overflow.
//! * Overflow and divide-by-zero are explicit — every fallible step returns
//!   `None` rather than panicking or wrapping silently.

use crate::decode::PumpCurve;

/// pump.fun charges a 1% fee on buys: `fee = amount * 100 / 10_000`.
const FEE_NUMERATOR: u128 = 100;
/// Basis-point denominator used for the buy fee.
const FEE_DENOMINATOR: u128 = 10_000;

/// Compute tokens received for buying with `sol_in` lamports against `curve`.
///
/// Ports `BondingCurveSimulator.simulateBuy`:
///
/// 1. `fee            = sol_in * 100 / 10_000`
/// 2. `sol_in_net     = sol_in - fee`
/// 3. `k              = virtual_sol * virtual_token`  (widened to `u128`)
/// 4. `new_v_sol      = virtual_sol + sol_in_net`
/// 5. `new_v_token    = k / new_v_sol`
/// 6. `tokens_out     = virtual_token - new_v_token`
///
/// Returns `None` on overflow, on an empty/zero-reserve curve that would
/// divide by zero, or if the result does not fit in `u64`.
///
/// # Constitution
/// §22 — integer-only; all arithmetic widened to `u128` and checked.
pub fn pump_amount_out(curve: &PumpCurve, sol_in: u64) -> Option<u64> {
    let v_sol = curve.virtual_sol as u128;
    let v_token = curve.virtual_token as u128;
    let sol_in = sol_in as u128;

    // 1% buy fee, floor-divided exactly as the legacy bigint math.
    let fee = sol_in.checked_mul(FEE_NUMERATOR)? / FEE_DENOMINATOR;
    let sol_in_net = sol_in.checked_sub(fee)?;

    // Constant product k = vSol * vToken.
    let k = v_sol.checked_mul(v_token)?;
    let new_v_sol = v_sol.checked_add(sol_in_net)?;
    if new_v_sol == 0 {
        // Would divide by zero (only reachable when vSol == 0 and net == 0).
        return None;
    }
    let new_v_token = k / new_v_sol;

    // tokens_out = vToken - new_v_token. new_v_token <= v_token whenever
    // sol_in_net >= 0, but guard explicitly rather than assume.
    let tokens_out = v_token.checked_sub(new_v_token)?;
    u64::try_from(tokens_out).ok()
}

/// Constant-product AMM output with a basis-point fee, full-width `u128`.
///
/// Given input reserve `reserve_in`, output reserve `reserve_out`, gross input
/// `amount_in`, and a fee in basis points `fee_bps`:
///
/// 1. `amount_in_net = amount_in * (10_000 - fee_bps) / 10_000`
/// 2. `amount_out    = reserve_out * amount_in_net / (reserve_in + amount_in_net)`
///
/// This is the classic `x*y=k` swap: it preserves the invariant that the new
/// product is `>=` the old one after the fee is retained. Everything is `u128`
/// so no precision is lost for realistic Solana reserve/amount magnitudes.
///
/// Returns `None` if `fee_bps > 10_000` (nonsensical fee), on any arithmetic
/// overflow, or when `reserve_in + amount_in_net == 0` (empty pool).
///
/// # Constitution
/// §22 — integer-only, full-width, checked at every step.
pub fn pumpswap_amount_out(
    reserve_in: u128,
    reserve_out: u128,
    amount_in: u128,
    fee_bps: u32,
) -> Option<u128> {
    let fee_bps = fee_bps as u128;
    if fee_bps > FEE_DENOMINATOR {
        return None;
    }

    // Apply fee: keep (10_000 - fee_bps)/10_000 of the input.
    let keep_bps = FEE_DENOMINATOR - fee_bps;
    let amount_in_net = amount_in.checked_mul(keep_bps)? / FEE_DENOMINATOR;

    let denominator = reserve_in.checked_add(amount_in_net)?;
    if denominator == 0 {
        return None;
    }
    let numerator = reserve_out.checked_mul(amount_in_net)?;
    Some(numerator / denominator)
}
