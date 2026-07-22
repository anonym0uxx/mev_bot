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
