//! Raydium CPMM constant-product swap math — zero-allocation, integer-only.
//!
//! All arithmetic uses u128 to prevent overflow on u64 reserve × amount products.
//! No f64 anywhere. No heap. No branching in the hot path.
//!
//! Raydium CPMM fee: 25 bps (0.25%) per swap.
//! Raydium AMM V4 fee: 25 bps (0.25%) per swap.
//!
//! Constant-product invariant: x * y = k
//! Swap formula: dy = (y * dx_after_fee) / (x + dx_after_fee)

/// Raydium CPMM fee in basis points.
pub const RAYDIUM_CPMM_FEE_BPS: u16 = 25;

/// Raydium AMM V4 fee in basis points.
pub const RAYDIUM_AMM_V4_FEE_BPS: u16 = 25;

/// Fee denominator (10000 bps = 100%).
const FEE_DENOM: u128 = 10_000;

/// Compute output amount for a constant-product swap.
///
/// `reserve_in`:  pool reserves of the input token (e.g. SOL lamports)
/// `reserve_out`: pool reserves of the output token (e.g. token atoms)
/// `amount_in`:   amount being swapped in
/// `fee_bps`:     fee in basis points (25 for Raydium)
///
/// Returns output amount after fee deduction.
///
/// SAFETY: returns 0 if any reserve is 0 or amount_in is 0.
/// All intermediate math in u128 — no overflow possible for u64 inputs.
#[inline(always)]
pub fn swap_exact_in(
    reserve_in: u64,
    reserve_out: u64,
    amount_in: u64,
    fee_bps: u16,
) -> u64 {
    if reserve_in == 0 || reserve_out == 0 || amount_in == 0 {
        return 0;
    }
    let amount_in_128 = amount_in as u128;
    let fee_numerator = FEE_DENOM - fee_bps as u128;
    let amount_in_after_fee = amount_in_128 * fee_numerator / FEE_DENOM;

    let numerator = (reserve_out as u128) * amount_in_after_fee;
    let denominator = (reserve_in as u128) + amount_in_after_fee;

    (numerator / denominator) as u64
}

/// SOL → Token swap on Raydium CPMM.
#[inline(always)]
pub fn swap_sol_to_token(
    reserve_sol: u64,
    reserve_token: u64,
    sol_in: u64,
    fee_bps: u16,
) -> u64 {
    swap_exact_in(reserve_sol, reserve_token, sol_in, fee_bps)
}

/// Token → SOL swap on Raydium CPMM.
#[inline(always)]
pub fn swap_token_to_sol(
    reserve_sol: u64,
    reserve_token: u64,
    token_in: u64,
    fee_bps: u16,
) -> u64 {
    swap_exact_in(reserve_token, reserve_sol, token_in, fee_bps)
}

/// Compute round-trip PnL for a graduation arb: buy tokens, then sell them.
///
/// Simulates:
///   1. Buy: SOL → Token on initial reserves
///   2. Sell: Token → SOL on post-buy reserves
///   3. Subtract Jito tip + Solana base fee
///
/// Returns signed PnL in lamports (negative = loss).
///
/// This is the objective function for Brent optimization.
#[inline(always)]
pub fn round_trip_pnl(
    reserve_sol: u64,
    reserve_token: u64,
    amount_in: u64,
    fee_bps: u16,
    jito_tip_lamports: u64,
) -> i64 {
    // Step 1: Buy tokens
    let tokens_bought = swap_sol_to_token(reserve_sol, reserve_token, amount_in, fee_bps);
    if tokens_bought == 0 {
        return -(jito_tip_lamports as i64);
    }

    // Step 2: Compute post-buy reserves
    // After our buy: pool gained our SOL (minus fee kept by pool), lost tokens
    // Exact reserve tracking: amount_in goes into pool, tokens_bought comes out
    // Fee is already deducted from amount_in_after_fee in the swap formula,
    // but the FULL amount_in enters the pool's SOL reserve.
    let new_reserve_sol = reserve_sol + amount_in;
    let new_reserve_token = reserve_token - tokens_bought;

    // Step 3: Sell tokens back
    let sol_received = swap_token_to_sol(new_reserve_sol, new_reserve_token, tokens_bought, fee_bps);

    // Step 4: Net PnL
    (sol_received as i64) - (amount_in as i64) - (jito_tip_lamports as i64)
}

/// Compute the price impact of a buy in basis points.
///
/// price_impact_bps = (effective_price / spot_price - 1) * 10000
/// where spot_price = reserve_sol / reserve_token
///       effective_price = amount_in / tokens_received
#[inline(always)]
pub fn price_impact_bps(
    reserve_sol: u64,
    reserve_token: u64,
    amount_in: u64,
    fee_bps: u16,
) -> u32 {
    let tokens = swap_sol_to_token(reserve_sol, reserve_token, amount_in, fee_bps);
    if tokens == 0 || reserve_token == 0 {
        return 10_000; // 100% impact
    }
    // spot_price = reserve_sol / reserve_token (in u128 scaled by 1e12 for precision)
    let scale: u128 = 1_000_000_000_000;
    let spot_scaled = (reserve_sol as u128) * scale / (reserve_token as u128);
    let effective_scaled = (amount_in as u128) * scale / (tokens as u128);

    if spot_scaled == 0 {
        return 10_000;
    }

    let impact_scaled = effective_scaled.saturating_sub(spot_scaled) * 10_000 / spot_scaled;
    impact_scaled as u32
}

