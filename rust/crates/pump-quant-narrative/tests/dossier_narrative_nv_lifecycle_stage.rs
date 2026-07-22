// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'narrative' component (leaf 'nv_lifecycle_stage').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_narrative::narrative::*;

fn s(level: u64, velocity: i64, acceleration: i64) -> AttentionSeries {
    AttentionSeries {
        level,
        velocity,
        acceleration,
    }
}

#[test]
fn nv_ls_ordered_rule_precedence() {
    // Rule 1: negative velocity => Decay, even when viral & above floor.
    assert_eq!(
        nv_lifecycle_stage(&s(1000, -1, 9), 3 * FP_ONE, 100),
        LifecycleStage::Decay
    );
    // Rule 2: below floor => Formation, even with viral coeff.
    assert_eq!(
        nv_lifecycle_stage(&s(10, 2, 2), 5 * FP_ONE, 100),
        LifecycleStage::Formation
    );
    // Rule 3: coeff >= FP_ONE and accel >= 0 => Virality (accel==0 boundary qualifies).
    assert_eq!(
        nv_lifecycle_stage(&s(500, 20, 0), FP_ONE, 100),
        LifecycleStage::Virality
    );
    // Rule 4: accel > 0 but not viral => Emergence.
    assert_eq!(
        nv_lifecycle_stage(&s(500, 20, 5), FP_ONE - 1, 100),
        LifecycleStage::Emergence
    );
    // Rule 5: above floor, non-negative velocity, accel <= 0, not viral => Saturation.
    assert_eq!(
        nv_lifecycle_stage(&s(500, 20, -3), 0, 100),
        LifecycleStage::Saturation
    );
}

#[test]
fn nv_ls_boundaries() {
    // level == floor is NOT below floor (strict <), zero velocity/accel, not viral => Saturation.
    assert_eq!(
        nv_lifecycle_stage(&s(100, 0, 0), 0, 100),
        LifecycleStage::Saturation
    );
    // Viral precedence over Emergence when both accel>0 and viral.
    assert_eq!(
        nv_lifecycle_stage(&s(500, 20, 8), 2 * FP_ONE, 100),
        LifecycleStage::Virality
    );
    // Coeff one below FP_ONE with accel>=0 is not viral -> Emergence path.
    assert_eq!(
        nv_lifecycle_stage(&s(500, 1, 1), FP_ONE - 1, 100),
        LifecycleStage::Emergence
    );
}
