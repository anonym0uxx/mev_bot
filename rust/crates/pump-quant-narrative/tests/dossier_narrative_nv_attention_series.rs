// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'narrative' component (leaf 'nv_attention_series').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::narrative::*;

#[test]
fn nv_as_derivatives_computed() {
    // window 1, need 3 samples. levels: [10, 30, 45]
    // last=45, mid=30, first=10; velocity=15, velocity_prev=20, accel=-5.
    let out = nv_attention_series(&[10, 30, 45], 1).unwrap();
    assert_eq!(
        out,
        AttentionSeries {
            level: 45,
            velocity: 15,
            acceleration: -5
        }
    );

    // window 2, need 5 samples. [0, 0, 100, 0, 400]
    // last=400 (idx4), mid=samples[4-2]=100, first=samples[4-4]=0.
    // velocity=400-100=300, velocity_prev=100-0=100, accel=200.
    let out2 = nv_attention_series(&[0, 0, 100, 0, 400], 2).unwrap();
    assert_eq!(
        out2,
        AttentionSeries {
            level: 400,
            velocity: 300,
            acceleration: 200
        }
    );
}

#[test]
fn nv_as_rejects_insufficient_or_zero_window() {
    // window 0 => None.
    assert_eq!(nv_attention_series(&[1, 2, 3, 4, 5], 0), None);
    // window 1 needs 3 samples; only 2 given => None.
    assert_eq!(nv_attention_series(&[10, 20], 1), None);
    // exactly need boundary (3 samples, window 1) => Some.
    assert!(nv_attention_series(&[5, 5, 5], 1).is_some());
    // constant series => zero velocity and acceleration.
    let flat = nv_attention_series(&[7, 7, 7], 1).unwrap();
    assert_eq!(
        flat,
        AttentionSeries {
            level: 7,
            velocity: 0,
            acceleration: 0
        }
    );
}
