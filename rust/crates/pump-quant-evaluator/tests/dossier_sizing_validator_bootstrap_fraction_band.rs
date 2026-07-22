// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'sizing_validator' component (leaf 'bootstrap_fraction_band').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_evaluator::sizing_validator::*;

#[test]
fn prop_bootstrap_fraction_band() {
    let returns = vec![10_000i64, -5_000, 8_000, -4_000, 12_000];

    // Deterministic in (returns, seed): identical seed -> byte-identical band.
    let a = bootstrap_fraction_band(&returns, 10_000, 500, 200, 42);
    let b = bootstrap_fraction_band(&returns, 10_000, 500, 200, 42);
    assert_eq!(a, b);

    // Echoes resample count, band is ordered and bounded by the searched ceiling.
    assert_eq!(a.n_resamples, 200);
    assert!(a.p05 <= a.p50 && a.p50 <= a.p95, "band must be ordered");
    assert!(a.p95 <= 10_000);

    // Empty input -> all-zero band.
    let e = bootstrap_fraction_band(&[], 10_000, 500, 10, 1);
    assert_eq!(
        e,
        Band {
            p05: 0,
            p50: 0,
            p95: 0,
            n_resamples: 0
        }
    );

    // Zero resamples -> all-zero band regardless of returns.
    let z = bootstrap_fraction_band(&[1_000i64], 10_000, 500, 0, 1);
    assert_eq!(
        z,
        Band {
            p05: 0,
            p50: 0,
            p95: 0,
            n_resamples: 0
        }
    );
}
