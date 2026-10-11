//! FEES R10 F5: the PumpSwap fee tier of OUR shadow pool state.
//!
//! Under `paper_fill_v2_shadow` our fill is priced on `observed reserves + our carried delta`. The landing event's
//! rates are the tier the program charged on the OBSERVED state. If our carried trades move the pool market cap
//! across a tier threshold, the observed rate is not the rate our fill would pay: the tier must come from OUR
//! shadow state.
//!
//! The program's tier is `poolMarketCap = floor((quote + virtual quote) x live supply / base)` (pump-swap-sdk 1.20.0).
//! * Supply evidenced (`Some`): the shadow market cap is computed and its tier priced; the observed event must be
//!   consistent with the same supply (else `shadow_fee_tier_undetermined`).
//! * Supply NOT evidenced (`None`, the engine today): the event's tier bounds the observed market cap to an
//!   interval; the shadow market cap is that interval scaled by the price ratio shadow/observed (supply cancels).
//!   If every tier the scaled interval touches charges the same rates, those rates are priced; otherwise the tier
//!   cannot be determined from evidence and the fill is refused by name `shadow_fee_tier_undetermined`.
//!   Never 1e15, never the observed rate when the shadow tier may differ.
#![forbid(unsafe_code)]

use pump_quant_protocol::fee_schedule::{self, FeeVenue};
use pump_quant_protocol::pumpswap_event::CashbackField;
use pump_quant_protocol::pumpswap_fees::FeeTier;

use crate::exec_quote::{FeeCtx, QuoteRefusal};

/// Pre-trade pool state: (base reserve, quote vault).
pub type PoolState = (u64, u64);
/// Event-shaped rates: `(lp, protocol, coin_creator)` bp and the cashback field.
pub type EventRates = (Option<(u32, u32, u32)>, CashbackField);

/// The rates OUR fill on `shadow` pays, as event-shaped `(parts, cashback)` (a tier of the config in force), from
/// the landing event's `parts`/`cashback` charged on `observed`. Identical states return the event's rates.
///
/// # Errors
/// [`QuoteRefusal::ShadowFeeTierUndetermined`], or the refusals of the event-rate validation.
pub fn shadow_pool_rates(
    ctx: FeeCtx,
    observed: PoolState,
    shadow: PoolState,
    vq: u64,
    parts: Option<(u32, u32, u32)>,
    cashback: CashbackField,
    supply: Option<u128>,
) -> Result<EventRates, QuoteRefusal> {
    // The event's own rates must be a tier first (same refusal order as the fill path).
    crate::exec_quote::amm_rates_for(ctx, parts, cashback)?;
    if observed == shadow {
        return Ok((parts, cashback));
    }
    let (lp, pr, cr) = parts.ok_or(QuoteRefusal::AmmEconomicsMissing)?;
    let cb_mode = cr == 0 && cashback.known().is_some_and(|(b, _)| b > 0);
    let ev_creator = if cb_mode {
        cashback.known().map_or(0, |(b, _)| b)
    } else {
        u64::from(cr)
    };
    let cif = fee_schedule::config_in_force(FeeVenue::PumpSwapCanonical, ctx.slot, ctx.sel)
        .map_err(QuoteRefusal::FeeSchedule)?;
    let tiers = &cif.config.fee_tiers;
    let same = |t: &FeeTier| {
        (t.fees.lp_bps, t.fees.protocol_bps, t.fees.creator_bps)
            == (u64::from(lp), u64::from(pr), ev_creator)
    };
    let und = QuoteRefusal::ShadowFeeTierUndetermined;
    let eff = |q: u64| u128::from(q) + u128::from(vq);
    let (ob, oq) = observed;
    let (sb, sq) = shadow;
    if ob == 0 || sb == 0 {
        return Err(und);
    }
    let pick: &FeeTier = if let Some(s) = supply {
        let mcap = |b: u64, q: u64| eff(q).checked_mul(s).map(|x| x / u128::from(b));
        let (om, sm) = (mcap(ob, oq).ok_or(und)?, mcap(sb, sq).ok_or(und)?);
        if !same(&tiers[tier_at(tiers, om)]) {
            return Err(und); // the supply does not reproduce the observed event's tier
        }
        &tiers[tier_at(tiers, sm)]
    } else {
        // Observed mcap interval [lo, hi) = hull of the tiers charging the event's rates.
        let idx: Vec<usize> = (0..tiers.len()).filter(|&i| same(&tiers[i])).collect();
        let (first, last) = (*idx.first().ok_or(und)?, *idx.last().ok_or(und)?);
        let lo = if first == 0 {
            0
        } else {
            tiers[first].threshold
        };
        let hi = tiers.get(last + 1).map(|t| t.threshold);
        // None = unbounded. shadow = observed x (eff(sq) x ob) / (eff(oq) x sb): floor the low end, ceil the high.
        let num = eff(sq).checked_mul(u128::from(ob)).ok_or(und)?;
        let den = eff(oq).checked_mul(u128::from(sb)).ok_or(und)?;
        let slo = lo.checked_mul(num).ok_or(und)? / den;
        let shi = match hi {
            Some(h) => Some(h.checked_mul(num).ok_or(und)?.div_ceil(den)),
            None => None,
        };
        // Highest mcap the interval can hold (exclusive upper bound -> shi - 1).
        let a = tier_at(tiers, slo);
        let b = shi.map_or(tiers.len() - 1, |h| tier_at(tiers, h.saturating_sub(1)));
        let f0 = tiers[a].fees;
        if (a..=b).any(|i| tiers[i].fees != f0) {
            return Err(und);
        }
        &tiers[a]
    };
    let f = pick.fees;
    let to_u32 = |x: u64| u32::try_from(x).map_err(|_| QuoteRefusal::Unpriceable);
    if cb_mode {
        let cb = match cashback {
            CashbackField::Known {
                lamports,
                layout_len,
                ..
            } => CashbackField::Known {
                bps: f.creator_bps,
                lamports,
                layout_len,
            },
            other => other,
        };
        Ok((Some((to_u32(f.lp_bps)?, to_u32(f.protocol_bps)?, 0)), cb))
    } else {
        Ok((
            Some((
                to_u32(f.lp_bps)?,
                to_u32(f.protocol_bps)?,
                to_u32(f.creator_bps)?,
            )),
            cashback,
        ))
    }
}

