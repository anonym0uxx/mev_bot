// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'edge_decomposition' component (leaf 'confidence_rank_orders').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::edge_decomposition::*;

#[test]
fn prop_confidence_rank_orders() {
    assert_eq!(Attribution::Measured.confidence_rank(), 0);
    assert_eq!(Attribution::Estimated.confidence_rank(), 1);
    assert_eq!(Attribution::Assumed.confidence_rank(), 2);
    assert_eq!(Attribution::Unknown.confidence_rank(), 3);
    let order = [
        Attribution::Measured,
        Attribution::Estimated,
        Attribution::Assumed,
        Attribution::Unknown,
    ];
    for w in order.windows(2) {
        assert!(w[0].confidence_rank() < w[1].confidence_rank());
    }
    assert!(Attribution::Measured < Attribution::Unknown);
    assert!(Attribution::Estimated < Attribution::Assumed);
}
