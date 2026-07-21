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
