//! The venue axis — TWO distinct notions that must never be conflated.
//!
//! The decision bundle states both, and they answer different questions:
//!
//! 1. **Observed venue** (`venue=` in the prompt) — where the mint's *last 200 trades* actually
//!    landed, as a set: `pumpfun`, `pumpswap`, or `mixed` when both appear. The corpus builds
//!    exactly this way (`build_states_v2._episode`: a set over the trailing 200 trades, and
//!    `mixed` when it holds more than one value). It is a *backward* observation of the tape,
//!    and it is computed by the state ledger ([`crate::state_ledger::StateSnapshot::venue`]).
//!
//! 2. **Sizing venue** ([`EntryVenue`]) — where OUR NEXT FILL would land. This is a *forward*
//!    judgement, and the ruled size rule reads it (`KELLY_AUDIT_C12`: AMM BUY → FULL,
//!    bonding-curve BUY → SMALL). It is derived from the virtual SOL reserves crossing
//!    graduation, using **the same constant the cost authority switches on**
//!    ([`crate::curve_state::GRADUATION_VSOL_LAMPORTS`]) so the size rule and the fee can never
//!    disagree about which venue a candidate is on — the two would otherwise drift apart
//!    silently, and a SMALL size charged an AMM fee is exactly that kind of drift.
//!
//! A graduated mint whose recent tape also contains curve prints is legitimately "observed
//! pumpfun, sized as AMM". That is not an inconsistency; it is the two questions having
//! different answers, and there is a test for it.

use pump_quant_inference::EntryVenue;

/// The venue our next fill would land on, from the market's virtual SOL reserves.
///
/// Delegates the boundary to the cost authority's own constant: this function must never
/// carry a threshold of its own.
#[must_use]
pub fn entry_venue_from_reserves(vsol_lamports: u64) -> EntryVenue {
    if vsol_lamports >= crate::curve_state::GRADUATION_VSOL_LAMPORTS {
        EntryVenue::Amm
    } else {
        EntryVenue::BondingCurve
    }
}

/// Whether our next fill lands on the AMM — the bundle's `size_amm` flag, which decides which
/// regime the SIZE OPTIONS cost block is measured against.
#[must_use]
pub fn sized_on_amm(venue: EntryVenue) -> bool {
    matches!(venue, EntryVenue::Amm)
}

/// The corpus's label for a *single* venue observation (`pumpfun` / `pumpswap`), as
/// [`crate::state_ledger::StateSnapshot::venue`] renders it after folding its window.
#[must_use]
pub fn venue_label(venue: EntryVenue) -> &'static str {
    match venue {
        EntryVenue::Amm => "pumpswap",
        EntryVenue::BondingCurve => "pumpfun",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost_model::{
        venue_fee_bps_per_leg, VENUE_FEE_BPS_CURVE, VENUE_FEE_BPS_POST_GRADUATION,
    };
    use crate::curve_state::GRADUATION_VSOL_LAMPORTS;

    /// The drift guard: the size rule's venue and the cost authority's fee must flip at the
    /// same depth, in the same direction, for every depth around the boundary.
    #[test]
    fn the_sizing_venue_flips_exactly_where_the_fee_does() {
        let boundary = GRADUATION_VSOL_LAMPORTS;
        for vsol in [
            boundary / 4,
            boundary - 2,
            boundary - 1,
            boundary,
            boundary + 1,
            boundary * 4,
        ] {
            let venue = entry_venue_from_reserves(vsol);
            let fee = venue_fee_bps_per_leg(vsol);
            let expect_fee = match venue {
                EntryVenue::Amm => VENUE_FEE_BPS_POST_GRADUATION,
                EntryVenue::BondingCurve => VENUE_FEE_BPS_CURVE,
            };
            assert_eq!(
                fee, expect_fee,
                "venue {venue:?} and fee disagree at vsol {vsol}"
            );
        }
        // And the boundary itself is the AMM side on both axes (>= is the authority's rule).
        assert_eq!(entry_venue_from_reserves(boundary), EntryVenue::Amm);
        assert_eq!(
            venue_fee_bps_per_leg(boundary),
            VENUE_FEE_BPS_POST_GRADUATION
        );
    }

    #[test]
    fn the_observed_label_and_the_sizing_venue_are_different_questions() {
        // A graduated market: our fill lands on the AMM...
        let venue = entry_venue_from_reserves(GRADUATION_VSOL_LAMPORTS * 2);
        assert_eq!(venue, EntryVenue::Amm);
        assert!(sized_on_amm(venue));
        assert_eq!(venue_label(venue), "pumpswap");
        // ...while its recent tape may still read `pumpfun` (or `mixed`). The bundle carries
        // both, and neither is derived from the other.
        let observed = "pumpfun";
        assert_ne!(observed, venue_label(venue));
    }

    #[test]
    fn a_curve_candidate_is_never_sized_as_amm() {
        let venue = entry_venue_from_reserves(GRADUATION_VSOL_LAMPORTS - 1);
        assert_eq!(venue, EntryVenue::BondingCurve);
        assert!(!sized_on_amm(venue));
        assert_eq!(venue_label(venue), "pumpfun");
    }
}
