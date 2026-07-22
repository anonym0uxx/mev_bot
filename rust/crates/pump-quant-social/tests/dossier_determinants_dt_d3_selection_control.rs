// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'determinants' component (leaf 'dt_d3_selection_control').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_social::determinants::*;

#[test]
fn dt_d3_selection_prop() {
    const HL: u64 = 1_000_000_000;
    // excess = (5000-1000, 2000-3000) = (4000,-1000) age0 -> mean 1500.
    // conf = 10000*2/(2+25) = 740.
    let samples = [
        SelectionSample {
            call_markout_bps: 5_000,
            control_markout_bps: 1_000,
            age_ns: 0,
        },
        SelectionSample {
            call_markout_bps: 2_000,
            control_markout_bps: 3_000,
            age_ns: 0,
        },
    ];
    let s = d3_selection_control(&samples, HL);
    assert_eq!(s.value_bps, 1_500);
    assert_eq!(s.sample_size, 2);
    assert_eq!(s.confidence_bps, 740);

    // Pure-selection account (call == control) -> exactly zero excess regardless of level.
    let selection_only: Vec<SelectionSample> = (0..10)
        .map(|i| SelectionSample {
            call_markout_bps: 3_000 + i * 100,
            control_markout_bps: 3_000 + i * 100,
            age_ns: 0,
        })
        .collect();
    let z = d3_selection_control(&selection_only, HL);
    assert_eq!(z.value_bps, 0);
    assert_eq!(z.sample_size, 10);

    // Edge: empty -> empty score.
    let e = d3_selection_control(&[], HL);
    assert_eq!(e.value_bps, 0);
    assert_eq!(e.sample_size, 0);
    assert_eq!(e.confidence_bps, 0);
}
