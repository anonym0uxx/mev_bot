//! `convexity_enrich` — builder for real (non-degenerate) convexity events
//! (constitution §49).
//!
//! The unified ledger in [`crate::convexity_ledger`] folds [`ConvexityEvent`]s,
//! but those events have to be *constructed* from what actually happened at each
//! rule firing. This module is that construction layer: the app supplies the
//! marks it observed — a veto that removed a position, a confidence-reducer that
//! took a fractional slice, or a rule that allowed full participation — and this
//! builder turns each into the correctly-signed [`ConvexityEvent`] the ledger
//! expects, so no rule is recorded with a degenerate (self-cancelling) event.
//!
//! Two suppression shapes are modeled explicitly (§49):
//!   * **veto → counterfactual-vs-zero:** the full unsuppressed position's
//!     outcome is the counterfactual; the realized outcome is exactly zero
//!     (nothing was taken).
//!   * **haircut → reduced-vs-full size:** the counterfactual is the full-size
//!     outcome; the realized outcome is that same path scaled by the applied
//!     size fraction (integer `num/den`), so a partial de-risk records the slice
//!     it kept, not a phantom all-or-nothing.
//!
//! Pure and integer-only (§22): the app supplies the marks; scaling is `i128`
//! `num/den` truncating toward zero; no floats, no wall-clock, no RNG.

use crate::convexity_ledger::{ConvexityEvent, RuleId};

/// A size fraction `num/den` in `[0, 1]`, integer-only. Used to scale a
/// full-position counterfactual down to the slice a haircut actually took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeFraction {
    /// Numerator of the applied size fraction.
    pub num: u64,
    /// Denominator of the applied size fraction (must be non-zero).
    pub den: u64,
}

impl SizeFraction {
    /// Construct a fraction. Panics if `den == 0` (a zero denominator is
    /// malformed input, never a silent 0/0).
    pub fn new(num: u64, den: u64) -> Self {
        assert!(den != 0, "SizeFraction: denominator must be non-zero");
        SizeFraction { num, den }
    }

    /// Scale a bps value by this fraction, `i128` interim, truncating toward zero.
    fn scale_bps(&self, full_bps: i64) -> i64 {
        ((full_bps as i128 * self.num as i128) / self.den as i128) as i64
    }
}

/// One observed rule firing the app hands to the builder (§49). Each variant
/// carries the counterfactual of the *full, unsuppressed* position plus the MFE
/// of the underlying, so both sides of the convexity ruler are recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvexityMark {
    /// A hard veto removed the position entirely (counterfactual-vs-zero).
    Veto {
        /// Rule that fired.
        rule: RuleId,
        /// Net bps the full, unsuppressed position would have realized.
        counterfactual_bps: i64,
        /// Max favorable excursion of the underlying, bps.
        mfe_bps: i64,
    },
    /// A confidence-reducer / partial de-risk took only a size fraction
    /// (reduced-vs-full size).
    Haircut {
        /// Rule that fired.
        rule: RuleId,
        /// Net bps the full-size position would have realized.
        full_counterfactual_bps: i64,
        /// The size fraction actually taken.
        applied: SizeFraction,
        /// Max favorable excursion of the underlying, bps.
        mfe_bps: i64,
    },
    /// The rule allowed full participation (no suppression) — recorded so the
    /// ledger can credit MFE captured and right-tail preserved.
    Allowed {
        /// Rule that (did not) fire.
        rule: RuleId,
        /// Net bps realized at full participation (≈ counterfactual).
        realized_bps: i64,
        /// Max favorable excursion of the underlying, bps.
        mfe_bps: i64,
    },
}

/// Turn a single mark into its [`ConvexityEvent`] (§49). Pure.
///
/// A veto records `counterfactual = full, realized = 0` and is `suppressed`. A
/// haircut records `counterfactual = full, realized = full·applied` and is
/// `suppressed` (a partial de-risk is a suppression of the un-taken slice). An
/// allow records `counterfactual ≈ realized` and is *not* suppressed.
pub fn event_from_mark(mark: &ConvexityMark) -> ConvexityEvent {
    match *mark {
        ConvexityMark::Veto {
            rule,
            counterfactual_bps,
            mfe_bps,
        } => ConvexityEvent {
            rule,
            suppressed: true,
            counterfactual_bps,
            realized_bps: 0,
            mfe_bps,
        },
        ConvexityMark::Haircut {
            rule,
            full_counterfactual_bps,
            applied,
            mfe_bps,
        } => ConvexityEvent {
            rule,
            suppressed: true,
            counterfactual_bps: full_counterfactual_bps,
            realized_bps: applied.scale_bps(full_counterfactual_bps),
            mfe_bps,
        },
        ConvexityMark::Allowed {
            rule,
            realized_bps,
            mfe_bps,
        } => ConvexityEvent {
            rule,
            suppressed: false,
            counterfactual_bps: realized_bps,
            realized_bps,
            mfe_bps,
        },
    }
}

