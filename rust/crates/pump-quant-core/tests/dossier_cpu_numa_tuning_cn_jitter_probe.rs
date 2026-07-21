// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'cpu_numa_tuning' component (leaf 'cn_jitter_probe').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports)]
use pump_quant_core::cpu_numa_tuning::*;

#[test]
fn prop_jitter_percentiles_nearest_rank() {
    let deltas: Vec<u64> = (1..=100).collect();
    let s = jitter_stats(&deltas);
    assert_eq!(s.p50_ns, 50);
    assert_eq!(s.p99_ns, 99);
    assert_eq!(s.p999_ns, 100); // nearest-rank on n=100
    assert_eq!(s.max_ns, 100);
    assert!(jitter_stats(&[]).is_missing());
}
