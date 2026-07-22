// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'active_market_universe' component (leaf 'reprioritize_ranks').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::active_market_universe::*;

#[test]
fn reprioritize_ranks() {
    fn cand(token_id: u64, score: u64) -> QualifiedCandidate {
        QualifiedCandidate {
            token_id,
            discovery_source: DiscoverySource::ActiveMarketQualification,
            priority_score: score,
            rank: 999,
        }
    }

    let mut cands = vec![cand(30, 500), cand(10, 500), cand(20, 900)];
    reprioritize(&mut cands);

    assert_eq!(cands[0].token_id, 20);
    assert_eq!(cands[1].token_id, 10);
    assert_eq!(cands[2].token_id, 30);

    assert_eq!(cands[0].rank, 0);
    assert_eq!(cands[1].rank, 1);
    assert_eq!(cands[2].rank, 2);

    for w in cands.windows(2) {
        assert!(w[0].priority_score >= w[1].priority_score);
    }

    let snapshot = cands.clone();
    reprioritize(&mut cands);
    assert_eq!(cands, snapshot);

    let mut empty: Vec<QualifiedCandidate> = Vec::new();
    reprioritize(&mut empty);
    assert!(empty.is_empty());
}
