use pump_quant_narrative::{nv_lifecycle_stage, AttentionSeries, LifecycleStage, FP_ONE};

fn series(level: u64, velocity: i64, acceleration: i64) -> AttentionSeries {
    AttentionSeries {
        level,
        velocity,
        acceleration,
    }
}

#[test]
fn negative_velocity_is_decay_regardless() {
    // even above floor & viral, negative velocity => Decay (rule 1 first).
    let s = series(1000, -5, 3);
    assert_eq!(
        nv_lifecycle_stage(&s, 3 * FP_ONE, 100),
        LifecycleStage::Decay
    );
}

#[test]
fn below_floor_is_formation() {
    // level 50 < floor 100, non-negative velocity.
    let s = series(50, 4, 4);
    assert_eq!(
        nv_lifecycle_stage(&s, 2 * FP_ONE, 100),
        LifecycleStage::Formation
    );
}

#[test]
fn viral_when_coeff_ge_one_and_accel_nonneg() {
    let s = series(500, 20, 0); // accel == 0 still qualifies (>= 0).
    assert_eq!(
        nv_lifecycle_stage(&s, FP_ONE, 100),
        LifecycleStage::Virality
    );
}

#[test]
fn emergence_when_accel_positive_but_not_viral() {
    let s = series(500, 20, 5);
    // coeff below 1.0 -> not viral, accel>0 -> Emergence.
    assert_eq!(
        nv_lifecycle_stage(&s, FP_ONE - 1, 100),
        LifecycleStage::Emergence
    );
}

#[test]
fn saturation_when_above_floor_decelerating() {
    let s = series(500, 20, -3); // vel>=0, accel<0, not viral.
    assert_eq!(nv_lifecycle_stage(&s, 0, 100), LifecycleStage::Saturation);
}

#[test]
fn viral_precedence_over_emergence() {
    // accel positive AND viral -> Virality wins (rule ordering).
    let s = series(500, 20, 8);
    assert_eq!(
        nv_lifecycle_stage(&s, 2 * FP_ONE, 100),
        LifecycleStage::Virality
    );
}

#[test]
fn floor_precedence_over_virality() {
    // viral coeff but below floor -> Formation (floor rule first).
    let s = series(10, 2, 2);
    assert_eq!(
        nv_lifecycle_stage(&s, 5 * FP_ONE, 100),
        LifecycleStage::Formation
    );
}
