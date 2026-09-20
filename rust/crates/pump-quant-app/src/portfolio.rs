//! The portfolio-layer exposure control — the half of the size question that is **not** a
//! per-row decision.
//!
//! `KELLY_AUDIT_C12` (2026-09-19) ruled the serve-time size EV-gated/binary *and* measured why
//! per-row sizing cannot be the whole story: ruin is a cluster property. Under per-stratum
//! tiering the median concurrent AMM exposure was **46x the account** and 15 of 48 clusters
//! were total losses, which is why the audit pairs the venue rule with a portfolio-layer
//! control rather than trusting a per-trade fraction. This module is that control, kept pure
//! so it is testable without a clock, a tape, or an engine:
//!
//! * **at most one live position per mint episode** — a second admit into a mint we still hold
//!   is refused rather than silently replacing the position we paid for;
//! * **at most `k_max` live positions** (`cfg.max_concurrent_positions`) — the existing gate
//!   cap, restated here so the sizing can see it;
//! * **per-position notional = `deployable / k_max`**, so a full-book cluster is the account's
//!   deployable budget rather than a multiple of it. The survival floor is respected by
//!   construction on this path: the notional is derived *after* the floor is carved out, which
//!   is why a tight floor yields a small trade instead of a vetoed one.
//!
//! When the cap cannot be enforced live (a serve path with no portfolio view), the audit's
//! fallback applies: every position is sized uniformly at **half**, so aggregate exposure stays
//! inside what the cap would have allowed. That half is a Rust-chosen fraction, NOT the model's
//! retired `MID` tier — `EntryVenue::ruled_size` never returns `Mid`, and the fallback goes
//! through [`resolve_clip_at_fraction_bps`] precisely so the two are not confusable.

use pump_quant_inference::SizeTier;
use std::collections::BTreeSet;

use pump_quant_inference::EntryVenue;

/// The served fraction when the portfolio cap is unenforceable — the audit's uniform-MID
/// fallback, in bps. A Rust policy, not a tier the model may name.
pub const UNENFORCEABLE_FALLBACK_BPS: u32 = 5_000;

/// Why an admission was refused. Both are selection refusals: they cost nothing and are
/// journaled by the caller — never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionRefusal {
    /// The mint already has a live position. One position per mint episode.
    AlreadyHeld,
    /// `k_max` positions are already live.
    CapReached,
}

impl AdmissionRefusal {
    /// Stable journal token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AdmissionRefusal::AlreadyHeld => "already_held",
            AdmissionRefusal::CapReached => "cap_reached",
        }
    }
}

/// The verdict of the portfolio layer for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// There is room: the caller may size and submit.
    Open,
    /// There is no room, and why.
    Refuse(AdmissionRefusal),
}

/// The portfolio-layer cap: how many positions may be live, whether that can be enforced, and
/// everything derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortfolioCap {
    /// Maximum concurrently live positions (`cfg.max_concurrent_positions`).
    k_max: usize,
    /// Whether the serving engine can actually enforce the cap at admit time. `false` selects
    /// the uniform-half fallback.
    enforced: bool,
}

impl PortfolioCap {
    /// A cap that can be enforced live (the engine's own case).
    #[must_use]
    pub fn enforced(k_max: usize) -> Self {
        Self {
            // A zero cap would refuse every trade at a degenerate configuration; clamp to one
            // (the gate's own floor) rather than divide by zero downstream.
            k_max: k_max.max(1),
            enforced: true,
        }
    }

    /// A cap the serve path CANNOT enforce (no portfolio view). Sizing falls back to uniform
    /// half so exposure stays inside what the cap would have allowed.
    #[must_use]
    pub fn unenforceable(k_max: usize) -> Self {
        Self {
            k_max: k_max.max(1),
            enforced: false,
        }
    }

    /// The concurrency cap in force.
    #[must_use]
    pub fn k_max(&self) -> usize {
        self.k_max
    }

    /// Whether the cap is enforced live.
    #[must_use]
    pub fn is_enforced(&self) -> bool {
        self.enforced
    }

    /// One position per mint episode, then `k_max` positions overall. Order matters only for
    /// the refusal reason: a mint we already hold is `AlreadyHeld` even when the book is full,
    /// because that is the cause a human needs to read.
    #[must_use]
    pub fn admit(&self, live: &BTreeSet<[u8; 32]>, mint: &[u8; 32]) -> Admission {
        if live.contains(mint) {
            return Admission::Refuse(AdmissionRefusal::AlreadyHeld);
        }
        if live.len() >= self.k_max {
            return Admission::Refuse(AdmissionRefusal::CapReached);
        }
        Admission::Open
    }

