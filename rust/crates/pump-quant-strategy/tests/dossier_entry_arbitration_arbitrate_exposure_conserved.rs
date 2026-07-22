// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'entry_arbitration' component (leaf 'arbitrate_exposure_conserved').
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
fn arbitrate_exposure_cap_and_forgone_conservation() {
    let cands = [c(1, 500, 700), c(2, 400, 700), c(3, 300, 200)];
    let params = ArbitrationParams {
        max_slots: 3,
        exposure_cap_lamports: 1_000,
        min_expected_net_lamports: 0,
    };
    let out = arbitrate(&cands, &params);
    // 1 takes 700; 2 needs 700 > 300 remaining -> skipped; 3 fits (200 <= 300).
    let ids: Vec<u64> = out.awarded.iter().map(|a| a.candidate_id).collect();
    assert_eq!(ids, vec![1, 3]);
    // Used exposure never exceeds the cap and equals the sum of awarded sizes.
    assert_eq!(out.used_exposure_lamports, 900);
    assert!(out.used_exposure_lamports <= params.exposure_cap_lamports);
    let sum_awarded: u64 = out.awarded.iter().map(|a| a.size_lamports).sum();
    assert_eq!(out.used_exposure_lamports, sum_awarded);
    // Forgone equals the summed net of eligible-but-unfunded candidates (only id 2).
    assert_eq!(out.forgone_opportunity_cost_lamports, 400);

    // Conservation: awarded-net + forgone-net == total eligible net.
    let awarded_net: i64 = out
        .awarded
        .iter()
        .map(|a| a.expected_net_sol_lamports)
        .sum();
    assert_eq!(
        awarded_net + out.forgone_opportunity_cost_lamports,
        500 + 400 + 300
    );

    // Order independence of the whole outcome.
    let shuffled = [c(3, 300, 200), c(2, 400, 700), c(1, 500, 700)];
    assert_eq!(arbitrate(&cands, &params), arbitrate(&shuffled, &params));
}
