// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'setup_classifier' component (leaf 'precedence_and_rejection').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_signals::setup_classifier::*;

fn base() -> MarketState {
    MarketState {
        price_open_fp: 1_000,
        price_fp: 1_000,
        high_extreme_fp: 1_000,
        low_extreme_fp: 1_000,
        prior_high_fp: 2_000,
        prior_low_fp: 500,
        vwap_fp: 1_000,
        cvd_delta: 0,
        range_bps: 10,
        prior_range_bps: 10,
    }
}

#[test]
fn classify_setup_precedence_and_rejections() {
    let t = SetupThresholds::neutral();

    let mut s = base();
    s.prior_low_fp = 1_000;
    s.vwap_fp = 1_000;
    s.low_extreme_fp = 940;
    s.price_fp = 1_010;
    s.price_open_fp = 1_000;
    s.high_extreme_fp = 1_010;
    s.cvd_delta = 5_000;
    assert_eq!(classify_setup(&s, &t), SetupFamily::FailedBreakdownReversal);

    let mut s = base();
    s.prior_low_fp = 1_000;
    s.low_extreme_fp = 900;
    s.price_fp = 950;
    s.cvd_delta = 5_000;
    assert_ne!(classify_setup(&s, &t), SetupFamily::FailedBreakdownReversal);

    let mut s = base();
    s.prior_high_fp = 1_000;
    s.high_extreme_fp = 1_080;
    s.price_fp = 1_030;
    s.low_extreme_fp = 1_000;
    s.cvd_delta = 100;
    assert_ne!(classify_setup(&s, &t), SetupFamily::BreakoutRetest);

    let mut s = base();
    s.prior_range_bps = 300;
    s.range_bps = 2_000;
    s.cvd_delta = 0;
    s.prior_low_fp = 100;
    s.prior_high_fp = 5_000;
    s.low_extreme_fp = 1_000;
    assert_ne!(classify_setup(&s, &t), SetupFamily::CompressionExpansion);
}
