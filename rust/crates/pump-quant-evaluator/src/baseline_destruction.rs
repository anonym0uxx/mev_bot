//! `baseline_destruction` — baseline-destruction verdict (constitution §42, §52,
//! §35).
//!
//! Responsibility: a challenger earns promotion only by *destroying* — beating
//! by a required margin — not just the reigning champion but every naive
//! baseline it is measured against (constitution §52 baselines: fixed ratios and
//! trivial rules are challenger baselines that a real edge must dominate). To
//! guard against the multiple-comparisons trap of picking the best of many
//! contests, the required margin is inflated family-wise by the number of
//! competitors (a Bonferroni-flavoured, integer, deterministic correction: the
//! more rivals you test against and cherry-pick from, the larger the edge you
//! must show to claim it is real).
//!
//! Integer-only (constitution §22): reconciled metric values are `i128`
//! (lamports or any fixed-point metric); no floats.

/// What kind of rival a competitor value represents (constitution §52).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompetitorKind {
    /// The reigning champion policy.
    Champion,
    /// A naive/trivial baseline (fixed ratio, buy-and-hold, random-entry, …).
    NaiveBaseline,
}

/// One rival the challenger is measured against, in reconciled metric units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Competitor {
    /// Which kind of rival this is.
    pub kind: CompetitorKind,
    /// Reconciled metric value (e.g. net-SOL lamports); higher is better.
    pub value: i128,
}

impl Competitor {
    /// Construct a champion competitor.
    pub fn champion(value: i128) -> Self {
        Competitor {
            kind: CompetitorKind::Champion,
            value,
        }
    }

    /// Construct a naive-baseline competitor.
    pub fn baseline(value: i128) -> Self {
        Competitor {
            kind: CompetitorKind::NaiveBaseline,
            value,
        }
    }
}

/// Verdict of a baseline-destruction test (constitution §42).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestructionVerdict {
    /// Challenger beat every competitor by the corrected margin.
    Defeats {
        /// The family-wise corrected margin each rival had to be beaten by.
        effective_margin: i128,
    },
    /// Challenger failed against at least one rival.
    Fails {
        /// The family-wise corrected margin.
        effective_margin: i128,
        /// The rival with the *smallest* challenger advantage (the binding one).
        blocking_kind: CompetitorKind,
        /// The blocking rival's value.
        blocking_value: i128,
        /// Challenger minus blocking value (may be negative).
        blocking_margin: i128,
    },
    /// No competitors supplied — a destruction claim over an empty field is
    /// meaningless and never granted.
    NoField,
}

impl DestructionVerdict {
    /// True iff the challenger destroyed the whole field.
    pub fn defeats(&self) -> bool {
        matches!(self, DestructionVerdict::Defeats { .. })
    }
}

/// Does the challenger destroy the champion *and* every naive baseline by the
/// family-wise-corrected required margin?
///
/// Responsibility (constitution §42, §52): let `K = competitors.len()` and the
/// corrected bar be `effective_margin = required_margin · K` (saturating,
/// non-negative by contract). The challenger destroys the field iff for **every**
/// competitor `challenger − competitor.value ≥ effective_margin`. If any rival is
/// not beaten by that bar the verdict is [`DestructionVerdict::Fails`], reporting
/// the *binding* rival (the one with the smallest challenger advantage, tie-broken
/// deterministically by input order). An empty field yields
/// [`DestructionVerdict::NoField`]. Deterministic; margin comparisons are checked
/// `i128`.
pub fn baseline_destruction(
    challenger: i128,
    competitors: &[Competitor],
    required_margin: i128,
) -> DestructionVerdict {
    if competitors.is_empty() {
        return DestructionVerdict::NoField;
    }

    let k = competitors.len() as i128;
    // Family-wise inflation of the required margin. Saturating by contract:
    // an absurd required_margin cannot panic the frozen evaluator.
    let effective_margin = required_margin.saturating_mul(k);

    // Find the binding rival: the smallest challenger advantage, first-wins tie.
    let mut binding: Option<&Competitor> = None;
    let mut binding_margin: i128 = 0;
    for c in competitors {
        let margin = challenger
            .checked_sub(c.value)
            .expect("baseline_destruction: margin i128 overflow");
        match binding {
            None => {
                binding = Some(c);
                binding_margin = margin;
            }
            Some(_) if margin < binding_margin => {
                binding = Some(c);
                binding_margin = margin;
            }
            _ => {}
        }
    }

    let binding = binding.expect("baseline_destruction: non-empty field has a binding rival");
    if binding_margin >= effective_margin {
        DestructionVerdict::Defeats { effective_margin }
    } else {
        DestructionVerdict::Fails {
            effective_margin,
            blocking_kind: binding.kind,
            blocking_value: binding.value,
            blocking_margin: binding_margin,
        }
    }
}
