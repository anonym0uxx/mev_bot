//! Leaf: `envelope`. Bounds enforcement (§56.2): construction validation,
//! clamp/reject/snap classification with hand-computed expectations across many
//! inputs and edge cases, and the memory-bounded registry.

use pump_quant_governance::envelope::{
    ChangeOutcome, DimensionId, EnforcementMode, EnvelopeError, ParameterEnvelope,
    ParameterRegistry, RegistryError,
};

#[test]
fn construction_validation() {
    assert_eq!(
        ParameterEnvelope::new(10, 5, 1),
        Err(EnvelopeError::InvertedBounds)
    );
    assert_eq!(
        ParameterEnvelope::new(0, 10, 0),
        Err(EnvelopeError::ZeroStep)
    );
    assert_eq!(
        ParameterEnvelope::new(-1, 10, -2),
        Err(EnvelopeError::ZeroStep)
    );
    // Full i128 span is not representable as `max - min`.
    assert_eq!(
        ParameterEnvelope::new(i128::MIN, i128::MAX, 1),
        Err(EnvelopeError::SpanNotRepresentable)
    );
    assert!(ParameterEnvelope::new(0, 10, 1).is_ok());
}

/// Enforcement classification across many hand-computed cases, including grid
/// ties (resolved toward `min`), boundary clamps, and rejects.
#[test]
fn enforce_classification_table() {
    // Grid points: 0, 4, 8 (min=0, max=10, step=4). 10 is in-bounds but off-grid.
    let env = ParameterEnvelope::new(0, 10, 4).unwrap();
    let current = 4;

    // (proposed, mode, expected_outcome, expected_value)
    let cases: &[(i128, EnforcementMode, ChangeOutcome, i128)] = &[
        // In-envelope, exactly on grid.
        (0, EnforcementMode::Clamp, ChangeOutcome::Accepted, 0),
        (8, EnforcementMode::Reject, ChangeOutcome::Accepted, 8),
        // Off-grid, snapped to nearest.
        (3, EnforcementMode::Clamp, ChangeOutcome::Snapped, 4),
        (5, EnforcementMode::Reject, ChangeOutcome::Snapped, 4),
        // Ties resolve toward min: 2 is equidistant 0/4 -> 0; 6 -> 4.
        (2, EnforcementMode::Clamp, ChangeOutcome::Snapped, 0),
        (6, EnforcementMode::Clamp, ChangeOutcome::Snapped, 4),
        // In-bounds but above the top grid point 8: snaps to 8.
        (10, EnforcementMode::Clamp, ChangeOutcome::Snapped, 8),
        // Below min: clamp -> min(0); reject -> current(4).
        (-3, EnforcementMode::Clamp, ChangeOutcome::Clamped, 0),
        (-3, EnforcementMode::Reject, ChangeOutcome::Rejected, 4),
        // Above max: clamp -> largest grid <= max = 8; reject -> current(4).
        (99, EnforcementMode::Clamp, ChangeOutcome::Clamped, 8),
        (99, EnforcementMode::Reject, ChangeOutcome::Rejected, 4),
    ];

    for &(proposed, mode, exp_outcome, exp_value) in cases {
        let d = env.enforce(proposed, current, mode);
        assert_eq!(d.outcome, exp_outcome, "proposed={proposed} mode={mode:?}");
        assert_eq!(d.value, exp_value, "proposed={proposed} mode={mode:?}");
    }
}

/// Negative-domain envelope with step 1 (every integer allowed).
#[test]
fn negative_domain_step_one() {
    let env = ParameterEnvelope::new(-500, -100, 1).unwrap();
    // In range, on grid (step 1).
    let d = env.enforce(-300, -200, EnforcementMode::Reject);
    assert_eq!(d.outcome, ChangeOutcome::Accepted);
    assert_eq!(d.value, -300);
    // Below min.
    let d = env.enforce(-999, -200, EnforcementMode::Clamp);
    assert_eq!((d.outcome, d.value), (ChangeOutcome::Clamped, -500));
    // Above max.
    let d = env.enforce(0, -200, EnforcementMode::Reject);
    assert_eq!((d.outcome, d.value), (ChangeOutcome::Rejected, -200));
}

