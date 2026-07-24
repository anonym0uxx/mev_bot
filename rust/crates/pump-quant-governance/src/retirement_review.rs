//! §56 **sequential-retirement review** — the typed boundary between a
//! *nomination* and a *retirement*.
//!
//! # The failure this module exists to prevent
//!
//! An engine that measures its own realized outcomes will eventually be able to
//! say "this discovery lane has lost money over its last thirty trades". That
//! sentence is useful and it is also dangerous, because the obvious next step —
//! "so retire it" — is precisely the inference §51 exists to forbid. Episodic,
//! self-generated evidence is drawn from markets the strategy selected, at times
//! it selected, under a policy it was simultaneously changing; it is the most
//! overfit-prone data in the building. Retiring on it is how a bot talks itself
//! out of the one lane that pays, one unlucky week at a time.
//!
//! So §56 sequential retirement is a **governed** decision. It runs under the §51
//! FDR/PBO promotion statistics and against the §52 baselines, on the slow path,
//! with a human in the loop. What episodic evidence is genuinely good at is
//! **nomination**: telling that review which four subjects out of forty are worth
//! spending an FDR-corrected test on. Attention is the scarce resource; nominating
//! well is a real contribution, and it is a different contribution from deciding.
//!
//! # How the boundary is made structural rather than documented
//!
//! [`RetirementNomination`] is a plain record with no method that returns a
//! retirement, and [`ReviewOutcome`] — the type that *can* express one — cannot be
//! constructed from a nomination alone. [`review`] takes the nomination **plus**
//! the two governed verdicts (§51 statistical, §52 baseline) and returns
//! [`ReviewOutcome::Retire`] only when both concur. A caller holding a thousand
//! damning nominations and no statistical verdict cannot reach `Retire`: there is
//! no such code path to call.
//!
//! Deterministic, integer-only, allocation-free per call (§22).

/// What kind of subject a nomination concerns. Mirrors the producer's vocabulary
/// so a nomination crossing the process boundary as JSON round-trips exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewSubject {
    /// An independent discovery lane (§71.2).
    Lane,
    /// A named style lens / setup archetype.
    Archetype,
    /// One conditioned setup class.
    SetupClass,
    /// A paid or curated alpha source.
    Source,
}

impl ReviewSubject {
    /// The stable wire label.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lane => "lane",
            Self::Archetype => "archetype",
            Self::SetupClass => "setup_class",
            Self::Source => "source",
        }
    }

    /// Parse the wire label. Unknown labels are `None` — an unrecognised subject
    /// is refused, never coerced into the nearest match (§18).
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "lane" => Some(Self::Lane),
            "archetype" => Some(Self::Archetype),
            "setup_class" => Some(Self::SetupClass),
            "source" => Some(Self::Source),
            _ => None,
        }
    }
}

/// One §56 retirement-review **nomination**: a subject the producer's own realized
/// evidence says is worth examining.
///
/// This type deliberately carries no verdict, no probability, and no method that
/// yields a retirement. It is an agenda item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetirementNomination {
    /// What kind of subject.
    pub subject: ReviewSubject,
    /// Trades / episodes the nomination stands on.
    pub n: u32,
    /// The realized net that earned the nomination, lamports (signed).
    pub realized_net_lamports: i64,
}

impl RetirementNomination {
    /// Whether the nomination itself clears a review's own sample floor.
    ///
    /// A review may run a floor stricter than the producer's; this is the only
    /// question a nomination can answer about itself, and the answer is never
    /// "retire".
    #[must_use]
    pub const fn clears(&self, min_sample: u32) -> bool {
        self.n >= min_sample
    }
}

