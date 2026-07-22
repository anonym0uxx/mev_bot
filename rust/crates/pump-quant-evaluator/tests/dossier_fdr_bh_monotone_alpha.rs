// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'fdr' component (leaf 'bh_monotone_alpha').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::fdr::*;

#[test]
fn bh_monotone_in_alpha_and_deterministic() {
    // m=1 exact: reject iff p <= alpha; threshold == p when rejected.
    let one = vec![Hypothesis::new(42, 10_000)];
    assert_eq!(benjamini_hochberg(&one, 10_000).discovered, vec![42]);
    assert_eq!(benjamini_hochberg(&one, 10_000).threshold_ppm, 10_000);
    // Rejection edge: alpha just below p discovers nothing.
    assert!(benjamini_hochberg(&one, 9_999).discovered.is_empty());

    // Deterministic family; raising alpha never drops an existing discovery,
    // and repeated calls are bit-identical.
    let fam: Vec<Hypothesis> = (0..10u64)
        .map(|k| Hypothesis::new(k, ((k * 40_000 + 3_000) % 1_000_001) as u32))
        .collect();
    let mut prev: Vec<u64> = Vec::new();
    for a in (0u32..=300_000).step_by(5_000) {
        let r1 = benjamini_hochberg(&fam, a);
        let r2 = benjamini_hochberg(&fam, a);
        assert_eq!(r1, r2); // determinism
        for id in &prev {
            assert!(r1.discovered.contains(id), "alpha={a} lost id {id}");
        }
        prev = r1.discovered.clone();
    }

    // At alpha = 1.0 every hypothesis (all p <= 363_000) is discovered.
    let all = benjamini_hochberg(&fam, 1_000_000);
    assert_eq!(all.discovered.len(), 10);
    assert_eq!(all.discovered, (0..10u64).collect::<Vec<_>>());
    assert_eq!(all.threshold_ppm, 363_000);
}
