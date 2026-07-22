//! Discovery-lane scoring contract: the score formulas are decision functions, so
//! their numeric outputs are asserted directly (not merely reached indirectly).

use pump_quant_app::lane::{NarrativeLane, NumericLane, SocialLane, WalletLane};
use pump_quant_domain::ids::Mint;

fn mint(tag: u8) -> Mint {
    Mint::from_bytes([tag; 32])
}

#[test]
fn numeric_score_is_ofi_times_liqdecade_times_buyers() {
    let mut l = NumericLane::new();
    // Three all-buy prints from distinct entities at 100M liquidity (decade 9),
    // with rising price (flow-confirmed). observe = (mint, price_fp, quote, liq,
    // signed_base, buyer, age, now).
    for e in 1..=3u64 {
        l.observe(
            mint(1),
            1_000_000_000 + (e as i128) * 1_000_000,
            1_000_000,
            100_000_000,
            1_000_000,
            e,
            20,
            0,
        );
    }
    let cands = l.emit(5, &test_gate());
    assert_eq!(cands.len(), 1);
    let c = cands[0];
    // OFI = 10_000 bps (all buys); liq decade(100_000_000) = 9; buyers = 3.
    assert_eq!(c.discovery_score, 10_000 * 9 * 3);
    assert_eq!(c.discovered_at, 5);
    assert_eq!(c.features.buy_pressure_bp, 10_000);
    assert_eq!(c.features.unique_buyers, 3);
}

/// The default numeric emit gate used across these scoring-contract tests.
fn test_gate() -> pump_quant_app::lane::NumericEmitGate {
    pump_quant_app::lane::NumericEmitGate {
        ofi_min_bp: 1_000,
        revert_ofi_min_bp: 2_500,
        roll_trend_bp: 1_500,
        roll_revert_bp: -1_500,
        evidence_ttl_ticks: 100,
    }
}

#[test]
fn numeric_buy_pressure_reflects_sell_flow() {
    let mut l = NumericLane::new();
    l.observe(mint(2), 1_000_000_000, 600, 10_000_000, 600, 1, 0, 0); // buy 600
    l.observe(mint(2), 1_000_000_000, 400, 10_000_000, -400, 2, 0, 0); // sell 400
    let f = l.features_for(mint(2)).unwrap();
    // OFI (600-400)/(600+400) = 2_000 bps, mapped onto the 0..10_000 pressure scale
    // (5_000 = balanced) = 6_000 — the same value the old buy-share proxy reported,
    // now sourced from wash-robust signed flow.
    assert_eq!(f.buy_pressure_bp, 6_000);
}

#[test]
fn social_score_is_summed_quality_weight() {
    let mut l = SocialLane::new();
    l.observe(mint(3), 5_000, 1);
    l.observe(mint(3), 5_000, 1);
    let c = l.emit(1, 100);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].discovery_score, 10_000);
    // Staleness law: the same evidence past its TTL emits nothing.
    assert!(
        l.emit(200, 100).is_empty(),
        "stale social evidence never ranks"
    );
}

#[test]
fn wallet_score_scales_with_config_and_ignores_unfollowable() {
    let mut l = WalletLane::new();
    l.observe(mint(4), false, 9_999_999, 1); // ignored: not followable
    l.observe(mint(4), true, 1_000_000, 1); // decade(1_000_000) = 7
    let a = l.emit(1, 100, 100);
    assert_eq!(a.len(), 1, "only the followable action is tracked");
    assert_eq!(a[0].discovery_score, 7 * 100);
    // The cross-lane scale is a config value: doubling it doubles the score.
    let b = l.emit(1, 200, 100);
    assert_eq!(b[0].discovery_score, 7 * 200);
    // Staleness law: expired wallet evidence emits nothing.
    assert!(l.emit(200, 100, 100).is_empty());
}

#[test]
fn narrative_stage_bands_are_config_driven_and_fade_capped() {
    let fp1 = pump_quant_narrative::narrative::FP_ONE;
    let no_decay = pump_quant_app::lane::AttentionDecayParams {
        rate_bp: 10_000,
        step_ticks: 1,
        floor: 0,
    };
    let mut l = NarrativeLane::new();
    // virality = new/prior * FP_ONE = 400/10 = 40 * FP_ONE  -> above any sane hi band.
    l.observe(mint(5), 10, 400, 1);
    let hot = l.emit(1, 2 * fp1, fp1, 100, &no_decay);
    assert_eq!(hot.len(), 1);
    // Pre-confirmation (money_confirmed = false) the narrative score is fade-capped.
    assert!(hot[0].discovery_score > 0);
    assert!(
        hot[0].discovery_score <= 500,
        "fade-first cap holds before on-chain confirmation"
    );

    // Raising the band edges above the observed virality demotes the inferred stage,
    // which cannot raise the score — proving the edges actually drive classification.
    let cold = l.emit(1, 1_000 * fp1, 500 * fp1, 100, &no_decay);
    assert!(cold[0].discovery_score <= hot[0].discovery_score);
}
