// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'entry_arbitration' component (leaf 'arbitrate_slots_bounded').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::entry_arbitration::*;

fn c(id: u64, net: i64, size: u64) -> EntryCandidate {
    EntryCandidate {
        candidate_id: id,
        entry_mode: 1,
        archetype: 2,
        regime: 3,
        expected_net_sol_lamports: net,
        size_lamports: size,
    }
}

#[test]
fn arbitrate_awards_top_ranked_bounded_by_slots() {
    let cands = [c(1, 100, 10), c(2, 400, 10), c(3, 300, 10), c(4, 200, 10)];
    let params = ArbitrationParams {
        max_slots: 2,
        exposure_cap_lamports: 1_000,
        min_expected_net_lamports: 0,
    };
    let out = arbitrate(&cands, &params);
    // Awarded is bounded by max_slots and ordered best-first.
    assert_eq!(out.awarded.len(), 2);
    assert_eq!(out.awarded[0].candidate_id, 2);
    assert_eq!(out.awarded[1].candidate_id, 3);
    assert_eq!(out.used_exposure_lamports, 20);
    // Losers' net summed as forgone: 200 + 100 = 300.
    assert_eq!(out.forgone_opportunity_cost_lamports, 300);
    assert_eq!(out.rejected_below_floor, 0);

    // Edge: zero slots -> nothing awarded, every eligible candidate forgone.
    let zero = ArbitrationParams {
        max_slots: 0,
        exposure_cap_lamports: 1_000,
        min_expected_net_lamports: 0,
    };
    let out0 = arbitrate(&[c(1, 100, 10), c(2, 50, 10)], &zero);
    assert!(out0.awarded.is_empty());
    assert_eq!(out0.used_exposure_lamports, 0);
    assert_eq!(out0.forgone_opportunity_cost_lamports, 150);

    // awarded.len never exceeds max_slots even with surplus candidates.
    assert!(out.awarded.len() as u32 <= params.max_slots);
}
