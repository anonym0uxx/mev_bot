use pump_quant_narrative::{nv_virality_coeff, FP_ONE};

#[test]
fn supercritical_spread_above_one() {
    // 100 prior active, 250 new -> 2.5x -> 25_000 fp.
    assert_eq!(nv_virality_coeff(100, 250), Some(25_000));
    assert!(nv_virality_coeff(100, 250).unwrap() > FP_ONE);
}

#[test]
fn exactly_one_is_fp_one() {
    // equal counts -> 1.0.
    assert_eq!(nv_virality_coeff(500, 500), Some(FP_ONE));
}

#[test]
fn subcritical_below_one() {
    // 200 prior, 50 new -> 0.25 -> 2_500 fp.
    assert_eq!(nv_virality_coeff(200, 50), Some(2_500));
    assert!(nv_virality_coeff(200, 50).unwrap() < FP_ONE);
}

#[test]
fn zero_prior_is_undefined_none() {
    assert_eq!(nv_virality_coeff(0, 100), None);
    assert_eq!(nv_virality_coeff(0, 0), None);
}

#[test]
fn zero_new_is_dead_cascade_zero() {
    assert_eq!(nv_virality_coeff(100, 0), Some(0));
}

#[test]
fn integer_truncation_matches_hand_computation() {
    // 3 prior, 10 new -> 10*10000/3 = 33333 (floor).
    assert_eq!(nv_virality_coeff(3, 10), Some(33_333));
}

#[test]
fn large_inputs_saturate_not_panic() {
    // new=u64::MAX, prior=1 -> u64::MAX*10000 overflows u64 -> saturates.
    assert_eq!(nv_virality_coeff(1, u64::MAX), Some(u64::MAX));
}