/// The outcome of one governed §56 review. `Retire` is reachable ONLY through
/// [`review`], and only with both governed verdicts concurring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// The nomination did not clear the review's sample floor. No decision was
    /// taken and none was possible (§46).
    InsufficientEvidence {
        /// The sample the nomination carried.
        n: u32,
        /// The floor it failed to reach.
        min_sample: u32,
    },
    /// The governed statistics did not concur: the subject keeps running. This is
    /// the outcome for EVERY nomination that arrives without a passing §51
    /// verdict, however damning the episodic evidence looks.
    Keep,
    /// §51 and §52 both concurred over a sufficient sample: retire.
    Retire,
}

impl ReviewOutcome {
    /// Whether this outcome retires the subject.
    #[must_use]
    pub const fn retires(self) -> bool {
        matches!(self, Self::Retire)
    }
}

/// Run one governed §56 retirement review.
///
/// * `nomination` — the episodic agenda item (an INPUT, never a verdict).
/// * `min_sample` — the review's own §46 floor.
/// * `statistically_confirmed` — the §51 verdict: did an FDR-corrected,
///   PBO-checked test over this subject conclude the deterioration is real?
/// * `baseline_confirmed` — the §52 verdict: does the subject genuinely fail
///   against its baselines?
///
/// `Retire` requires all three. A nomination on its own — even one carrying a
/// catastrophic realized net over a huge sample — returns [`ReviewOutcome::Keep`],
/// because episodic recall is not a statistical test and this crate will not let a
/// caller pretend otherwise.
#[must_use]
pub const fn review(
    nomination: &RetirementNomination,
    min_sample: u32,
    statistically_confirmed: bool,
    baseline_confirmed: bool,
) -> ReviewOutcome {
    if !nomination.clears(min_sample) {
        return ReviewOutcome::InsufficientEvidence {
            n: nomination.n,
            min_sample,
        };
    }
    if statistically_confirmed && baseline_confirmed {
        ReviewOutcome::Retire
    } else {
        ReviewOutcome::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn damning() -> RetirementNomination {
        RetirementNomination {
            subject: ReviewSubject::Lane,
            n: 10_000,
            realized_net_lamports: -900_000_000,
        }
    }

    /// The load-bearing property: episodic evidence ALONE never retires anything,
    /// no matter how bad it looks or how large the sample.
    #[test]
    fn a_nomination_alone_never_retires() {
        assert_eq!(review(&damning(), 12, false, false), ReviewOutcome::Keep);
        assert_eq!(review(&damning(), 12, true, false), ReviewOutcome::Keep);
        assert_eq!(review(&damning(), 12, false, true), ReviewOutcome::Keep);
        assert!(!review(&damning(), 12, false, true).retires());
    }

    /// Both governed verdicts concurring over a sufficient sample DOES retire —
    /// the law is a real gate, not a permanent veto.
    #[test]
    fn both_governed_verdicts_retire() {
        assert_eq!(review(&damning(), 12, true, true), ReviewOutcome::Retire);
        assert!(review(&damning(), 12, true, true).retires());
    }

    /// §46 fail-closed: the sample floor binds before the verdicts are consulted,
    /// so a thin nomination cannot be rubber-stamped by a passing test.
    #[test]
    fn the_sample_floor_binds_first() {
        let thin = RetirementNomination {
            subject: ReviewSubject::SetupClass,
            n: 3,
            realized_net_lamports: -1,
        };
        assert_eq!(
            review(&thin, 12, true, true),
            ReviewOutcome::InsufficientEvidence {
                n: 3,
                min_sample: 12
            }
        );
        assert!(!review(&thin, 12, true, true).retires());
    }

    #[test]
    fn subject_labels_round_trip_and_refuse_the_unknown() {
        for s in [
            ReviewSubject::Lane,
            ReviewSubject::Archetype,
            ReviewSubject::SetupClass,
            ReviewSubject::Source,
        ] {
            assert_eq!(ReviewSubject::from_name(s.name()), Some(s));
        }
        assert_eq!(ReviewSubject::from_name("strategy"), None);
        assert_eq!(ReviewSubject::from_name(""), None);
    }
}
