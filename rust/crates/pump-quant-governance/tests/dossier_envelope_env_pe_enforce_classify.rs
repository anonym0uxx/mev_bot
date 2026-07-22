// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'envelope' component (leaf 'env_pe_enforce_classify').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_governance::envelope::*;

#[test]
fn env_pe_enforce_classify() {
    // Grid points 0,4,8 (min=0,max=10,step=4); 10 is in-bounds but off-grid.
    let env = ParameterEnvelope::new(0, 10, 4).unwrap();
    let current = 4;

    let cases: &[(i128, EnforcementMode, ChangeOutcome, i128)] = &[
        (0, EnforcementMode::Clamp, ChangeOutcome::Accepted, 0),
        (8, EnforcementMode::Reject, ChangeOutcome::Accepted, 8),
        (3, EnforcementMode::Clamp, ChangeOutcome::Snapped, 4),
        (5, EnforcementMode::Reject, ChangeOutcome::Snapped, 4),
        // Ties resolve toward min.
        (2, EnforcementMode::Clamp, ChangeOutcome::Snapped, 0),
        (6, EnforcementMode::Clamp, ChangeOutcome::Snapped, 4),
        // In-bounds above the top grid point 8 snaps down to 8.
        (10, EnforcementMode::Clamp, ChangeOutcome::Snapped, 8),
        // Below min.
        (-3, EnforcementMode::Clamp, ChangeOutcome::Clamped, 0),
        (-3, EnforcementMode::Reject, ChangeOutcome::Rejected, 4),
        // Above max clamps to largest grid <= max = 8.
        (99, EnforcementMode::Clamp, ChangeOutcome::Clamped, 8),
        (99, EnforcementMode::Reject, ChangeOutcome::Rejected, 4),
    ];
    for &(proposed, mode, exp_outcome, exp_value) in cases {
        let d = env.enforce(proposed, current, mode);
        assert_eq!(d.outcome, exp_outcome, "proposed={proposed} mode={mode:?}");
        assert_eq!(d.value, exp_value, "proposed={proposed} mode={mode:?}");
    }

    // Invariant sweep: a non-Rejected result is always in-envelope AND on grid;
    // a Rejected result returns exactly `current` and only for crossings.
    let envelopes = [
        ParameterEnvelope::new(0, 10, 1).unwrap(),
        ParameterEnvelope::new(0, 10, 3).unwrap(),
        ParameterEnvelope::new(-50, 50, 7).unwrap(),
        ParameterEnvelope::new(-9, 9, 4).unwrap(),
    ];
    for env in envelopes {
        let cur = env.min();
        let mut proposed = env.min() - 20;
        while proposed <= env.max() + 20 {
            for mode in [EnforcementMode::Clamp, EnforcementMode::Reject] {
                let d = env.enforce(proposed, cur, mode);
                match d.outcome {
                    ChangeOutcome::Rejected => {
                        assert_eq!(d.value, cur);
                        assert!(!env.contains(proposed));
                        assert_eq!(mode, EnforcementMode::Reject);
                    }
                    _ => {
                        assert!(env.contains(d.value));
                        assert!(env.is_grid_value(d.value));
                    }
                }
            }
            proposed += 1;
        }
    }
}
