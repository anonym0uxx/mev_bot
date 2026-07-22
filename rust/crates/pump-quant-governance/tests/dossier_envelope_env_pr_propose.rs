// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'envelope' component (leaf 'env_pr_propose').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_governance::envelope::*;

#[test]
fn env_pr_propose() {
    let mut reg = ParameterRegistry::new(4);
    let env = ParameterEnvelope::new(0, 100, 5).unwrap();
    reg.register(DimensionId(7), env, 12).unwrap();
    assert_eq!(reg.current(DimensionId(7)), Some(10));

    // In-envelope off-grid: snapped, current advances to grid value 25.
    let d = reg
        .propose(DimensionId(7), 23, EnforcementMode::Clamp)
        .unwrap();
    assert_eq!((d.outcome, d.value), (ChangeOutcome::Snapped, 25));
    assert_eq!(reg.current(DimensionId(7)), Some(25));

    // Reject leaves current untouched (envelope crossing needs the slow path).
    let d = reg
        .propose(DimensionId(7), 10_000, EnforcementMode::Reject)
        .unwrap();
    assert_eq!(d.outcome, ChangeOutcome::Rejected);
    assert_eq!(d.value, 25);
    assert_eq!(reg.current(DimensionId(7)), Some(25));

    // Clamp above max pins to the top grid value (100 is on grid) and persists.
    let d = reg
        .propose(DimensionId(7), 10_000, EnforcementMode::Clamp)
        .unwrap();
    assert_eq!((d.outcome, d.value), (ChangeOutcome::Clamped, 100));
    assert_eq!(reg.current(DimensionId(7)), Some(100));

    // In-envelope on-grid: accepted verbatim.
    let d = reg
        .propose(DimensionId(7), 40, EnforcementMode::Reject)
        .unwrap();
    assert_eq!((d.outcome, d.value), (ChangeOutcome::Accepted, 40));
    assert_eq!(reg.current(DimensionId(7)), Some(40));

    // Unknown dimension errors and does not create state.
    assert_eq!(
        reg.propose(DimensionId(42), 0, EnforcementMode::Clamp),
        Err(RegistryError::UnknownDimension)
    );
    assert_eq!(reg.current(DimensionId(42)), None);
}
