// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'determinants' component (leaf 'dt_d5_skin_in_game').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_social::determinants::*;

#[test]
fn dt_d5_skin_prop() {
    // Aligned accumulation: buy 8/10 -> 8000, no dumping -> value 8000, not suspect.
    let aligned = SkinInGameEvidence {
        funding_edges: 3,
        timing_edges: 1,
        metadata_reuse_edges: 0,
        buy_before_call: 8,
        distribute_into_call: 0,
        total_calls: 10,
    };
    let ra = d5_skin_in_game(&aligned, 3_000);
    assert_eq!(ra.score.value_bps, 8_000);
    assert_eq!(ra.score.sample_size, 10);
    assert!(!ra.shill_suspect);

    // Dumping: buy 2->2000, dump 5->5000; value=clamp(2000-2*5000)=-8000; dump 5000>3000 -> suspect.
    let dumping = SkinInGameEvidence {
        funding_edges: 0,
        timing_edges: 0,
        metadata_reuse_edges: 0,
        buy_before_call: 2,
        distribute_into_call: 5,
        total_calls: 10,
    };
    let rd = d5_skin_in_game(&dumping, 3_000);
    assert_eq!(rd.score.value_bps, -8_000);
    assert!(rd.shill_suspect);

    // Threshold boundary: dump_share exactly at threshold is NOT suspect (strict >).
    let at_thresh = SkinInGameEvidence {
        funding_edges: 0,
        timing_edges: 0,
        metadata_reuse_edges: 0,
        buy_before_call: 0,
        distribute_into_call: 3,
        total_calls: 10,
    };
    let rt = d5_skin_in_game(&at_thresh, 3_000);
    assert!(!rt.shill_suspect);

    // Edge: no covered calls -> empty score, not suspect.
    let empty = SkinInGameEvidence {
        funding_edges: 5,
        timing_edges: 5,
        metadata_reuse_edges: 5,
        buy_before_call: 0,
        distribute_into_call: 0,
        total_calls: 0,
    };
    let re = d5_skin_in_game(&empty, 3_000);
    assert_eq!(re.score.value_bps, 0);
    assert_eq!(re.score.sample_size, 0);
    assert_eq!(re.score.confidence_bps, 0);
    assert!(!re.shill_suspect);
}
