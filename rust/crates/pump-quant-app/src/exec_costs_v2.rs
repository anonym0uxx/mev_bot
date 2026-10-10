//! Prompt-contract v2 execution costs (SCOPE_ENG_R5 W2 F2; corrected SCOPE_FEES_R6 T2), derived from the SAME
//! slot-aware fee authority that quotes and paper-executes the order ([`pump_quant_protocol::fee_schedule`],
//! selected by [`crate::exec_quote::FeeCtx`]). No fee number lives here.
//!
//! * Curve: the curve config in force at the decision slot, tier by `bondingCurveMarketCap` (one tier today).
//! * Pool: the PROTOCOL tier, selected exactly as the pinned PumpSwap program does (pump-swap-sdk 1.20.0
//!   `computeFeesBps` -> `poolMarketCap` -> `calculateFeeTier`): market cap = floor((quote vault + virtual quote) x
//!   LIVE base-mint supply / base reserve), lamports; tier = the highest tier whose threshold <= that cap (below the
//!   first threshold: tier 0). A higher market cap selects a LOWER fee rate.
//!
//! R6 T2: the earlier "+-5% band" (state the higher-FEE-RATE neighbour near a threshold) was OUR estimate, not a
//! protocol rule. Its measured cause was the 1e15 supply assumption (live supply < 1e15 lowers the program's cap,
//! proc/fees_r6_t1_supply.json). It is REMOVED: the stated fee is the protocol fee from the live supply, and an
//! unknown supply is a named refusal (never 1e15, never a band). No stress allowance is stated in the prompt.
#![forbid(unsafe_code)]

use pump_quant_proposal::contract_v2::ExecCosts;
use pump_quant_protocol::fee_schedule::{self, FeeScheduleRefusal, FeeStamp};

use crate::exec_quote::{FeeCtx, QuoteRefusal, NETWORK_FEE_P50_LAMPORTS};

/// Which market the order lands on, with the verified state the fee tier is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMarket {
    /// Bonding curve at (virtual SOL, virtual tokens).
    Curve { vsol: u64, vtok: u64 },
    /// Canonical PumpSwap pool, pre-trade: `effective_quote` = quote vault + virtual quote reserves (lamports),
    /// `base_reserve` (raw), and the LIVE base-mint supply read from the mint account (`None` = not read).
    Pool {
        effective_quote: u128,
        base_reserve: u128,
        mint_supply: Option<u128>,
    },
}

/// `poolMarketCap`: floor(effective_quote x supply / base_reserve), lamports. Unknown supply / zero base / overflow
/// are named refusals.
///
/// # Errors
/// [`QuoteRefusal::PoolMintSupplyUnknown`], [`QuoteRefusal::FeeSchedule`].
pub fn pool_market_cap(
    effective_quote: u128,
    base_reserve: u128,
    mint_supply: Option<u128>,
) -> Result<u128, QuoteRefusal> {
    let supply = mint_supply.ok_or(QuoteRefusal::PoolMintSupplyUnknown)?;
    if base_reserve == 0 {
        return Err(QuoteRefusal::FeeSchedule(
            FeeScheduleRefusal::MarketCapUndefined,
        ));
    }
    effective_quote
        .checked_mul(supply)
        .map(|x| x / base_reserve)
        .ok_or(QuoteRefusal::FeeSchedule(FeeScheduleRefusal::Overflow))
}

fn pool_rate(mcap: u128, ctx: FeeCtx) -> Result<(u64, FeeStamp), QuoteRefusal> {
    let (r, s) = fee_schedule::pool_rates_at_market_cap(mcap, ctx.slot, ctx.sel)
        .map_err(QuoteRefusal::FeeSchedule)?;
    Ok((r.total_bps(), s))
}

