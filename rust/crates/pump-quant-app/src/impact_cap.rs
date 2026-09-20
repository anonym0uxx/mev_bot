//! Our own market impact — the number that replaces a deterministic size clamp.
//!
//! WHY THIS EXISTS INSTEAD OF `x_max`. `gate.rs` bounds an admitted trade with a payout-band
//! clamp (`x_max`) applied at five sites in the engine. That clamp is load-bearing today only
//! because nothing else bounds what OUR OWN leg does to the price: on a thin pool a
//! FULL-sized clip can move the book ~1,000 bp against us before the position is even open,
//! and a bound that is not enforced would make the bot capable of wrecking its own fills.
//!
//! But a payout band is the wrong instrument under the authority split: it decides the SIZE,
//! which is the trading brain's call. What the engine actually needs is the fact — how far our
//! own notional moves the price — and a VETO when that fact is unsafe. Rust resolves and
//! vetoes; it does not substitute its own size for the brain's.
//!
//! THE MODEL. On a constant-product curve the marginal impact of deploying `notional` into a
//! reserve of `vsol` is `notional / vsol` — the same expression `curve_fill::own_impact_bps`
//! uses for the entry fill price, so the veto and the fill model cannot disagree about what a
//! leg costs. It is a lower bound on the full price path (impact is convex in size), and being
//! a lower bound is the correct direction for a safety check only if it is exact at the
//! decision point; it is exact there, because the clip is what we deploy in one leg.

#![forbid(unsafe_code)]

/// The deepest reserve the leg is priced against, in lamports: `vsol` on the bonding curve,
/// the pool's pricing reserve on the AMM. `None` when no depth could be established.
///
/// `None` is NOT zero and NOT a pass: an unpriced book cannot support the claim "this trade is
/// safe for us to take", so it fails closed. The cause is named so the caller can tell an
/// absent observation from an excessive one — they call for different repairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactVeto {
    /// No depth was available, so our own impact is unknown. Fail closed.
    DepthUnknown,
    /// The leg would move the price further than the limit allows.
    TooLarge {
        /// Our own impact, bp, as charged by the fill model.
        impact_bps: u64,
        /// The limit in force.
        max_bps: u64,
    },
}

impl ImpactVeto {
    /// Stable label for journals and dashboards — never reword.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ImpactVeto::DepthUnknown => "own_impact_unpriced",
            ImpactVeto::TooLarge { .. } => "own_impact_too_large",
        }
    }
}

/// Price our own impact and veto the leg if it is unsafe.
///
/// Returns the impact in basis points when the leg may proceed, so the caller can journal the
/// number even on the accepted path — a trade that only just cleared the limit is the signal
/// that the limit is about to bind.
pub fn own_impact_veto(
    depth_lamports: Option<u64>,
    clip_lamports: u64,
    max_impact_bps: u64,
) -> Result<u64, ImpactVeto> {
    // A zero clip has no impact by construction: nothing is deployed, so nothing moves —
    // which is why this check comes FIRST, before the book is consulted at all. It cannot
    // move capital, so it needs no price, and an unpriced book must not turn "nothing" into a
    // refusal.
    if clip_lamports == 0 {
        return Ok(0);
    }
    let Some(depth) = depth_lamports else {
        return Err(ImpactVeto::DepthUnknown);
    };
    if depth == 0 {
        return Err(ImpactVeto::DepthUnknown);
    }
    let impact_bps = match crate::curve_fill::own_impact_bps(depth, clip_lamports) {
        Some(bp) => bp,
        // Overflow on the bp conversion means the leg is larger than any priceable book;
        // that is a refusal, not a wrap-around.
        None => {
            return Err(ImpactVeto::TooLarge {
                impact_bps: u64::MAX,
                max_bps: max_impact_bps,
            })
        }
    };
    if impact_bps > max_impact_bps {
        return Err(ImpactVeto::TooLarge {
            impact_bps,
            max_bps: max_impact_bps,
        });
    }
    Ok(impact_bps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leg_within_the_limit_reports_its_impact() {
        // 1 SOL into a 100 SOL book is 100 bp = exactly the limit.
        let bp = own_impact_veto(Some(100_000_000_000), 1_000_000_000, 100).expect("at the limit");
        assert_eq!(bp, 100);
    }

    #[test]
    fn a_leg_past_the_limit_is_vetoed_with_both_numbers() {
        // 2 SOL into 100 SOL is 200 bp, over a 100 bp limit.
        let veto = own_impact_veto(Some(100_000_000_000), 2_000_000_000, 100).unwrap_err();
        assert_eq!(
            veto,
            ImpactVeto::TooLarge {
                impact_bps: 200,
                max_bps: 100
            }
        );
        assert_eq!(veto.as_str(), "own_impact_too_large");
    }

    #[test]
    fn an_unpriced_book_fails_closed_and_is_not_zero_impact() {
        let veto = own_impact_veto(None, 1_000_000_000, 1_000).unwrap_err();
        assert_eq!(veto, ImpactVeto::DepthUnknown);
        assert_eq!(veto.as_str(), "own_impact_unpriced");
        // A zero depth is the same refusal: we cannot claim safety from an empty reserve.
        assert_eq!(
            own_impact_veto(Some(0), 1_000_000_000, 1_000).unwrap_err(),
            ImpactVeto::DepthUnknown
        );
        // ...but a zero clip deploys nothing and moves nothing.
        assert_eq!(own_impact_veto(None, 0, 1_000), Ok(0));
    }

    #[test]
    fn the_veto_agrees_with_the_fill_model_it_prices_against() {
        // The veto must not invent its own impact arithmetic: for the same (depth, notional)
        // the numbers are the same object.
        let (depth, notional) = (30_000_000_000u64, 750_000_000u64);
        let veto_bp = own_impact_veto(Some(depth), notional, u64::MAX).expect("under limit");
        assert_eq!(
            veto_bp,
            crate::curve_fill::own_impact_bps(depth, notional).unwrap()
        );
    }
}