/// Build the full [`ConvexityEvent`] vector from a batch of marks (§49).
///
/// Order-preserving and pure — the app supplies the marks, this returns the
/// events ready for [`crate::convexity_ledger::build_ledger`]. Deterministic.
pub fn build_events(marks: &[ConvexityMark]) -> Vec<ConvexityEvent> {
    marks.iter().map(event_from_mark).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convexity_ledger::{build_ledger, RuleKind};

    fn rule(id: u64) -> RuleId {
        RuleId::new(RuleKind::Veto, id)
    }

    #[test]
    fn veto_is_counterfactual_vs_zero() {
        let m = ConvexityMark::Veto {
            rule: rule(1),
            counterfactual_bps: 8_000,
            mfe_bps: 9_000,
        };
        let e = event_from_mark(&m);
        assert!(e.suppressed);
        assert_eq!(e.counterfactual_bps, 8_000);
        assert_eq!(e.realized_bps, 0);
        assert_eq!(e.mfe_bps, 9_000);
    }

    #[test]
    fn haircut_scales_realized_by_fraction() {
        // Half size on a +10000 bps full path -> realized 5000.
        let m = ConvexityMark::Haircut {
            rule: rule(2),
            full_counterfactual_bps: 10_000,
            applied: SizeFraction::new(1, 2),
            mfe_bps: 12_000,
        };
        let e = event_from_mark(&m);
        assert!(e.suppressed);
        assert_eq!(e.counterfactual_bps, 10_000);
        assert_eq!(e.realized_bps, 5_000);
    }

    #[test]
    fn haircut_truncates_toward_zero() {
        // 1/3 of -100 = -33 (toward zero).
        let m = ConvexityMark::Haircut {
            rule: rule(3),
            full_counterfactual_bps: -100,
            applied: SizeFraction::new(1, 3),
            mfe_bps: 0,
        };
        let e = event_from_mark(&m);
        assert_eq!(e.realized_bps, -33);
    }

    #[test]
    fn allowed_is_not_suppressed() {
        let m = ConvexityMark::Allowed {
            rule: rule(4),
            realized_bps: 4_000,
            mfe_bps: 5_000,
        };
        let e = event_from_mark(&m);
        assert!(!e.suppressed);
        assert_eq!(e.counterfactual_bps, 4_000);
        assert_eq!(e.realized_bps, 4_000);
    }

    #[test]
    fn build_events_feeds_ledger_non_degenerate() {
        let marks = vec![
            ConvexityMark::Veto {
                rule: rule(1),
                counterfactual_bps: -5_000,
                mfe_bps: 100,
            },
            ConvexityMark::Allowed {
                rule: rule(1),
                realized_bps: 12_000,
                mfe_bps: 15_000,
            },
        ];
        let events = build_events(&marks);
        assert_eq!(events.len(), 2);
        let led = build_ledger(&events, 5_000);
        assert_eq!(led.len(), 1);
        // The veto avoided a -5000 loss; the allow captured 12000 MFE-realized.
        assert_eq!(led[0].losses_avoided_bps, 5_000);
        assert_eq!(led[0].mfe_captured_bps, 12_000);
    }

    #[test]
    #[should_panic(expected = "denominator must be non-zero")]
    fn zero_denominator_panics() {
        let _ = SizeFraction::new(1, 0);
    }

    #[test]
    fn build_is_order_preserving_and_deterministic() {
        let marks = vec![
            ConvexityMark::Veto {
                rule: rule(9),
                counterfactual_bps: 1_000,
                mfe_bps: 0,
            },
            ConvexityMark::Veto {
                rule: rule(1),
                counterfactual_bps: 2_000,
                mfe_bps: 0,
            },
        ];
        let a = build_events(&marks);
        let b = build_events(&marks);
        assert_eq!(a, b);
        // order preserved (rule 9 first, as supplied).
        assert_eq!(a[0].rule, rule(9));
    }
}
