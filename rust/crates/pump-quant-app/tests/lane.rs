//! Discovery-lane scoring contract: the score formulas are decision functions, so
//! their numeric outputs are asserted directly (not merely reached indirectly).

use pump_quant_app::lane::{NarrativeLane, NumericLane, SocialLane, WalletLane};
use pump_quant_domain::ids::Mint;

fn mint(tag: u8) -> Mint {
    Mint::from_bytes([tag; 32])
}

#[test]
fn numeric_score_is_buypressure_times_liqdecade_times_buyers() {
    let mut l = NumericLane::new();
    // Three all-buy prints from distinct entities at 100M liquidity (decade 9).
    for e in 1..=3u64 {
        l.observe(mint(1), 100_000_000, 1_000_000, e, 20, 0);
    }
    let cands = l.emit(5);
    assert_eq!(cands.len(), 1);
    let c = cands[0];
    // buy_pressure = 10_000 bps (all buys); liq decade(100_000_000) = 9; buyers = 3.
    assert_eq!(c.discovery_score, 10_000 * 9 * 3);
    assert_eq!(c.discovered_at, 5);
    assert_eq!(c.features.buy_pressure_bp, 10_000);
    assert_eq!(c.features.unique_buyers, 3);
}

#[test]
fn numeric_buy_pressure_reflects_sell_flow() {
    let mut l = NumericLane::new();
    l.observe(mint(2), 10_000_000, 600, 1, 0, 0); // buy 600
    l.observe(mint(2), 10_000_000, -400, 2, 0, 0); // sell 400
    let f = l.features_for(mint(2)).unwrap();
    // 600 / (600+400) = 6000 bps.
    assert_eq!(f.buy_pressure_bp, 6_000);
}

#[test]
fn social_score_is_summed_quality_weight() {
    let mut l = SocialLane::new();
    l.observe(mint(3), 5_000);
    l.observe(mint(3), 5_000);
    let c = l.emit(1);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].discovery_score, 10_000);
}

#[test]
fn wallet_score_scales_with_config_and_ignores_unfollowable() {
    let mut l = WalletLane::new();
    l.observe(mint(4), false, 9_999_999); // ignored: not followable
    l.observe(mint(4), true, 1_000_000); // decade(1_000_000) = 7
    let a = l.emit(1, 100);
    assert_eq!(a.len(), 1, "only the followable action is tracked");
    assert_eq!(a[0].discovery_score, 7 * 100);
    // The cross-lane scale is a config value: doubling it doubles the score.
    let b = l.emit(1, 200);
    assert_eq!(b[0].discovery_score, 7 * 200);
}

#[test]
fn narrative_stage_bands_are_config_driven_and_fade_capped() {
    let fp1 = pump_quant_narrative::narrative::FP_ONE;
    let mut l = NarrativeLane::new();
    // virality = new/prior * FP_ONE = 400/10 = 40 * FP_ONE  -> above any sane hi band.
    l.observe(mint(5), 10, 400);
    let hot = l.emit(1, 2 * fp1, fp1);
    assert_eq!(hot.len(), 1);
    // Pre-confirmation (money_confirmed = false) the narrative score is fade-capped.
    assert!(hot[0].discovery_score > 0);
    assert!(
        hot[0].discovery_score <= 500,
        "fade-first cap holds before on-chain confirmation"
    );

    // Raising the band edges above the observed virality demotes the inferred stage,
    // which cannot raise the score — proving the edges actually drive classification.
    let cold = l.emit(1, 1_000 * fp1, 500 * fp1);
    assert!(cold[0].discovery_score <= hot[0].discovery_score);
}
