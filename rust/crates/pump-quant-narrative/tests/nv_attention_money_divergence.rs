use pump_quant_narrative::{nv_attention_money_divergence, AttentionMoneyDivergence};

#[test]
fn both_rising_is_confirmed() {
    assert_eq!(
        nv_attention_money_divergence(50, 40, 10),
        AttentionMoneyDivergence::Confirmed
    );
}

#[test]
fn attention_only_leads() {
    assert_eq!(
        nv_attention_money_divergence(50, 5, 10),
        AttentionMoneyDivergence::AttentionLeads
    );
}

#[test]
fn money_only_leads() {
    assert_eq!(
        nv_attention_money_divergence(5, 50, 10),
        AttentionMoneyDivergence::MoneyLeads
    );
}

#[test]
fn neither_is_saturating() {
    assert_eq!(
        nv_attention_money_divergence(-20, 0, 10),
        AttentionMoneyDivergence::Saturating
    );
}

#[test]
fn threshold_is_strict_deadband() {
    // velocity == threshold is NOT rising.
    assert_eq!(
        nv_attention_money_divergence(10, 10, 10),
        AttentionMoneyDivergence::Saturating
    );
    // one tick above flips it.
    assert_eq!(
        nv_attention_money_divergence(11, 10, 10),
        AttentionMoneyDivergence::AttentionLeads
    );
}

#[test]
fn zero_threshold_positive_counts() {
    assert_eq!(
        nv_attention_money_divergence(1, 1, 0),
        AttentionMoneyDivergence::Confirmed
    );
    assert_eq!(
        nv_attention_money_divergence(0, 1, 0),
        AttentionMoneyDivergence::MoneyLeads
    );
}
