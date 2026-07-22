// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_no_llm_fact').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn model_and_research_rejected_chain_admitted() {
    let m = TaggedValue::Model(ModelOutput {
        text: "buy now".into(),
    });
    assert_eq!(admit_fact(m), Err(FactError::ModelOutputRejected));

    let r = TaggedValue::ResearchArtifact(ResearchArtifact {
        note: "narrative".into(),
        provenance: CaptureProvenance::Social,
        horizon: HorizonClass::Short,
    });
    assert_eq!(admit_fact(r), Err(FactError::ResearchRejected));

    let c = TaggedValue::ChainEvidence(ChainEvidence {
        slot: 100,
        value: 9,
    });
    assert_eq!(
        admit_fact(c),
        Ok(FactValue {
            slot: 100,
            value: 9
        })
    );
}
