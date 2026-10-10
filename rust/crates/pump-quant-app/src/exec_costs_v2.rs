//! Prompt-contract v2 execution costs (SCOPE_ENG_R5 W2 F2), derived from the SAME slot-aware fee authority that
//! quotes and paper-executes the order ([`pump_quant_protocol::fee_schedule`], selected by [`crate::exec_quote::FeeCtx`]).
//! No fee number lives here: the curve rate is the curve config in force at the decision slot; the pool rate is
//! the PumpSwap tier for the pool's market cap at that slot, taken CONSERVATIVELY near a tier threshold (the higher
//! of the tiers within [`AMM_TIER_BOUNDARY_BAND_BPS`]), because the program's market cap can sit across a
//! threshold from ours (measured: proc/fees_amm_boundary_cW6r0.json — 535/49,181 events charged the next-higher
//! tier, p99 4.5% from the threshold, always higher, never lower).
#![forbid(unsafe_code)]

use pump_quant_proposal::contract_v2::ExecCosts;
use pump_quant_protocol::fee_schedule::{self, FeeStamp};

use crate::exec_quote::{FeeCtx, QuoteRefusal, NETWORK_FEE_P50_LAMPORTS};

/// Relative band around a PumpSwap tier threshold inside which the higher-fee neighbour is stated, bp of the cap.
pub const AMM_TIER_BOUNDARY_BAND_BPS: u128 = 500;

/// Which market the order lands on, with the reserves the fee tier is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMarket {
    /// Bonding curve at (virtual SOL, virtual tokens).
    Curve { vsol: u64, vtok: u64 },
    /// Canonical PumpSwap pool: market cap in lamports (effective quote x supply / base).
    Pool { mcap_lamports: u128 },
}

fn pool_rate(mcap: u128, ctx: FeeCtx) -> Result<(u64, FeeStamp), QuoteRefusal> {
    let at = |m: u128| {
        fee_schedule::pool_rates_at_market_cap(m, ctx.slot, ctx.sel)
            .map_err(QuoteRefusal::FeeSchedule)
    };
    let band = mcap / 10_000 * AMM_TIER_BOUNDARY_BAND_BPS;
    let mut best = at(mcap)?;
    for m in [mcap.saturating_sub(band), mcap.saturating_add(band)] {
        let r = at(m)?;
        if r.0.total_bps() > best.0.total_bps() {
            best = r;
        }
    }
    Ok((best.0.total_bps(), best.1))
}

/// The v2 per-row execution costs for an order on `market` at `ctx`, with the configured tip and the
/// unrefunded token-account rent. A slot with no known configuration is a named refusal (the v2 prompt is then
/// not cut), never today's rate.
///
/// # Errors
/// [`QuoteRefusal::FeeSchedule`].
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
        ExecMarket::Pool { mcap_lamports } => pool_rate(mcap_lamports, ctx)?,
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
        let r = exec_costs(
            ExecMarket::Pool {
                mcap_lamports: 30_000 * SOL,
            },
            FeeCtx::by_slot(441_327_828),
            0,
            0,
        );
        assert!(matches!(r, Err(QuoteRefusal::FeeSchedule(_))), "{r:?}");
    }

    #[test]
    fn pool_tier_crossing_and_conservative_boundary() {
        let at = |sol: u128| {
            exec_costs(
                ExecMarket::Pool {
                    mcap_lamports: sol * SOL,
                },
                FeeCtx::new(454_754_100, FeeSelection::BySlot),
                0,
                0,
            )
            .unwrap()
            .venue_bp_per_side
        };
        // Deep inside tiers: 29,470-34,380 SOL = 75 bp; 34,380-39,300 = 70 bp.
        assert_eq!(at(31_900), 75);
        assert_eq!(at(37_000), 70);
        // 2% above the 34,380 threshold: the higher neighbour (75) is stated, never the cheaper 70.
        assert_eq!(at(35_000), 75);
        // Below the first threshold (420 SOL): 125.
        assert_eq!(at(100), 125);
    }
}