    /// The notional one position may deploy: the deployable budget split across the cap.
    ///
    /// Integer division in lamports, and the remainder is deliberately NOT redistributed —
    /// dropping a few lamports to the floor is the conservative direction, and it keeps the
    /// function a pure `f(deployable, k_max)` the tests can pin.
    #[must_use]
    pub fn per_position_notional(&self, deployable_lamports: u64) -> u64 {
        deployable_lamports / self.k_max as u64
    }

    /// The fraction of the per-position notional that is served.
    #[must_use]
    /// The fraction the VENUE RULE would use — `KELLY_AUDIT_C12`'s amm->FULL / curve->SMALL.
    ///
    /// This is a **default**, not the serving authority: since the trading brain holds
    /// decision authority, the size that sizes a trade is the tier the MODEL emitted (see
    /// [`PortfolioCap::fraction_bps_for`]). This accessor is what a non-model caller — a
    /// replay harness, a stub, a pre-weights smoke — sizes with when no verdict exists.
    pub fn served_fraction_bps(&self, venue: EntryVenue) -> u32 {
        if self.enforced {
            venue.ruled_size().fraction_bps()
        } else {
            UNENFORCEABLE_FALLBACK_BPS
        }
    }

    /// The clip fraction for a tier **the model chose**.
    ///
    /// The model's own weight governs. The one exception is capital safety, not inference:
    /// when the concurrency cap cannot be counted live (`enforced == false`) a per-row weight
    /// cannot be trusted to keep the book bounded, so the audit's uniform-half fallback is
    /// used instead. That substitution is about the book being uncountable, not about the
    /// model being overruled.
    pub fn fraction_bps_for(&self, tier: SizeTier) -> u32 {
        if self.enforced {
            tier.fraction_bps()
        } else {
            UNENFORCEABLE_FALLBACK_BPS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_inference::SizeTier;

    fn mints(n: usize) -> BTreeSet<[u8; 32]> {
        (0..n).map(|i| [i as u8; 32]).collect()
    }

    #[test]
    fn one_position_per_mint_episode_even_when_the_book_has_room() {
        let cap = PortfolioCap::enforced(3);
        let live = mints(1);
        let m = [0u8; 32];
        assert_eq!(
            cap.admit(&live, &m),
            Admission::Refuse(AdmissionRefusal::AlreadyHeld)
        );
        assert_eq!(cap.admit(&live, &[9u8; 32]), Admission::Open);
    }

    #[test]
    fn the_cap_refuses_and_an_already_held_mint_reports_that_cause_first() {
        let cap = PortfolioCap::enforced(2);
        let live = mints(2);
        assert_eq!(
            cap.admit(&live, &[7u8; 32]),
            Admission::Refuse(AdmissionRefusal::CapReached)
        );
        // The book is full AND the mint is held: the human-readable cause is the mint.
        assert_eq!(
            cap.admit(&live, &[0u8; 32]),
            Admission::Refuse(AdmissionRefusal::AlreadyHeld)
        );
    }

    #[test]
    fn a_full_book_is_the_deployable_budget_not_a_multiple_of_it() {
        let cap = PortfolioCap::enforced(4);
        assert_eq!(cap.per_position_notional(1_000_000_000), 250_000_000);
        // Remainder is dropped to the floor, never redistributed.
        assert_eq!(cap.per_position_notional(1_000_000_001), 250_000_000);
        // A degenerate cap cannot divide by zero.
        assert_eq!(
            PortfolioCap::enforced(0).per_position_notional(1_000_000_000),
            1_000_000_000
        );
    }

    #[test]
    fn the_unenforceable_cap_sizes_uniformly_at_half_and_never_serves_mid_tier() {
        let cap = PortfolioCap::unenforceable(4);
        assert_eq!(
            cap.served_fraction_bps(EntryVenue::Amm),
            UNENFORCEABLE_FALLBACK_BPS
        );
        assert_eq!(
            cap.served_fraction_bps(EntryVenue::BondingCurve),
            UNENFORCEABLE_FALLBACK_BPS
        );
        assert!(!cap.is_enforced());
        // The fallback is a fraction, not the retired tier: the venue rule can never name MID.
        for v in [EntryVenue::Amm, EntryVenue::BondingCurve] {
            assert_ne!(v.ruled_size(), SizeTier::Mid);
        }
        // And when the cap IS enforceable the venue rule is what is served.
        let ok = PortfolioCap::enforced(4);
        assert_eq!(
            ok.served_fraction_bps(EntryVenue::Amm),
            SizeTier::Full.fraction_bps()
        );
        assert_eq!(
            ok.served_fraction_bps(EntryVenue::BondingCurve),
            SizeTier::Small.fraction_bps()
        );
    }
}
