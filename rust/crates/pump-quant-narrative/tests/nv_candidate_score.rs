use pump_quant_narrative::{nv_candidate_score, AttentionMoneyDivergence, LifecycleStage, FP_ONE};

#[test]
fn max_confirmed_setup() {
    // Emergence 350 + Confirmed 300 + virality(2.0->200) + prelegibility(FP_ONE->150)
    // = 1000, money_confirmed true -> no cap.
    let s = nv_candidate_score(
        LifecycleStage::Emergence,
        AttentionMoneyDivergence::Confirmed,
        2 * FP_ONE,
        FP_ONE,
        true,
    );
    assert_eq!(s, 1000);
}

#[test]
fn fade_first_caps_unconfirmed() {
    // Same maxed inputs but money_confirmed false -> capped at 500.
    let s = nv_candidate_score(
        LifecycleStage::Emergence,
        AttentionMoneyDivergence::Confirmed,
        2 * FP_ONE,
        FP_ONE,
        false,
    );
    assert_eq!(s, 500);
}

#[test]
fn decay_saturating_scores_zero_components() {
    // Decay 0 + Saturating 0 + virality 0 + prelegibility 0 = 0.
    let s = nv_candidate_score(
        LifecycleStage::Decay,
        AttentionMoneyDivergence::Saturating,
        0,
        0,
        true,
    );
    assert_eq!(s, 0);
}

#[test]
fn partial_components_add_up() {
    // Virality stage 300 + MoneyLeads 120 + virality(1.0 -> 10000*200/20000=100)
    // + prelegibility(0.5 -> 5000*150/10000=75) = 595. money_confirmed true.
    let s = nv_candidate_score(
        LifecycleStage::Virality,
        AttentionMoneyDivergence::MoneyLeads,
        FP_ONE,
        FP_ONE / 2,
        true,
    );
    assert_eq!(s, 300 + 120 + 100 + 75);
}

#[test]
fn virality_band_saturates_at_200() {
    // coeff 5.0 -> 50000*200/20000 = 500 but band clamps to 200.
    // Formation 100 + AttentionLeads 200 + 200 + prelegibility 0 = 500.
    let s = nv_candidate_score(
        LifecycleStage::Formation,
        AttentionMoneyDivergence::AttentionLeads,
        5 * FP_ONE,
        0,
        true,
    );
    assert_eq!(s, 500);
}

#[test]
fn prelegibility_band_clamps_input() {
    // pre_legibility beyond FP_ONE is clamped -> 150 max.
    // Saturation 100 + AttentionLeads 200 + virality 0 + 150 = 450.
    let s = nv_candidate_score(
        LifecycleStage::Saturation,
        AttentionMoneyDivergence::AttentionLeads,
        0,
        FP_ONE * 10,
        true,
    );
    assert_eq!(s, 450);
}

#[test]
fn unconfirmed_below_cap_is_untouched() {
    // raw = Formation 100 + Saturating 0 = 100 < 500, cap does not change it.
    let s = nv_candidate_score(
        LifecycleStage::Formation,
        AttentionMoneyDivergence::Saturating,
        0,
        0,
        false,
    );
    assert_eq!(s, 100);
}
