//! Pump.fun constant-product AMM bonding curve simulation.
//!
//! k = vSol * vTokens (invariant, maintained across swaps).
//! All values in lamports (SOL) and raw token units.
//! Uses u128 intermediate math to avoid overflow on the k product.

/// Result of simulating a buy on the bonding curve.
#[derive(Debug, Clone, Copy)]
pub struct BuyResult {
    /// Tokens received from the AMM.
    pub tokens_out: u64,
    /// New virtual SOL reserves after the swap.
    pub new_vsol: u64,
    /// New virtual token reserves after the swap.
    pub new_vtokens: u64,
    /// Price impact in basis points (10000 = 100%).
    pub price_impact_bps: u32,
    /// Minimum tokens out after applying slippage tolerance.
    pub min_tokens_out: u64,
}

/// Result of simulating a sell on the bonding curve.
#[derive(Debug, Clone, Copy)]
pub struct SellResult {
    /// SOL received (lamports), after 1% pump.fun fee.
    pub sol_out: u64,
    /// New virtual SOL reserves after the swap.
    pub new_vsol: u64,
    /// New virtual token reserves after the swap.
    pub new_vtokens: u64,
    /// Price impact in basis points.
    pub price_impact_bps: u32,
}

/// Simulate a pump.fun buy.
///
/// Fee model: pump.fun charges 1% on the SOL input *before* AMM swap.
///
/// # Arguments
/// * `vsol` — current virtual SOL reserves (lamports)
/// * `vtokens` — current virtual token reserves
/// * `sol_in` — total SOL the buyer sends (lamports), fee is deducted from this
/// * `slippage_bps` — slippage tolerance in basis points (e.g. 100 = 1%)
pub fn simulate_buy(vsol: u64, vtokens: u64, sol_in: u64, slippage_bps: u32) -> BuyResult {
    // 1% fee deducted before swap
    let fee = sol_in / 100;
    let sol_after_fee = sol_in - fee;

    // Constant product: k = vsol * vtokens
    let k = (vsol as u128) * (vtokens as u128);

    // New vSol after adding buyer's SOL (post-fee)
    let new_vsol = vsol + sol_after_fee;

    // new_vtokens = k / new_vsol (round up to be conservative — fewer tokens out)
    let new_vtokens_128 = (k + (new_vsol as u128) - 1) / (new_vsol as u128);
    let new_vtokens = new_vtokens_128 as u64;

    // Tokens out = old vtokens - new vtokens
    let tokens_out = vtokens.saturating_sub(new_vtokens);

    // Price impact: how much worse than the spot price did we get?
    // Spot price = vsol / vtokens (SOL per token)
    // Effective price = sol_after_fee / tokens_out
    // Impact = (effective - spot) / spot
    // In bps: impact_bps = ((sol_after_fee * vtokens) / (tokens_out * vsol) - 1) * 10000
    let price_impact_bps = if tokens_out > 0 && vsol > 0 {
        // Numerator: sol_after_fee * vtokens (what we paid * original token reserve)
        // Denominator: tokens_out * vsol (what we got * original sol reserve)
        let num = (sol_after_fee as u128) * (vtokens as u128) * 10_000;
        let den = (tokens_out as u128) * (vsol as u128);
        if den > 0 {
            let ratio = num / den;
            // ratio is impact_ratio * 10000; impact_bps = ratio - 10000
            ratio.saturating_sub(10_000) as u32
        } else {
            0
        }
    } else {
        0
    };

    // Minimum tokens out with slippage tolerance
    let min_tokens_out = if slippage_bps >= 10_000 {
        0 // 100%+ slippage = accept anything
    } else {
        // min = tokens_out * (10000 - slippage_bps) / 10000
        ((tokens_out as u128) * ((10_000 - slippage_bps) as u128) / 10_000) as u64
    };

    BuyResult {
        tokens_out,
        new_vsol,
        new_vtokens,
        price_impact_bps,
        min_tokens_out,
    }
}

