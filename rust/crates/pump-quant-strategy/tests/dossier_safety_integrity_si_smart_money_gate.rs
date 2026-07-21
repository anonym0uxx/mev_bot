#![allow(unused_imports)]
use pump_quant_strategy::safety_integrity::*;

fn full() -> WalletEvidence {
    WalletEvidence { realized: true, self_dealing: false, external_counterparty: true,
        family_screened: true, luck_filtered: true, lagged_shadow_positive: true,
        publicly_legible: false, inverting_cohort: false }
}

#[test]
fn raw_self_dealing_public_all_unqualified() {
    let unrealized = WalletEvidence { realized: false, ..full() };
    assert_eq!(classify_wallet(&unrealized), SmartMoneyClass::Unqualified);
    let selfdeal = WalletEvidence { self_dealing: true, ..full() };
    assert_eq!(classify_wallet(&selfdeal), SmartMoneyClass::Unqualified);
    let public = WalletEvidence { publicly_legible: true, ..full() };
    assert_eq!(classify_wallet(&public), SmartMoneyClass::Unqualified);
}
#[test]
fn full_gate_qualifies() {
    assert_eq!(classify_wallet(&full()), SmartMoneyClass::Qualified);
}
#[test]
fn missing_a_gate_unqualified() {
    let no_luck = WalletEvidence { luck_filtered: false, ..full() };
    assert_eq!(classify_wallet(&no_luck), SmartMoneyClass::Unqualified);
}
