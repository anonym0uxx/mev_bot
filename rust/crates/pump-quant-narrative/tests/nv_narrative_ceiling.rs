use pump_quant_narrative::{nv_narrative_ceiling, NarrativeClass, FP_ONE};

#[test]
fn news_two_x_neutral_regime() {
    // reach 1000 * 2.0 * 1.0 = 2000.
    assert_eq!(
        nv_narrative_ceiling(NarrativeClass::News, 1000, FP_ONE),
        2000
    );
}

#[test]
fn trend_five_x() {
    // 1000 * 5.0 = 5000.
    assert_eq!(
        nv_narrative_ceiling(NarrativeClass::Trend, 1000, FP_ONE),
        5000
    );
}

#[test]
fn tech_eight_x() {
    assert_eq!(
        nv_narrative_ceiling(NarrativeClass::Tech, 1000, FP_ONE),
        8000
    );
}

#[test]
fn culture_twelve_x_highest() {
    assert_eq!(
        nv_narrative_ceiling(NarrativeClass::Culture, 1000, FP_ONE),
        12000
    );
    // culture ceiling exceeds every other class for identical reach.
    let base = 500;
    assert!(
        nv_narrative_ceiling(NarrativeClass::Culture, base, FP_ONE)
            > nv_narrative_ceiling(NarrativeClass::Tech, base, FP_ONE)
    );
}

#[test]
fn regime_multiplier_scales_result() {
    // regime 1.5x (15000 fp): 1000 * 5.0 * 1.5 = 7500.
    assert_eq!(
        nv_narrative_ceiling(NarrativeClass::Trend, 1000, 15_000),
        7500
    );
    // regime 0.5x halves it: 1000 * 5.0 * 0.5 = 2500.
    assert_eq!(
        nv_narrative_ceiling(NarrativeClass::Trend, 1000, 5_000),
        2500
    );
}

#[test]
fn zero_reach_zero_ceiling() {
    assert_eq!(nv_narrative_ceiling(NarrativeClass::Culture, 0, FP_ONE), 0);
}

#[test]
fn huge_reach_saturates() {
    // u64::MAX * 12 overflows u64 -> saturates, no panic/wrap.
    assert_eq!(
        nv_narrative_ceiling(NarrativeClass::Culture, u64::MAX, FP_ONE),
        u64::MAX
    );
}
