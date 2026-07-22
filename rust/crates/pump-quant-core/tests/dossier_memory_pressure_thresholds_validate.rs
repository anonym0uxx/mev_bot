// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'memory_pressure' component (leaf 'thresholds_validate').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::memory_pressure::*;

#[test]
fn thresholds_validate_prop() {
    // Default set is valid.
    assert_eq!(PressureThresholds::default().validate(), Ok(()));

    // Rejection: zero budget.
    let mut z = PressureThresholds::default();
    z.budget_bytes = 0;
    assert_eq!(z.validate(), Err(ThresholdError::ZeroBudget));

    // Rejection: RSS cuts not strictly increasing.
    let mut r = PressureThresholds::default();
    r.rss_hard_bps = r.rss_soft_bps;
    assert_eq!(r.validate(), Err(ThresholdError::RssNotMonotone));

    // Rejection: available floors not strictly decreasing.
    let mut a = PressureThresholds::default();
    a.avail_hard_bytes = a.avail_soft_bytes;
    assert_eq!(a.validate(), Err(ThresholdError::AvailNotMonotone));

    // Acceptance: a minimal strictly-ordered custom set validates.
    let ok = PressureThresholds {
        version: 5,
        budget_bytes: 2_000_000,
        rss_soft_bps: 1,
        rss_hard_bps: 2,
        rss_critical_bps: 3,
        avail_soft_bytes: 30,
        avail_hard_bytes: 20,
        avail_critical_bytes: 10,
    };
    assert_eq!(ok.validate(), Ok(()));
}