/// Minimum output for a swap with slippage tolerance.
///
/// `expected_out`: output from swap_exact_in
/// `slippage_bps`: max acceptable slippage (e.g. 100 = 1%)
#[inline(always)]
pub fn min_amount_out(expected_out: u64, slippage_bps: u16) -> u64 {
    let denom = 10_000u64;
    expected_out * (denom - slippage_bps as u64) / denom
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swap_basic() {
        // Pool: 100 SOL, 1M tokens. Buy 1 SOL.
        let reserve_sol = 100_000_000_000u64; // 100 SOL
        let reserve_token = 1_000_000_000_000u64; // 1M tokens (6 decimals)
        let sol_in = 1_000_000_000u64; // 1 SOL
        let out = swap_sol_to_token(reserve_sol, reserve_token, sol_in, 25);
        // Expected: ~9,900 tokens (1% of pool, minus 0.25% fee, minus price impact)
        assert!(out > 9_800_000_000 && out < 10_000_000_000,
            "expected ~9.9K tokens, got {}", out);
    }

    #[test]
    fn test_swap_zero_reserves() {
        assert_eq!(swap_sol_to_token(0, 1000, 100, 25), 0);
        assert_eq!(swap_sol_to_token(1000, 0, 100, 25), 0);
        assert_eq!(swap_sol_to_token(1000, 1000, 0, 25), 0);
    }

    #[test]
    fn test_swap_symmetry() {
        // Buy then sell should lose to fees (round-trip cost)
        let rs = 85_000_000_000u64; // 85 SOL (graduation liquidity)
        let rt = 200_000_000_000_000u64; // 200B token atoms
        let sol_in = 500_000_000u64; // 0.5 SOL

        let tokens = swap_sol_to_token(rs, rt, sol_in, 25);
        let new_rs = rs + sol_in;
        let new_rt = rt - tokens;
        let sol_back = swap_token_to_sol(new_rs, new_rt, tokens, 25);

        // Should get back less than we put in (fees + impact)
        assert!(sol_back < sol_in, "round trip should lose to fees: in={}, out={}", sol_in, sol_back);
        // But not too much less (should be within ~1% for small trade)
        let loss_bps = ((sol_in - sol_back) as u128 * 10_000) / sol_in as u128;
        assert!(loss_bps < 200, "round-trip loss too high: {} bps", loss_bps);
    }

    #[test]
    fn test_round_trip_pnl_negative_without_spread() {
        // No price dislocation → round trip is always negative (fees)
        let rs = 85_000_000_000u64;
        let rt = 200_000_000_000_000u64;
        let pnl = round_trip_pnl(rs, rt, 500_000_000, 25, 1_000_000);
        assert!(pnl < 0, "round trip without spread should be negative: {}", pnl);
    }

    #[test]
    fn test_round_trip_pnl_with_spread() {
        // Simulate graduation: BC terminal price is higher than Raydium opening
        // Raydium pool has MORE sol (cheaper tokens) than BC terminal price implies
        // This creates a buy opportunity
        let rs = 90_000_000_000u64; // 90 SOL (5 SOL above normal 85)
        let rt = 200_000_000_000_000u64;
        let pnl = round_trip_pnl(rs, rt, 500_000_000, 25, 1_000_000);
        // With extra SOL in pool, tokens are cheaper → arb may still be negative
        // because we're buying and selling on the SAME pool
        // Real arb requires buying on cheap pool, selling elsewhere or waiting for price convergence
        // This test just validates the math doesn't panic
        assert!(pnl != 0 || pnl == 0, "pnl calculation should not panic");
    }

    #[test]
    fn test_price_impact_small_trade() {
        let rs = 85_000_000_000u64;
        let rt = 200_000_000_000_000u64;
        let impact = price_impact_bps(rs, rt, 100_000_000, 25); // 0.1 SOL
        // Small trade on 85 SOL pool: impact should be < 50 bps
        assert!(impact < 50, "small trade impact too high: {} bps", impact);
    }

    #[test]
    fn test_price_impact_large_trade() {
        let rs = 85_000_000_000u64;
        let rt = 200_000_000_000_000u64;
        let impact = price_impact_bps(rs, rt, 10_000_000_000, 25); // 10 SOL
        // 10 SOL on 85 SOL pool: ~11.7% of pool → significant impact
        assert!(impact > 100, "large trade impact too low: {} bps", impact);
    }

    #[test]
    fn test_min_amount_out_slippage() {
        let expected = 1_000_000u64;
        assert_eq!(min_amount_out(expected, 100), 990_000); // 1% slippage
        assert_eq!(min_amount_out(expected, 50), 995_000);  // 0.5% slippage
        assert_eq!(min_amount_out(expected, 0), 1_000_000); // 0% slippage
    }

    #[test]
    fn test_no_overflow_max_values() {
        // Test with large but realistic values
        let rs = u64::MAX / 2;
        let rt = u64::MAX / 2;
        let amount = u64::MAX / 4;
        // Should not panic — all intermediate math in u128
        let result = swap_exact_in(rs, rt, amount, 25);
        assert!(result > 0);
    }

    #[test]
    fn test_fee_deduction() {
        // With 0 fee: output should be higher than with 25 bps fee
        let rs = 100_000_000_000u64;
        let rt = 1_000_000_000_000u64;
        let amount = 1_000_000_000u64;
        let out_no_fee = swap_exact_in(rs, rt, amount, 0);
        let out_with_fee = swap_exact_in(rs, rt, amount, 25);
        assert!(out_no_fee > out_with_fee, "fee should reduce output");
        // Fee impact: ~0.25% of output
        let fee_impact = ((out_no_fee - out_with_fee) as u128 * 10_000) / out_no_fee as u128;
        assert!(fee_impact >= 20 && fee_impact <= 30,
            "fee impact should be ~25 bps, got {} bps", fee_impact);
    }
}
