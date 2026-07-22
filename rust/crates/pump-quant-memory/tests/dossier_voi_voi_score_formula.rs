// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'voi' component (leaf 'voi_score_formula').
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
use pump_quant_memory::voi::*;

#[test]
fn voi_score_formula_and_edges() {
    fn mk(
        id: u64,
        impact: i128,
        prob_bps: i64,
        cost: u64,
        half_life: u64,
    ) -> pump_quant_memory::rows::Hypothesis {
        pump_quant_memory::rows::Hypothesis {
            id: pump_quant_memory::rows::HypothesisId(id),
            schema_version: 1,
            statement_hash: [0u8; 32],
            expected_impact_lamports: impact,
            prob_true_bps: prob_bps,
            cost_to_test_lamports: cost,
            edge_half_life_secs: half_life,
            status: pump_quant_memory::rows::InferenceState::Hypothesis,
        }
    }
    let refh = REF_HALF_LIFE_SECS as u64;
    // 50% of 1 SOL at reference half-life, minus 0.1 SOL cost => 0.4 SOL.
    assert_eq!(
        voi_score(&mk(1, 1_000_000_000, 5_000, 100_000_000, refh)),
        400_000_000
    );
    // Zero probability collapses gross to 0 => voi is exactly minus the cost.
    assert_eq!(
        voi_score(&mk(2, 5_000_000_000, 0, 42_000_000, refh)),
        -42_000_000
    );
    // Half the reference half-life halves the gross value.
    assert_eq!(
        voi_score(&mk(3, 1_000_000_000, 10_000, 0, refh / 2)),
        500_000_000
    );
    // Negative impact (fade) yields negative gross dominated further by cost.
    assert_eq!(
        voi_score(&mk(4, -1_000_000_000, 8_000, 10_000_000, refh)),
        -810_000_000
    );
    // Positive three-way-product overflow saturates gross to i128::MAX.
    assert_eq!(voi_score(&mk(5, i128::MAX, 10_000, 0, 0)), i128::MAX);
}
