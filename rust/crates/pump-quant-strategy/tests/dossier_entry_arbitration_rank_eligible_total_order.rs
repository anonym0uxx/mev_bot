// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'entry_arbitration' component (leaf 'rank_eligible_total_order').
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
fn rank_eligible_total_order_and_floor() {
    // Higher net first; equal net broken by ascending candidate_id.
    let cands = [c(5, 100, 10), c(2, 300, 10), c(9, 100, 10)];
    let (ranked, rejected) = rank_eligible(&cands, 0);
    assert_eq!(rejected, 0);
    let ids: Vec<u64> = ranked.iter().map(|x| x.candidate_id).collect();
    assert_eq!(ids, vec![2, 5, 9]);

    // Floor is strict (> floor). net == floor and net < floor are rejected.
    let cands2 = [c(1, 500, 10), c(2, 0, 10), c(3, -50, 10)];
    let (ranked2, rejected2) = rank_eligible(&cands2, 0);
    assert_eq!(rejected2, 2);
    assert_eq!(ranked2.len(), 1);
    assert_eq!(ranked2[0].candidate_id, 1);

    // Order independence: shuffled input yields identical ranking.
    let a = [c(1, 100, 10), c(2, 400, 10), c(3, 300, 10)];
    let b = [c(3, 300, 10), c(1, 100, 10), c(2, 400, 10)];
    let (ra, _) = rank_eligible(&a, 0);
    let (rb, _) = rank_eligible(&b, 0);
    assert_eq!(ra, rb);

    // Edge: empty slice -> empty ranking, zero rejected.
    let (re, rej_e) = rank_eligible(&[], 0);
    assert!(re.is_empty());
    assert_eq!(rej_e, 0);

    // eligible count + rejected count == total input.
    assert_eq!(ranked2.len() as u32 + rejected2, 3);
}
