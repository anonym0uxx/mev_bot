// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'fdr' component (leaf 'blocks_promotion').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::fdr::*;

#[test]
fn blocks_promotion_matches_discovery_membership() {
    // Concrete: discovered id not blocked; undiscovered blocked; unknown blocked.
    let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
    assert!(!blocks_promotion(&fam, 50_000, 1));
    assert!(blocks_promotion(&fam, 50_000, 2));
    assert!(blocks_promotion(&fam, 50_000, 99));

    // Rejection edge: empty family blocks every candidate.
    assert!(blocks_promotion(&[], 50_000, 1));

    // Property: blocks_promotion(id) == !discovered.contains(id) for all ids/alphas.
    let fam: Vec<Hypothesis> = (0..8u64)
        .map(|k| Hypothesis::new(k + 1, ((k * 60_000 + 1_000) % 1_000_001) as u32))
        .collect();
    for a in (0u32..=250_000).step_by(10_000) {
        let disc = benjamini_hochberg(&fam, a).discovered;
        for cand in 0u64..=10 {
            let blocked = blocks_promotion(&fam, a, cand);
            assert_eq!(blocked, !disc.contains(&cand), "alpha={a} cand={cand}");
        }
    }
}