/// The v2 per-row execution costs for an order on `market` at `ctx`, with the configured tip and the
/// unrefunded token-account rent. A slot with no known configuration, or a pool whose live supply was not read,
/// is a named refusal (the v2 prompt is then not cut), never today's rate and never an assumed supply.
///
/// # Errors
/// [`QuoteRefusal::FeeSchedule`], [`QuoteRefusal::PoolMintSupplyUnknown`].
pub fn exec_costs(
    market: ExecMarket,
    ctx: FeeCtx,
    tip_lamports: u64,
    rent_unrefunded_lamports: u64,
) -> Result<ExecCosts, QuoteRefusal> {
    let (bp, stamp) = match market {
        ExecMarket::Curve { vsol, vtok } => {
            let (r, s) = fee_schedule::curve_rates(vsol, vtok, ctx.slot, ctx.sel)
                .map_err(QuoteRefusal::FeeSchedule)?;
            (r.total_bps(), s)
        }
        ExecMarket::Pool {
            effective_quote,
            base_reserve,
            mint_supply,
        } => pool_rate(
            pool_market_cap(effective_quote, base_reserve, mint_supply)?,
            ctx,
        )?,
    };
    Ok(ExecCosts {
        venue_bp_per_side: bp,
        fee_stamp: stamp.label(),
        network_lamports_per_leg: NETWORK_FEE_P50_LAMPORTS,
        tip_lamports_per_leg: tip_lamports,
        rent_unrefunded_lamports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_protocol::fee_schedule::FeeSelection;

    const SOL: u128 = 1_000_000_000;
    const FULL: u128 = 1_000_000_000_000_000;

    /// A pool whose program market cap is exactly `mcap` lamports at supply 1e15 (base 1e15 => cap = quote).
    fn at_cap(mcap: u128) -> ExecMarket {
        ExecMarket::Pool {
            effective_quote: mcap,
            base_reserve: FULL,
            mint_supply: Some(FULL),
        }
    }

    fn bp(m: ExecMarket) -> Result<u64, QuoteRefusal> {
        exec_costs(m, FeeCtx::new(454_754_100, FeeSelection::BySlot), 0, 0)
            .map(|c| c.venue_bp_per_side)
    }

    #[test]
    fn curve_states_the_config_in_force_at_the_slot() {
        let c = exec_costs(
            ExecMarket::Curve {
                vsol: 32_000_000_000,
                vtok: 1_000_000_000_000_000,
            },
            FeeCtx::by_slot(454_754_100),
            10_000,
            5_000,
        )
        .unwrap();
        assert_eq!(c.venue_bp_per_side, 125);
        assert_eq!(c.fee_stamp, "curve@454754100:read_at_slot:tier0:125bp");
    }

    #[test]
    fn historical_slot_without_config_refuses_and_never_states_todays_rate() {
        let r = exec_costs(at_cap(30_000 * SOL), FeeCtx::by_slot(441_327_828), 0, 0);
        assert!(matches!(r, Err(QuoteRefusal::FeeSchedule(_))), "{r:?}");
    }

    /// R6 T3: the real tier-10/11 threshold of the pinned config (34,380 SOL: 20/5/50 = 75 bp below, 20/5/45 = 70 bp
    /// at and above). Immediately below / exactly at / immediately above, plus the point inside the old +-5% band
    /// where the band stated 75 and the protocol charges 70.
    #[test]
    fn protocol_tier_at_a_real_threshold_below_at_above_and_inside_the_old_band() {
        let th = 34_380 * SOL;
        assert_eq!(bp(at_cap(th - 1)), Ok(75));
        assert_eq!(bp(at_cap(th)), Ok(70));
        assert_eq!(bp(at_cap(th + 1)), Ok(70));
        // 35,000 SOL is 1.8% above the threshold: the old band stated 75; the protocol fee is 70.
        assert_eq!(bp(at_cap(35_000 * SOL)), Ok(70));
        // Deep inside tiers, and below the first threshold (420 SOL).
        assert_eq!(bp(at_cap(31_900 * SOL)), Ok(75));
        assert_eq!(bp(at_cap(37_000 * SOL)), Ok(70));
        assert_eq!(bp(at_cap(100 * SOL)), Ok(125));
    }

    /// R6 T2: the cap uses the LIVE supply. The same reserves at 1e15 sit above the threshold (70 bp); with a live
    /// supply of 0.98e15 the program's cap is below it and charges 75 bp — the measured cause of the old band.
    #[test]
    fn the_pool_cap_uses_the_live_supply_and_unknown_supply_refuses() {
        let (q, b) = (35_000 * SOL, FULL);
        let at = |s: Option<u128>| {
            bp(ExecMarket::Pool {
                effective_quote: q,
                base_reserve: b,
                mint_supply: s,
            })
        };
        assert_eq!(at(Some(FULL)), Ok(70));
        assert_eq!(at(Some(FULL / 100 * 98)), Ok(75));
        assert_eq!(at(None), Err(QuoteRefusal::PoolMintSupplyUnknown));
        assert_eq!(
            pool_market_cap(q, 0, Some(FULL)),
            Err(QuoteRefusal::FeeSchedule(
                FeeScheduleRefusal::MarketCapUndefined
            ))
        );
        // floor, as BN.div: 7 x 3 / 2 = 10.
        assert_eq!(pool_market_cap(7, 2, Some(3)), Ok(10));
    }
}
