// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'voi' component (leaf 'voi_rank_open').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_memory::voi::*;

#[test]
fn rank_open_excludes_closed_states() {
    fn mk(
        id: u64,
        impact: i128,
        status: pump_quant_memory::rows::InferenceState,
    ) -> pump_quant_memory::rows::Hypothesis {
        pump_quant_memory::rows::Hypothesis {
            id: pump_quant_memory::rows::HypothesisId(id),
            schema_version: 1,
            statement_hash: [0u8; 32],
            expected_impact_lamports: impact,
            prob_true_bps: 10_000,
            cost_to_test_lamports: 0,
            edge_half_life_secs: REF_HALF_LIFE_SECS as u64,
            status,
        }
    }
    use pump_quant_memory::rows::InferenceState as S;
    let hs = vec![
        mk(1, 2_000_000_000, S::ValidatedInference),    // closed
        mk(2, 1_000_000_000, S::Hypothesis),            // open
        mk(3, 500_000_000, S::RejectedInference),       // closed
        mk(4, 100_000_000, S::ProvisionalInference),    // open
        mk(5, 9_000_000_000, S::ExpiredInference),      // closed
        mk(6, 300_000_000, S::RegimeSpecificInference), // open
    ];
    let order: Vec<u64> = rank_open(&hs).iter().map(|r| r.id.0).collect();
    // Only open ids survive, ranked by descending impact: 2 (1 SOL), 6 (0.3), 4 (0.1).
    assert_eq!(order, vec![2, 6, 4]);

    // All-closed input yields an empty open queue.
    let closed = vec![
        mk(7, 5_000_000_000, S::RejectedInference),
        mk(8, 5_000_000_000, S::ValidatedInference),
    ];
    assert!(rank_open(&closed).is_empty());
}
