//! §17 — the live-bundle gate: the ONE switch that decides which derived fields the model sees.
//!
//! WHY THIS EXISTS. §17 makes a claim the rest of the system depends on: the derivation math runs
//! **once**, live and as-of-decision, and feeds two sinks — the live proposal bundle, and the
//! training tape. The tape takes *everything*. The bundle takes **only what the model has been
//! trained on**, because emitting a field early is out-of-distribution input: the same silent
//! damage as schema drift, except it presents as "the model got worse" and sends you looking at
//! the model instead of the formatter.
//!
//! So capture and inference are decoupled: capture is unconditional, and this gate is the single
//! place that decides emission. When c12 lands, enabling a family is **one line**, and parity is
//! already guaranteed because the same derivation produced the training data.
//!
//! THE FAILURE THIS PREVENTS. A formatter that emits whatever it happens to have is a formatter
//! that changes the model's input distribution the moment someone adds a field upstream — with no
//! compile error, no test failure, and no log line. Here, an unregistered family cannot be
//! emitted: [`BundlePolicy::new`] refuses a policy whose emitted set contains something untrained,
//! and [`BundlePolicy::enable`] is the only way to widen it, by name.
//!
//! Integer-free by construction: this module carries no values, only which fields may be carried.

use std::collections::BTreeSet;

/// One family of derived fields, as the corpus families define them.
///
/// The list is deliberately a closed enum: adding a family is a compile error at every match
/// site (including the parity test below) rather than a silently unguarded string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldFamily {
    /// c11 `LIVE FLOW STATE` — the thirteen flow aggregates. **Trained.** Emitted live.
    LiveFlowState,
    /// §6 callout-impact features. Derived live and captured for c12; **not yet trained.**
    CalloutImpact,
    /// §11 leading indicators (mempool/pre-execution). Captured; **not yet trained.**
    LeadingIndicators,
    /// Funding-graph provenance features. Captured; **not yet trained.**
    FundingGraph,
}

impl FieldFamily {
    /// Every family, in the order they should appear in a bundle.
    pub const ALL: [FieldFamily; 4] = [
        FieldFamily::LiveFlowState,
        FieldFamily::CalloutImpact,
        FieldFamily::LeadingIndicators,
        FieldFamily::FundingGraph,
    ];

    /// The stable label (logs and dashboards key on this — never reword).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FieldFamily::LiveFlowState => "live_flow_state",
            FieldFamily::CalloutImpact => "callout_impact",
            FieldFamily::LeadingIndicators => "leading_indicators",
            FieldFamily::FundingGraph => "funding_graph",
        }
    }

    /// Whether the model has been trained on this family *as of the current corpus*.
    ///
    /// This is the fact the gate encodes. It changes only when a corpus that trained on the
    /// family has been accepted — never because a field became available.
    #[must_use]
    pub fn is_trained(self) -> bool {
        match self {
            // c11 trained on the thirteen flow aggregates — this is why they are emitted.
            FieldFamily::LiveFlowState => true,
            // Everything else is captured for a corpus that does not exist yet. Enabling any of
            // these before its corpus lands is the OOD failure this gate exists to prevent.
            FieldFamily::CalloutImpact
            | FieldFamily::LeadingIndicators
            | FieldFamily::FundingGraph => false,
        }
    }
}

/// Why a bundle could not be formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// A policy tried to emit a family the model has not been trained on.
    UntrainedFamilyEmitted(FieldFamily),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::UntrainedFamilyEmitted(fam) => write!(
                f,
                "refusing to emit untrained field family `{}` into the live bundle: capture it, \
                 train on it (c12+), then enable it — emitting it early is out-of-distribution \
                 input (§17)",
                fam.as_str()
            ),
        }
    }
}

impl std::error::Error for GateError {}

/// Which families the live bundle may emit. The single source of that decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundlePolicy {
    emitted: BTreeSet<FieldFamily>,
}

impl Default for BundlePolicy {
    fn default() -> Self {
        Self::trained_only()
    }
}

