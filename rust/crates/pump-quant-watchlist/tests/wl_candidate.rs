//! `wl_candidate` leaf tests: typed record, lane priors, evidence strength.

use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};

fn mint(tag: u8) -> Mint {
    let mut b = [0u8; 32];
    b[0] = tag;
    Mint::new(b)
}

#[test]
fn lane_indices_are_dense_and_unique() {
    let mut seen = [false; Lane::COUNT];
    for lane in Lane::ALL {
        let i = lane.index();
        assert!(i < Lane::COUNT);
        assert!(!seen[i], "duplicate index {i}");
        seen[i] = true;
    }
    assert!(seen.iter().all(|&s| s), "indices must cover 0..COUNT");
    assert_eq!(Lane::COUNT, 4);
}

#[test]
fn lane_default_weight_priors_are_exact() {
    // Independently asserted against the documented static-by-design priors.
    assert_eq!(Lane::CreationSniper.default_weight_bp(), 8_000);
    assert_eq!(Lane::EarlyConfirmation.default_weight_bp(), 12_000);
    assert_eq!(Lane::GraduationTransition.default_weight_bp(), 11_000);
    assert_eq!(Lane::ActiveMarketScalp.default_weight_bp(), 10_000);
}

#[test]
fn constructor_stores_all_fields() {
    let f = Features {
        liquidity_lamports: 5_000_000_000,
        buy_pressure_bp: 7_500,
        unique_buyers: 42,
        age_slots: 3,
        buy_ratio_bp: 8_000,
        max_trade_lamports: 500_000_000,
        trades_observed: 42,
        volume_lamports: 0,
        ..Features::default()
    };
    let c = Candidate::new(mint(9), Lane::EarlyConfirmation, 1_234, 100, f);
    assert_eq!(c.mint, mint(9));
    assert_eq!(c.lane, Lane::EarlyConfirmation);
    assert_eq!(c.discovery_score, 1_234);
    assert_eq!(c.discovered_at, 100);
    assert_eq!(c.features, f);
    assert_eq!(c.mint.bytes()[0], 9);
}

#[test]
fn evidence_strength_is_score_times_weight() {
    let c = Candidate::new(mint(1), Lane::CreationSniper, 1_000, 0, Features::default());
    // Independent computation: 1_000 * 8_000 = 8_000_000.
    assert_eq!(c.evidence_strength(8_000), 8_000_000u128);
    // With a heavier weight the strength scales linearly.
    assert_eq!(c.evidence_strength(12_000), 12_000_000u128);
    // Zero score => zero strength regardless of weight.
    let z = Candidate::new(mint(2), Lane::ActiveMarketScalp, 0, 0, Features::default());
    assert_eq!(z.evidence_strength(10_000), 0u128);
}

#[test]
fn evidence_strength_does_not_overflow_at_extremes() {
    // Max u64 score * max plausible weight fits in u128 with room to spare.
    let c = Candidate::new(
        mint(3),
        Lane::ActiveMarketScalp,
        u64::MAX,
        0,
        Features::default(),
    );
    let expected = u128::from(u64::MAX) * u128::from(60_000u32);
    assert_eq!(c.evidence_strength(60_000), expected);
}
