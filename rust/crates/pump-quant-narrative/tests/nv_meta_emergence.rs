use pump_quant_narrative::{nv_meta_emergence, MetaEmergence};

#[test]
fn broad_acceleration_emerges() {
    // velocities 30,40,5,-2,60 ; accel_threshold 10.
    // accelerating: 30,40,60 -> 3. sum = 30+40+5-2+60 = 133.
    let m = nv_meta_emergence(&[30, 40, 5, -2, 60], 10, 3);
    assert_eq!(
        m,
        MetaEmergence {
            accelerating_tokens: 3,
            category_velocity: 133,
            emerging: true
        }
    );
}

#[test]
fn insufficient_breadth_does_not_emerge() {
    // only 1 token accelerating but min_breadth 2 -> not emerging.
    let m = nv_meta_emergence(&[50, 1, 1], 10, 2);
    assert_eq!(m.accelerating_tokens, 1);
    assert_eq!(m.category_velocity, 52);
    assert!(!m.emerging);
}

#[test]
fn breadth_met_but_negative_net_velocity_does_not_emerge() {
    // two tokens above threshold but big negatives drag net below zero.
    // 20,20,-100 ; threshold 10 -> accelerating 2, sum=-60.
    let m = nv_meta_emergence(&[20, 20, -100], 10, 2);
    assert_eq!(m.accelerating_tokens, 2);
    assert_eq!(m.category_velocity, -60);
    assert!(!m.emerging);
}

#[test]
fn empty_category_is_quiescent() {
    let m = nv_meta_emergence(&[], 0, 1);
    assert_eq!(
        m,
        MetaEmergence {
            accelerating_tokens: 0,
            category_velocity: 0,
            emerging: false
        }
    );
}

#[test]
fn threshold_is_strict() {
    // velocity == threshold does not count as accelerating.
    let m = nv_meta_emergence(&[10, 10, 11], 10, 1);
    assert_eq!(m.accelerating_tokens, 1); // only the 11.
    assert!(m.emerging);
}

#[test]
fn min_breadth_zero_emerges_on_positive_velocity() {
    // min_breadth 0 -> breadth always satisfied; net positive -> emerging.
    let m = nv_meta_emergence(&[3, 4], 100, 0);
    assert_eq!(m.accelerating_tokens, 0);
    assert_eq!(m.category_velocity, 7);
    assert!(m.emerging);
}

#[test]
fn velocity_sum_saturates() {
    let m = nv_meta_emergence(&[i64::MAX, i64::MAX], -1, 2);
    assert_eq!(m.accelerating_tokens, 2);
    assert_eq!(m.category_velocity, i64::MAX); // saturated, not wrapped.
    assert!(m.emerging);
}
