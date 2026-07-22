// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'envelope' component (leaf 'env_pr_register').
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
use pump_quant_governance::envelope::*;

#[test]
fn env_pr_register() {
    let mut reg = ParameterRegistry::new(2);
    assert!(reg.is_empty());
    assert_eq!(reg.capacity(), 2);
    assert_eq!(reg.len(), 0);

    let env = ParameterEnvelope::new(0, 100, 5).unwrap();

    // Initial off-grid value is snapped to the grid on registration: 12 -> 10.
    reg.register(DimensionId(7), env, 12).unwrap();
    assert_eq!(reg.current(DimensionId(7)), Some(10));
    assert_eq!(reg.len(), 1);
    assert!(!reg.is_empty());

    // Duplicate dimension is rejected without mutating state.
    assert_eq!(
        reg.register(DimensionId(7), env, 0),
        Err(RegistryError::DuplicateDimension)
    );
    assert_eq!(reg.len(), 1);

    // Initial outside the envelope is rejected.
    assert_eq!(
        reg.register(DimensionId(8), env, 999),
        Err(RegistryError::InitialOutOfEnvelope)
    );
    assert_eq!(reg.len(), 1);

    // Fill capacity, then the next distinct dimension exceeds it.
    reg.register(DimensionId(3), env, 0).unwrap();
    assert_eq!(reg.len(), 2);
    assert_eq!(
        reg.register(DimensionId(9), env, 0),
        Err(RegistryError::CapacityExceeded)
    );
    assert_eq!(reg.len(), 2);

    // Entries are kept sorted by dimension id for deterministic iteration.
    let dims: Vec<u32> = reg.parameters().iter().map(|p| p.dimension.0).collect();
    assert_eq!(dims, vec![3, 7]);

    // Envelope/current lookups: known vs unknown dimension.
    assert_eq!(reg.envelope(DimensionId(3)), Some(env));
    assert_eq!(reg.current(DimensionId(3)), Some(0));
    assert_eq!(reg.current(DimensionId(42)), None);
    assert_eq!(reg.envelope(DimensionId(42)), None);
}
