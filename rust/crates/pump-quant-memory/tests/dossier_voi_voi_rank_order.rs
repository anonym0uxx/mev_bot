// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'voi' component (leaf 'voi_rank_order').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_memory::voi::*;

#[test]
fn rank_orders_descending_score_then_ascending_id() {
    fn mk(id: u64, impact: i128, prob_bps: i64, cost: u64) -> pump_quant_memory::rows::Hypothesis {
        pump_quant_memory::rows::Hypothesis {
            id: pump_quant_memory::rows::HypothesisId(id),
            schema_version: 1,
            statement_hash: [0u8; 32],
            expected_impact_lamports: impact,
            prob_true_bps: prob_bps,
            cost_to_test_lamports: cost,
            edge_half_life_secs: REF_HALF_LIFE_SECS as u64,
            status: pump_quant_memory::rows::InferenceState::Hypothesis,
        }
    }
    let hs = vec![
        mk(1, 1_000_000_000, 5_000, 100_000_000), // 400M
        mk(2, 2_000_000_000, 2_500, 0),           // 500M
        mk(3, 500_000_000, 10_000, 50_000_000),   // 450M
        mk(4, -1_000_000_000, 8_000, 10_000_000), // -810M
        mk(5, 0, 10_000, 0),                      // 0
    ];
    let ranked = rank(&hs);
    let order: Vec<u64> = ranked.iter().map(|r| r.id.0).collect();
    assert_eq!(order, vec![2, 3, 1, 5, 4]);
    assert_eq!(ranked[0].score, 500_000_000);
    assert_eq!(ranked[4].score, -810_000_000);

    // Ties (all score 0) break by ascending id.
    let ties = vec![
        mk(8, 0, 10_000, 0),
        mk(3, 0, 10_000, 0),
        mk(6, 0, 10_000, 0),
    ];
    let tie_order: Vec<u64> = rank(&ties).iter().map(|r| r.id.0).collect();
    assert_eq!(tie_order, vec![3, 6, 8]);

    // Empty input ranks to empty.
    assert!(rank(&[]).is_empty());
}