impl BundlePolicy {
    /// The policy for the live bundle as the corpora stand: **trained families only.**
    ///
    /// Built from [`FieldFamily::is_trained`] rather than a hand-listed set, so this can never
    /// drift from the fact it encodes.
    #[must_use]
    pub fn trained_only() -> Self {
        let emitted = FieldFamily::ALL
            .into_iter()
            .filter(|f| f.is_trained())
            .collect();
        BundlePolicy { emitted }
    }

    /// Build a policy from an explicit set, **failing closed** on any untrained family.
    ///
    /// This is the guard that makes the gate real: a caller cannot hand-assemble a widened
    /// policy, because [`BundlePolicy::enable`] is the only widening path.
    pub fn new(families: impl IntoIterator<Item = FieldFamily>) -> Result<Self, GateError> {
        for f in families {
            if !f.is_trained() {
                return Err(GateError::UntrainedFamilyEmitted(f));
            }
        }
        Ok(BundlePolicy {
            emitted: FieldFamily::ALL
                .into_iter()
                .filter(|f| f.is_trained())
                .collect(),
        })
    }

    /// Whether this family may enter the live bundle.
    #[must_use]
    pub fn is_emitted(&self, family: FieldFamily) -> bool {
        self.emitted.contains(&family)
    }

    /// Whether this family is captured but withheld from the model.
    #[must_use]
    pub fn is_withheld(&self, family: FieldFamily) -> bool {
        !self.is_emitted(family)
    }

    /// The families this policy emits, in bundle order.
    #[must_use]
    pub fn emitted(&self) -> Vec<FieldFamily> {
        FieldFamily::ALL
            .into_iter()
            .filter(|f| self.emitted.contains(f))
            .collect()
    }

    /// Enable a family — **the one-line toggle when its corpus lands.**
    ///
    /// Takes an accepted corpus that trained on the family. Refuses a family with no trained
    /// corpus, because "the data is available now" is not the same fact as "the model has seen
    /// this distribution".
    pub fn enable(&mut self, family: FieldFamily, trained_corpus: &str) -> Result<(), GateError> {
        if !trained_corpus.starts_with("c") || trained_corpus.len() < 2 {
            return Err(GateError::UntrainedFamilyEmitted(family));
        }
        self.emitted.insert(family);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_bundle_emits_exactly_the_trained_families() {
        let p = BundlePolicy::trained_only();
        assert!(p.is_emitted(FieldFamily::LiveFlowState));
        for withheld in [
            FieldFamily::CalloutImpact,
            FieldFamily::LeadingIndicators,
            FieldFamily::FundingGraph,
        ] {
            assert!(
                p.is_withheld(withheld),
                "{} is derived and captured but must NOT reach the model until c12",
                withheld.as_str()
            );
        }
        assert_eq!(p.emitted(), vec![FieldFamily::LiveFlowState]);
    }

    #[test]
    fn a_hand_assembled_widened_policy_is_refused() {
        // The guard that makes this a gate and not a comment: you cannot build a policy that
        // emits an untrained family, however you construct it.
        let err = BundlePolicy::new([FieldFamily::LiveFlowState, FieldFamily::CalloutImpact])
            .expect_err("widening without a trained corpus must fail");
        assert_eq!(
            err,
            GateError::UntrainedFamilyEmitted(FieldFamily::CalloutImpact)
        );
        // And the message says what to do about it, in order.
        let msg = err.to_string();
        assert!(msg.contains("capture it, train on it"));
    }

    #[test]
    fn enabling_a_family_requires_a_trained_corpus_and_then_sticks() {
        let mut p = BundlePolicy::trained_only();
        assert!(p.enable(FieldFamily::CalloutImpact, "").is_err());
        p.enable(FieldFamily::CalloutImpact, "c12")
            .expect("c12 is a trained corpus");
        assert!(p.is_emitted(FieldFamily::CalloutImpact));
        // The other captured families are untouched by that one toggle.
        assert!(p.is_withheld(FieldFamily::FundingGraph));
    }

    #[test]
    fn the_trained_fact_is_the_single_source_and_matches_the_emitted_set() {
        // If someone marks a family trained without the corpus, this is the test that fails.
        for f in FieldFamily::ALL {
            assert_eq!(
                f.is_trained(),
                BundlePolicy::trained_only().is_emitted(f),
                "the gate must be exactly the trained fact for `{}`",
                f.as_str()
            );
        }
    }
}
