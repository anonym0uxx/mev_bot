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
    /// The mint's NAME and its resolved narrative (`TokenIdentity`). Captured at
    /// mint detection; **still not trained** — `is_trained()` stays `false` and says so.
    ///
    /// Operator decision 2026-09-26: the narrative is an INPUT the model infers
    /// over, not a gate. And the operator's follow-up ruling the same day: the
    /// CURRENT model must be able to use the token name, before any corpus has
    /// trained on the block. That is the shipped posture — see
    /// [`BundlePolicy::live`] — and it is a deliberate widening, not a claim that
    /// the corpus exists. The gate's §17 refusal still governs the derived
    /// families; the reasoning for treating the identity differently is in `live`.
    TokenIdentity,
}

impl FieldFamily {
    /// Every family, in the order they should appear in a bundle.
    pub const ALL: [FieldFamily; 5] = [
        FieldFamily::LiveFlowState,
        FieldFamily::CalloutImpact,
        FieldFamily::LeadingIndicators,
        FieldFamily::FundingGraph,
        FieldFamily::TokenIdentity,
    ];

    /// The stable label (logs and dashboards key on this — never reword).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FieldFamily::LiveFlowState => "live_flow_state",
            FieldFamily::CalloutImpact => "callout_impact",
            FieldFamily::LeadingIndicators => "leading_indicators",
            FieldFamily::FundingGraph => "funding_graph",
            FieldFamily::TokenIdentity => "token_identity",
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
            | FieldFamily::FundingGraph
            | FieldFamily::TokenIdentity => false,
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
    /// The shipped posture is the LIVE policy. See [`BundlePolicy::live`] for the
    /// operator decision that widens it past `trained_only()`.
    fn default() -> Self {
        Self::live()
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

    /// The policy the LIVE bundle uses, as the operator has ruled it.
    ///
    /// Trained families **plus `TokenIdentity`**. Operator decision 2026-09-26: the token name
    /// must reach the CURRENT model, before any corpus has trained on the block.
    ///
    /// Why this is not the §17 failure the gate exists to prevent. That guard is about **derived
    /// feature families**, whose meaning exists only in a training distribution — a new numeric
    /// aggregate is uninterpretable without one, which is why emitting it early is genuinely
    /// out-of-distribution. The identity block is not that: it is the mint's own **creation
    /// metadata plus human-readable labels**. The name is text the model reads, and the narrative
    /// renders as `animal` / `novel` / `Eligible`, never as bare wire codes. The current model can
    /// therefore use it without having trained on the block's *format*.
    ///
    /// The honest cost, stated so nobody reads this as validation: the block will be in the live
    /// prompt, but the model has never been taught to weight it, so it is evidence of **unknown
    /// reliability** until the c13 corpus lands. Capture it, emit it, measure what it does — do not
    /// treat it as signal that has been paid for.
    #[must_use]
    pub fn live() -> Self {
        let mut p = Self::trained_only();
        p.emitted.insert(FieldFamily::TokenIdentity);
        p
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

    /// Drop a family from the policy — **narrowing only, never a widening path.**
    ///
    /// Narrowing cannot make an untrained family emittable, so it needs no corpus argument.
    /// It exists so the §17 guard in the live formatter is a path with a test on it: a
    /// refusal branch that nothing can construct is indistinguishable from no guard at all.
    pub fn withhold(&mut self, family: FieldFamily) {
        self.emitted.remove(&family);
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

    /// Pins the operator decision: the CURRENT model sees the token name. If this flips back,
    /// the live prompt silently loses the name again and nobody notices.
    #[test]
    fn the_shipped_policy_emits_the_token_identity() {
        let p = BundlePolicy::default();
        assert!(
            p.is_emitted(FieldFamily::TokenIdentity),
            "the shipped policy must emit the identity"
        );
        assert!(p.is_emitted(FieldFamily::LiveFlowState));
        assert_eq!(BundlePolicy::live().emitted(), p.emitted());
    }

    /// The FACT is unchanged and must keep being stated: no corpus has trained on the identity.
    /// `live()` widens the shipped posture; it does not rewrite what is true about training.
    /// The §17 refusal must remain reachable, or it is not a guard.
    #[test]
    fn the_untrained_fact_and_the_refusal_path_survive_the_widening() {
        assert!(!FieldFamily::TokenIdentity.is_trained());
        assert!(BundlePolicy::trained_only().is_withheld(FieldFamily::TokenIdentity));
        assert_eq!(
            BundlePolicy::new([FieldFamily::LiveFlowState, FieldFamily::TokenIdentity]),
            Err(GateError::UntrainedFamilyEmitted(
                FieldFamily::TokenIdentity
            )),
            "hand-assembling a widened policy must still fail closed"
        );
    }

    #[test]
    fn the_live_bundle_emits_exactly_the_trained_families() {
        let p = BundlePolicy::trained_only();
        assert!(p.is_emitted(FieldFamily::LiveFlowState));
        for withheld in [
            FieldFamily::CalloutImpact,
            FieldFamily::LeadingIndicators,
            FieldFamily::FundingGraph,
            FieldFamily::TokenIdentity,
        ] {
            assert!(
                p.is_withheld(withheld),
                "{} is derived and captured but must NOT reach the model until a corpus trains on it",
                withheld.as_str()
            );
        }
        assert_eq!(p.emitted(), vec![FieldFamily::LiveFlowState]);
    }

    /// The identity is captured at mint detection and the model is NOT shown it
    /// until a corpus trained on it exists — the §17 OOD protection applied to the
    /// narrative. Enabling it is the documented one-line toggle.
    #[test]
    fn the_token_identity_is_withheld_until_a_corpus_trains_on_it() {
        let mut p = BundlePolicy::trained_only();
        assert!(p.is_withheld(FieldFamily::TokenIdentity));
        assert_eq!(FieldFamily::TokenIdentity.as_str(), "token_identity");
        assert!(!FieldFamily::TokenIdentity.is_trained());

        // A widening attempt without an accepted corpus fails closed...
        assert!(p.enable(FieldFamily::TokenIdentity, "").is_err());
        // ...and with one it sticks, touching nothing else.
        p.enable(FieldFamily::TokenIdentity, "c13")
            .expect("c13 is an accepted corpus");
        assert!(p.is_emitted(FieldFamily::TokenIdentity));
        assert!(p.is_withheld(FieldFamily::CalloutImpact));
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