/// Property (invariant over many inputs): every enforced result is either the
/// unchanged `current` (only on Reject) or an in-envelope grid value. Inputs are
/// a deterministic sweep, not RNG.
#[test]
fn enforced_result_is_always_in_envelope_and_on_grid() {
    let envelopes = [
        ParameterEnvelope::new(0, 10, 1).unwrap(),
        ParameterEnvelope::new(0, 10, 3).unwrap(),
        ParameterEnvelope::new(-50, 50, 7).unwrap(),
        ParameterEnvelope::new(100, 100, 1).unwrap(), // degenerate single point
        ParameterEnvelope::new(-9, 9, 4).unwrap(),
    ];
    for env in envelopes {
        let current = env.min(); // always a valid grid value
        let mut proposed = env.min().saturating_sub(20);
        while proposed <= env.max().saturating_add(20) {
            for mode in [EnforcementMode::Clamp, EnforcementMode::Reject] {
                let d = env.enforce(proposed, current, mode);
                match d.outcome {
                    ChangeOutcome::Rejected => {
                        assert_eq!(d.value, current);
                        assert!(!env.contains(proposed)); // only crossings reject
                    }
                    _ => {
                        assert!(env.contains(d.value), "value {} not in env", d.value);
                        assert!(env.is_grid_value(d.value), "value {} off grid", d.value);
                    }
                }
            }
            proposed += 1;
        }
    }
}

#[test]
fn registry_lifecycle() {
    let mut reg = ParameterRegistry::new(2);
    assert!(reg.is_empty());

    let env = ParameterEnvelope::new(0, 100, 5).unwrap();
    // Initial off-grid is snapped: 12 -> 10.
    reg.register(DimensionId(7), env, 12).unwrap();
    assert_eq!(reg.current(DimensionId(7)), Some(10));
    assert_eq!(reg.len(), 1);

    // Duplicate dimension rejected.
    assert_eq!(
        reg.register(DimensionId(7), env, 0),
        Err(RegistryError::DuplicateDimension)
    );
    // Initial out of envelope rejected.
    assert_eq!(
        reg.register(DimensionId(8), env, 999),
        Err(RegistryError::InitialOutOfEnvelope)
    );

    // Second registration fills capacity; third exceeds it.
    reg.register(DimensionId(3), env, 0).unwrap();
    assert_eq!(
        reg.register(DimensionId(9), env, 0),
        Err(RegistryError::CapacityExceeded)
    );

    // Deterministic sorted iteration by dimension id.
    let dims: Vec<u32> = reg.parameters().iter().map(|p| p.dimension.0).collect();
    assert_eq!(dims, vec![3, 7]);

    // Propose in-envelope snap updates current.
    let d = reg
        .propose(DimensionId(7), 23, EnforcementMode::Clamp)
        .unwrap();
    assert_eq!((d.outcome, d.value), (ChangeOutcome::Snapped, 25));
    assert_eq!(reg.current(DimensionId(7)), Some(25));

    // Reject leaves current unchanged.
    let d = reg
        .propose(DimensionId(7), 10_000, EnforcementMode::Reject)
        .unwrap();
    assert_eq!(d.outcome, ChangeOutcome::Rejected);
    assert_eq!(reg.current(DimensionId(7)), Some(25));

    // Clamp above max pins to the top grid value (100 is on grid).
    let d = reg
        .propose(DimensionId(7), 10_000, EnforcementMode::Clamp)
        .unwrap();
    assert_eq!((d.outcome, d.value), (ChangeOutcome::Clamped, 100));
    assert_eq!(reg.current(DimensionId(7)), Some(100));

    // Unknown dimension errors.
    assert_eq!(
        reg.propose(DimensionId(42), 0, EnforcementMode::Clamp),
        Err(RegistryError::UnknownDimension)
    );
}
