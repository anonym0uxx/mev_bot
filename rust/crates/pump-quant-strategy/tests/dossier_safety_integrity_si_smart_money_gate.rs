// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_smart_money_gate').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_strategy::safety_integrity::*;

fn full() -> WalletEvidence {
    WalletEvidence {
        realized: true,
        self_dealing: false,
        external_counterparty: true,
        family_screened: true,
        luck_filtered: true,
        lagged_shadow_positive: true,
        publicly_legible: false,
        inverting_cohort: false,
    }
}

#[test]
fn raw_self_dealing_public_all_unqualified() {
    let unrealized = WalletEvidence {
        realized: false,
        ..full()
    };
    assert_eq!(classify_wallet(&unrealized), SmartMoneyClass::Unqualified);
    let selfdeal = WalletEvidence {
        self_dealing: true,
        ..full()
    };
    assert_eq!(classify_wallet(&selfdeal), SmartMoneyClass::Unqualified);
    let public = WalletEvidence {
        publicly_legible: true,
        ..full()
    };
    assert_eq!(classify_wallet(&public), SmartMoneyClass::Unqualified);
}
#[test]
fn full_gate_qualifies() {
    assert_eq!(classify_wallet(&full()), SmartMoneyClass::Qualified);
}
#[test]
fn missing_a_gate_unqualified() {
    let no_luck = WalletEvidence {
        luck_filtered: false,
        ..full()
    };
    assert_eq!(classify_wallet(&no_luck), SmartMoneyClass::Unqualified);
}
