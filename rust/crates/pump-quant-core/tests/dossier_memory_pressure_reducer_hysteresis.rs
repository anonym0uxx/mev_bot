// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'memory_pressure' component (leaf 'reducer_hysteresis').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::memory_pressure::*;

#[test]
fn reducer_hysteresis_prop() {
    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    let mut r = PressureReducer::new(PressureThresholds::default(), 3);
    assert_eq!(r.level(), PressureLevel::Nominal);

    // Escalate instantly: single severe sample jumps straight to Critical.
    assert_eq!(
        r.observe(&MemorySample::rss(98 * GIB / 100)),
        PressureLevel::Critical
    );
    assert_eq!(r.samples_seen(), 1);
    assert!(r.shed_plan().flush_and_release);

    // De-escalate slowly: exactly `calm_required` consecutive lower samples per
    // one-level step, never more than one level per confirmed streak.
    let calm = MemorySample::rss(10 * MIB);
    r.observe(&calm);
    r.observe(&calm);
    assert_eq!(r.level(), PressureLevel::Critical); // 2 calm insufficient
    assert_eq!(r.observe(&calm), PressureLevel::Hard); // 3rd steps down one
    r.observe(&calm);
    r.observe(&calm);
    assert_eq!(r.level(), PressureLevel::Hard); // streak reset, 2 insufficient
    assert_eq!(r.observe(&calm), PressureLevel::Soft);

    // observe counts every sample.
    assert_eq!(r.samples_seen(), 7);

    // Edge: calm_required clamped to at least 1, so one calm sample relaxes.
    let mut r2 = PressureReducer::new(PressureThresholds::default(), 0);
    r2.observe(&MemorySample::rss(98 * GIB / 100));
    assert_eq!(
        r2.observe(&MemorySample::rss(10 * MIB)),
        PressureLevel::Hard
    );
}
