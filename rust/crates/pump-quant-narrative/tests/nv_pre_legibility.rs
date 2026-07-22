use pump_quant_narrative::{nv_pre_legibility, FP_ONE};

#[test]
fn aggregator_listed_is_zero() {
    // already public => no edge, regardless of youth/breadth.
    assert_eq!(nv_pre_legibility(5, 0, 0, true, 500), 0);
}

#[test]
fn no_sources_is_zero() {
    assert_eq!(nv_pre_legibility(0, 0, 0, false, 500), 0);
}

#[test]
fn young_broad_unlisted_scores_high() {
    // age 0 => raw=FP_ONE; concentration 0 => genuine=FP_ONE.
    // score = 10000 * 10000 / 10000 = 10000.
    assert_eq!(nv_pre_legibility(8, 0, 0, false, 500), FP_ONE);
}

#[test]
fn age_penalty_reduces_linearly() {
    // age 4 windows * age_step 1000 = 4000 penalty -> raw=6000.
    // concentration 0 -> genuine 10000. score = 6000*10000/10000 = 6000.
    assert_eq!(nv_pre_legibility(8, 0, 4, false, 1000), 6000);
}

#[test]
fn concentration_discounts_score() {
    // age 0 => raw=10000. concentration 2500 (25%) => genuine=7500.
    // score = 10000 * 7500 / 10000 = 7500.
    assert_eq!(nv_pre_legibility(8, 2500, 0, false, 1000), 7500);
}

#[test]
fn combined_age_and_concentration() {
    // age 2 * step 1500 = 3000 -> raw=7000. conc 4000 -> genuine=6000.
    // score = 7000 * 6000 / 10000 = 4200.
    assert_eq!(nv_pre_legibility(3, 4000, 2, false, 1500), 4200);
}

#[test]
fn age_penalty_saturates_at_fp_one() {
    // huge age * step saturates penalty to FP_ONE -> raw=0 -> score 0.
    assert_eq!(nv_pre_legibility(8, 0, 1000, false, FP_ONE), 0);
    // concentration >= FP_ONE clamps genuine to 0.
    assert_eq!(nv_pre_legibility(8, FP_ONE + 5, 0, false, 1000), 0);
}