/// `calculateFeeTier`: below the first threshold -> 0; else the highest tier whose threshold <= mcap.
fn tier_at(tiers: &[FeeTier], mcap: u128) -> usize {
    tiers.iter().rposition(|t| mcap >= t.threshold).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_protocol::fee_schedule::FeeSelection;

    const SLOT: u64 = 454_754_100;
    const SOL: u128 = 1_000_000_000;
    const FULL: u128 = 1_000_000_000_000_000;
    const CB_NONE: CashbackField = CashbackField::NotRecorded;

    fn ctx() -> FeeCtx {
        FeeCtx::new(SLOT, FeeSelection::BySlot)
    }
    /// Event parts of the tier the schedule selects at `mcap` (independent read of the authority).
    fn parts_at(mcap: u128) -> (u32, u32, u32) {
        let (r, _) =
            fee_schedule::pool_rates_at_market_cap(mcap, SLOT, FeeSelection::BySlot).unwrap();
        (
            u32::try_from(r.fees.lp_bps).unwrap(),
            u32::try_from(r.fees.protocol_bps).unwrap(),
            u32::try_from(r.fees.creator_bps).unwrap(),
        )
    }
    /// A pool with base = 1e15 at supply 1e15 has mcap = quote + vq exactly (vq = 0 here).
    fn pool(mcap: u128) -> PoolState {
        (u64::try_from(FULL).unwrap(), u64::try_from(mcap).unwrap())
    }

    /// Threshold 34,380 SOL (tier 10 75 bp below, tier 11 70 bp at/above). Supply evidenced: observed just below,
    /// our shadow EXACTLY at the threshold -> tier 11 (OUR state), not the observed event's tier 10.
    #[test]
    fn f5_shadow_exactly_at_a_threshold_takes_our_tier_not_the_observed_rate() {
        let th = 34_380 * SOL;
        let below = parts_at(th - 1);
        let at = parts_at(th);
        assert_ne!(below, at, "a real threshold");
        let obs = pool(th - 10 * SOL);
        let got = |shadow_mcap: u128| {
            shadow_pool_rates(
                ctx(),
                obs,
                pool(shadow_mcap),
                0,
                Some(below),
                CB_NONE,
                Some(FULL),
            )
            .map(|x| x.0)
        };
        assert_eq!(got(th), Ok(Some(at)), "shadow at the threshold: our tier");
        assert_eq!(
            got(th - 1),
            Ok(Some(below)),
            "one lamport below: the observed tier"
        );
        assert_eq!(got(th + 1), Ok(Some(at)));
        // Identical states: the event's rates, untouched.
        assert_eq!(
            shadow_pool_rates(ctx(), obs, obs, 0, Some(below), CB_NONE, None).map(|x| x.0),
            Ok(Some(below))
        );
    }

    /// Supply NOT evidenced: the event's tier only bounds the observed mcap; a shadow move that can reach the next
    /// tier is refused by name, never priced at the observed rate.
    #[test]
    fn f5_unknown_supply_tier_crossing_possible_is_refused_by_name() {
        let th = 34_380 * SOL;
        let below = parts_at(th - 1);
        let obs = pool(th - 10 * SOL);
        let r = shadow_pool_rates(ctx(), obs, pool(th), 0, Some(below), CB_NONE, None);
        assert_eq!(r, Err(QuoteRefusal::ShadowFeeTierUndetermined));
        assert_eq!(
            QuoteRefusal::ShadowFeeTierUndetermined.label(),
            "quote_unavailable:shadow_fee_tier_undetermined"
        );
    }

    /// Supply evidenced but inconsistent with the observed event's tier: refused by name (the supply is wrong).
    #[test]
    fn f5_supply_that_does_not_reproduce_the_observed_tier_is_refused() {
        let th = 34_380 * SOL;
        let at = parts_at(th);
        let obs = pool(th - 10 * SOL); // at supply 1e15 this is tier 10, but the event says tier 11
        let r = shadow_pool_rates(ctx(), obs, pool(th + SOL), 0, Some(at), CB_NONE, Some(FULL));
        assert_eq!(r, Err(QuoteRefusal::ShadowFeeTierUndetermined));
    }
}