/// Simulate a pump.fun sell.
///
/// Fee model: pump.fun charges 1% on the SOL output *after* AMM swap.
///
/// # Arguments
/// * `vsol` — current virtual SOL reserves (lamports)
/// * `vtokens` — current virtual token reserves
/// * `tokens_in` — tokens being sold
pub fn simulate_sell(vsol: u64, vtokens: u64, tokens_in: u64) -> SellResult {
    // Constant product: k = vsol * vtokens
    let k = (vsol as u128) * (vtokens as u128);

    // New vtokens after adding seller's tokens
    let new_vtokens = vtokens + tokens_in;

    // new_vsol = k / new_vtokens (round up — means less SOL out, conservative)
    let new_vsol_128 = (k + (new_vtokens as u128) - 1) / (new_vtokens as u128);
    let new_vsol = new_vsol_128 as u64;

    // Gross SOL out (before pump fee)
    let sol_out_gross = vsol.saturating_sub(new_vsol);

    // 1% pump fee on output
    let fee = sol_out_gross / 100;
    let sol_out = sol_out_gross - fee;

    // Price impact (same concept, sell direction)
    // Spot price (SOL per token) = vsol / vtokens
    // Effective price = sol_out_gross / tokens_in  (pre-fee to isolate AMM impact)
    let price_impact_bps = if tokens_in > 0 && vtokens > 0 {
        // spot gives us: tokens_in * vsol / vtokens SOL for a marginal trade
        // impact = 1 - (effective / spot)
        let spot_sol = (tokens_in as u128) * (vsol as u128) / (vtokens as u128);
        if spot_sol > 0 {
            let impact = spot_sol.saturating_sub(sol_out_gross as u128) * 10_000 / spot_sol;
            impact as u32
        } else {
            0
        }
    } else {
        0
    };

    SellResult {
        sol_out,
        new_vsol,
        new_vtokens,
        price_impact_bps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buy_basic() {
        // Initial reserves: 30 SOL, 1B tokens (typical pump.fun launch)
        let vsol = 30_000_000_000u64; // 30 SOL in lamports
        let vtokens = 1_000_000_000_000_000u64; // 1B tokens (6 decimals)
        let sol_in = 100_000_000u64; // 0.1 SOL

        let result = simulate_buy(vsol, vtokens, sol_in, 100);

        assert!(result.tokens_out > 0);
        assert!(result.new_vsol > vsol);
        assert!(result.new_vtokens < vtokens);
        assert_eq!(result.new_vsol, vsol + sol_in - sol_in / 100);
        // k should be approximately preserved
        let k_before = (vsol as u128) * (vtokens as u128);
        let k_after = (result.new_vsol as u128) * (result.new_vtokens as u128);
        // k_after >= k_before due to rounding up new_vtokens
        assert!(k_after >= k_before);
        assert!(result.min_tokens_out <= result.tokens_out);
    }

    #[test]
    fn test_sell_basic() {
        let vsol = 30_000_000_000u64;
        let vtokens = 1_000_000_000_000_000u64;
        let tokens_in = 1_000_000_000_000u64; // 1M tokens

        let result = simulate_sell(vsol, vtokens, tokens_in);

        assert!(result.sol_out > 0);
        assert!(result.new_vsol < vsol);
        assert!(result.new_vtokens > vtokens);
    }

    #[test]
    fn test_buy_sell_roundtrip_loses_money() {
        // Buy then sell should lose money (fees + price impact)
        let vsol = 30_000_000_000u64;
        let vtokens = 1_000_000_000_000_000u64;
        let sol_in = 100_000_000u64;

        let buy = simulate_buy(vsol, vtokens, sol_in, 0);
        let sell = simulate_sell(buy.new_vsol, buy.new_vtokens, buy.tokens_out);

        // Should get back less than we put in
        assert!(sell.sol_out < sol_in);
    }

    #[test]
    fn test_zero_input() {
        let vsol = 30_000_000_000u64;
        let vtokens = 1_000_000_000_000_000u64;

        let buy = simulate_buy(vsol, vtokens, 0, 100);
        assert_eq!(buy.tokens_out, 0);
        assert_eq!(buy.new_vsol, vsol);

        let sell = simulate_sell(vsol, vtokens, 0);
        assert_eq!(sell.sol_out, 0);
        assert_eq!(sell.new_vtokens, vtokens);
    }
}
