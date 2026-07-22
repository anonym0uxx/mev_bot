// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_backpressure_stale').
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
use pump_quant_strategy::safety_integrity::*;

#[test]
fn stale_rejected_fresh_accepted_boundary_defined() {
    // fresh
    assert_eq!(admit_or_stale(1000, 1500, 1000), Admission::Accept);
    // stale (age 2000 > 1000)
    assert_eq!(admit_or_stale(1000, 3000, 1000), Admission::RejectStale);
    // boundary: age exactly max_age -> Accept
    assert_eq!(admit_or_stale(1000, 2000, 1000), Admission::Accept);
    // just over boundary -> RejectStale
    assert_eq!(admit_or_stale(1000, 2001, 1000), Admission::RejectStale);
    // future timestamp saturates to age 0 -> Accept
    assert_eq!(admit_or_stale(5000, 1000, 1000), Admission::Accept);
}
