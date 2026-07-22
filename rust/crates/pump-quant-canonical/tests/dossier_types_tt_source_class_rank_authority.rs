// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'types' component (leaf 'tt_source_class_rank_authority').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_canonical::types::*;

#[test]
fn source_class_rank_is_strict_total_authority_order() {
    // Concrete authority ranks (§15): higher wins field resolution.
    assert_eq!(SourceClass::EarliestSignal.rank(), 0);
    assert_eq!(SourceClass::StructuredObservation.rank(), 1);
    assert_eq!(SourceClass::CanonicalRepair.rank(), 2);
    assert_eq!(SourceClass::ReconciledExecution.rank(), 3);

    // Strictly ascending authority: each level outranks the previous.
    assert!(SourceClass::EarliestSignal.rank() < SourceClass::StructuredObservation.rank());
    assert!(SourceClass::StructuredObservation.rank() < SourceClass::CanonicalRepair.rank());
    assert!(SourceClass::CanonicalRepair.rank() < SourceClass::ReconciledExecution.rank());

    // ReconciledExecution is the unique maximum authority; EarliestSignal the unique minimum.
    let ranks: Vec<u8> = SourceClass::ALL.iter().map(|c| c.rank()).collect();
    assert_eq!(ranks, vec![0, 1, 2, 3]);
    assert_eq!(
        *ranks.iter().max().unwrap(),
        SourceClass::ReconciledExecution.rank()
    );
    assert_eq!(
        *ranks.iter().min().unwrap(),
        SourceClass::EarliestSignal.rank()
    );

    // Injective: four classes -> four distinct ranks.
    let mut sorted = ranks.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 4);

    // ALL is ordered ascending by rank and covers exactly the four classes once.
    assert_eq!(SourceClass::ALL.len(), 4);
    for w in SourceClass::ALL.windows(2) {
        assert!(w[0].rank() < w[1].rank());
    }
    assert_eq!(SourceClass::ALL[0], SourceClass::EarliestSignal);
    assert_eq!(SourceClass::ALL[3], SourceClass::ReconciledExecution);
}
