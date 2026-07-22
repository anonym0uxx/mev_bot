// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'fdr' component (leaf 'bh_threshold_ordering').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::fdr::*;

#[test]
fn bh_threshold_and_ordering_consistency() {
    // Concrete BH-1995 case.
    let fam = vec![
        Hypothesis::new(1, 5_000),
        Hypothesis::new(2, 20_000),
        Hypothesis::new(3, 30_000),
        Hypothesis::new(4, 500_000),
    ];
    let r = benjamini_hochberg(&fam, 50_000);
    assert_eq!(r.discovered, vec![1, 2, 3]);
    assert_eq!(r.threshold_ppm, 30_000);
    assert_eq!(r.m, 4);

    // Rejection edge: nothing significant -> empty set, zero threshold.
    let none = benjamini_hochberg(&[Hypothesis::new(1, 900_000)], 50_000);
    assert!(none.discovered.is_empty());
    assert_eq!(none.threshold_ppm, 0);
    assert_eq!(none.m, 1);

    // Property sweep over deterministic families and an alpha grid.
    for m in 1u64..=12 {
        let fam: Vec<Hypothesis> = (0..m)
            .map(|k| Hypothesis::new(m * 100 + k, ((k * 90_000) % 1_000_001) as u32))
            .collect();
        for a in (0u32..=200_000).step_by(10_000) {
            let r = benjamini_hochberg(&fam, a);
            assert_eq!(r.m as u64, m);
            // Discovered ids strictly ascending (sorted + unique).
            for w in r.discovered.windows(2) {
                assert!(w[0] < w[1]);
            }
            if r.discovered.is_empty() {
                assert_eq!(r.threshold_ppm, 0);
            } else {
                // threshold_ppm == max p over discovered.
                let maxp = r
                    .discovered
                    .iter()
                    .map(|id| fam.iter().find(|h| h.id == *id).unwrap().p_ppm)
                    .max()
                    .unwrap();
                assert_eq!(r.threshold_ppm, maxp);
                // Step-up closure: p <= threshold iff discovered.
                for h in &fam {
                    if h.p_ppm <= r.threshold_ppm {
                        assert!(r.discovered.contains(&h.id));
                    } else {
                        assert!(!r.discovered.contains(&h.id));
                    }
                }
            }
        }
    }
}
