use pump_quant_narrative::{nv_attention_series, AttentionSeries};

#[test]
fn linear_growth_has_constant_velocity_zero_accel() {
    // samples: 0,10,20,30,40 ; window 1.
    // level=40, velocity=40-30=10, velocity_prev=30-20=10, accel=0.
    let s = nv_attention_series(&[0, 10, 20, 30, 40], 1).unwrap();
    assert_eq!(
        s,
        AttentionSeries {
            level: 40,
            velocity: 10,
            acceleration: 0
        }
    );
}

#[test]
fn quadratic_growth_has_positive_accel() {
    // samples: 0,1,4,9,16 (squares); window 1.
    // level=16, velocity=16-9=7, velocity_prev=9-4=5, accel=7-5=2.
    let s = nv_attention_series(&[0, 1, 4, 9, 16], 1).unwrap();
    assert_eq!(s.level, 16);
    assert_eq!(s.velocity, 7);
    assert_eq!(s.acceleration, 2);
}

#[test]
fn window_two_uses_correct_lookbacks() {
    // samples idx: [5,10,20,35,55] ; window 2 needs 2*2+1=5 samples.
    // level=55, mid=samples[4-2]=20, first=samples[4-4]=5.
    // velocity=55-20=35, velocity_prev=20-5=15, accel=35-15=20.
    let s = nv_attention_series(&[5, 10, 20, 35, 55], 2).unwrap();
    assert_eq!(s.level, 55);
    assert_eq!(s.velocity, 35);
    assert_eq!(s.acceleration, 20);
}

#[test]
fn declining_series_negative_velocity() {
    // 100,80,60 ; window 1. level=60, vel=60-80=-20, vel_prev=80-100=-20, accel=0.
    let s = nv_attention_series(&[100, 80, 60], 1).unwrap();
    assert_eq!(s.velocity, -20);
    assert_eq!(s.acceleration, 0);
}

#[test]
fn insufficient_samples_and_zero_window_return_none() {
    assert!(nv_attention_series(&[1, 2], 1).is_none()); // need 3, have 2.
    assert!(nv_attention_series(&[1, 2, 3, 4], 0).is_none()); // window 0.
    assert!(nv_attention_series(&[], 1).is_none());
    // exactly enough is fine:
    assert!(nv_attention_series(&[1, 2, 3], 1).is_some());
}

#[test]
fn large_values_saturate_not_wrap() {
    // huge jump down then flat: level small, velocity very negative but bounded.
    let s = nv_attention_series(&[u64::MAX, 0, 0], 1).unwrap();
    assert_eq!(s.level, 0);
    assert_eq!(s.velocity, 0); // 0-0
                               // velocity_prev = 0 - u64::MAX saturates to i64::MIN; accel = 0 - i64::MIN
                               // saturates to i64::MAX.
    assert_eq!(s.acceleration, i64::MAX);
}
