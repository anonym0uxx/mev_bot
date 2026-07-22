// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_narrative_boundary').
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
fn narrative_lands_in_research() {
    let n = NarrativeRecord {
        text: "hype".into(),
        provenance: CaptureProvenance::Chart,
        horizon: HorizonClass::Immediate,
    };
    let r: ResearchArtifact = narrative_to_research(n);
    assert_eq!(r.provenance, CaptureProvenance::Chart);
    assert_eq!(r.horizon, HorizonClass::Immediate);
    // The only admission path rejects research → no direct narrative->fact route.
    let tagged = TaggedValue::ResearchArtifact(r);
    assert_eq!(admit_fact(tagged), Err(FactError::ResearchRejected));
}
