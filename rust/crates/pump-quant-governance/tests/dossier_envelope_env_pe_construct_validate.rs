// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'envelope' component (leaf 'env_pe_construct_validate').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_governance::envelope::*;

#[test]
fn env_pe_construct_validate() {
    // Rejections, most-specific-first: step<=0 checked before bounds.
    assert_eq!(
        ParameterEnvelope::new(0, 10, 0),
        Err(EnvelopeError::ZeroStep)
    );
    assert_eq!(
        ParameterEnvelope::new(0, 10, -5),
        Err(EnvelopeError::ZeroStep)
    );
    // Inverted bounds with a valid positive step.
    assert_eq!(
        ParameterEnvelope::new(10, 5, 1),
        Err(EnvelopeError::InvertedBounds)
    );
    // Full i128 span not representable in `max - min`.
    assert_eq!(
        ParameterEnvelope::new(i128::MIN, i128::MAX, 1),
        Err(EnvelopeError::SpanNotRepresentable)
    );

    // Valid construction exposes exact accessors.
    let env = ParameterEnvelope::new(-9, 9, 4).unwrap();
    assert_eq!(env.min(), -9);
    assert_eq!(env.max(), 9);
    assert_eq!(env.step(), 4);

    // contains is the inclusive [min,max] predicate.
    assert!(env.contains(-9));
    assert!(env.contains(9));
    assert!(!env.contains(-10));
    assert!(!env.contains(10));

    // Grid points are min + k*step within [min,max]: -9,-5,-1,3,7.
    assert!(env.is_grid_value(-9));
    assert!(env.is_grid_value(-1));
    assert!(env.is_grid_value(7));
    // In-envelope but off-grid.
    assert!(!env.is_grid_value(-8));
    assert!(!env.is_grid_value(9));
    // Off-grid because out-of-envelope.
    assert!(!env.is_grid_value(11));

    // Degenerate single-point envelope: only min is a grid value.
    let point = ParameterEnvelope::new(100, 100, 1).unwrap();
    assert!(point.contains(100));
    assert!(point.is_grid_value(100));
    assert!(!point.contains(101));
}
