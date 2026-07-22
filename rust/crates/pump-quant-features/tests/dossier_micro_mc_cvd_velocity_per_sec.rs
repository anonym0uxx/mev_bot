// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'micro' component (leaf 'mc_cvd_velocity_per_sec').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_features::micro::*;

#[test]
fn mc_cvd_velocity_props() {
    // delta 500 over 2s -> 250 quote/sec.
    assert_eq!(cvd_velocity_per_sec(0, 0, 500, 2_000_000_000), Some(250));
    // Negative delta over 1s.
    assert_eq!(
        cvd_velocity_per_sec(100, 0, -900, 1_000_000_000),
        Some(-1000)
    );
    // Non-positive time base -> None (equal and inverted).
    assert_eq!(cvd_velocity_per_sec(0, 5, 10, 5), None);
    assert_eq!(cvd_velocity_per_sec(0, 6, 10, 5), None);
    // Zero delta over positive time -> Some(0).
    assert_eq!(cvd_velocity_per_sec(42, 0, 42, 3_000_000_000), Some(0));
    // Truncating integer division toward zero: delta 3 over 2s.
    // 3 * 1e9 / 2e9 = 1 (truncated).
    assert_eq!(cvd_velocity_per_sec(0, 0, 3, 2_000_000_000), Some(1));
    // Rate is invariant under equal time scaling when delta scales with dt:
    // delta d over exactly 1s always equals d.
    for d in [-1000i128, -7, 0, 1, 999, 1_000_000] {
        // dt = exactly 1 second (1e9 ns) -> rate equals the delta itself.
        assert_eq!(cvd_velocity_per_sec(0, 1_000, d, 1_000_001_000), Some(d));
    }
}
