//! REGRESSION CLASS 2 — law-presence invariants over PUBLIC library APIs.
//!
//! These laws have a pure, deterministic library surface, so the regression
//! tripwire is a direct call — no engine drive needed. Each asserts the law still
//! fires in its MANDATED DIRECTION (import the crate's public API; where a law is
//! config-gated, toggle it). Fast, integer, no RNG (§22).
//!
//!   * §70.6/§70.8 narrative class + ceiling — class conditions conviction & reach.
//!   * §70.1 composite money proxy — a wallet/holder-led market outscores buy-pressure.
//!   * §70.7 platform-lead — a mainstream-led mint earns runway the law-off path denies.
//!   * signal-horizon matching — a class/lane mismatch and a too-slow feature are rejected.
//!   * §24(d) burst-climax exit reason EXISTS (IntoStrength, code 9) and is terminal.
//!   * §51 FDR/PBO promotion blocker is consulted and fails closed.
//!   * quote-mint SOL-default identity — WSOL is the pinned default quote asset.

use pump_quant_app::attention::{
    narrative_class_conviction_bp, AttentionField, AttentionParams, MentionProvenance,
};
use pump_quant_narrative::attention_state::Mention;
use pump_quant_narrative::narrative::{nv_narrative_ceiling, NarrativeClass, FP_ONE};

fn men(ts_ns: u64, source_id: u64, weight: u64) -> Mention {
    Mention {
        ts_ns,
        source_id,
        community_id: source_id,
        weight,
        copycat: false,
    }
}

// ---------------------------------------------------------------------------
// §70.6/§70.8 narrative class + ceiling (pure functions).
// ---------------------------------------------------------------------------

#[test]
fn narrative_class_conditions_conviction_and_ceiling() {
    // Conviction sizing is class-ordered and reduce-only (never > 100%).
    assert!(
        narrative_class_conviction_bp(NarrativeClass::News)
            < narrative_class_conviction_bp(NarrativeClass::Tech),
        "News must size below Tech"
    );
    assert!(
        narrative_class_conviction_bp(NarrativeClass::Trend)
            < narrative_class_conviction_bp(NarrativeClass::Culture),
        "Trend must size below Culture"
    );
    assert!(
        narrative_class_conviction_bp(NarrativeClass::Culture) <= 10_000,
        "class conviction is reduce-only (≤ 100%)"
    );
    // The same reach projects a strictly higher ceiling for a durable class.
    assert!(
        nv_narrative_ceiling(NarrativeClass::Trend, 1_000, FP_ONE)
            < nv_narrative_ceiling(NarrativeClass::Culture, 1_000, FP_ONE),
        "a durable class must project a higher reach ceiling than a fast one"
    );
}

// ---------------------------------------------------------------------------
// §70.1 composite money proxy — a wallet/holder-led market outscores flat
// buy-pressure. The two `money_of` closures ARE the two config arms.
// ---------------------------------------------------------------------------

#[test]
fn composite_money_proxy_outscores_buy_pressure_alone() {
    let build = || {
        let mut f = AttentionField::new(AttentionParams::standard());
        let m = [7u8; 32];
        for i in 0..8u64 {
            f.observe(m, men(1_000 + i * 10, i, 400));
        }
        f
    };

    const FLAT_BP: u64 = 5_000;
    let composite_later: u64 = FLAT_BP + 500 * 3 + 200 * 8; // rising money (§70.1 fold)

    // Arm A — composite money proxy (money rises across emits).
    let mut a = build();
    let mut buf = Vec::new();
    a.emit_into(&mut buf, 1, |_| FLAT_BP, |_| true);
    buf.clear();
    for i in 8..16u64 {
        a.observe([7u8; 32], men(2_000 + i * 10, i, 800));
    }
    a.emit_into(&mut buf, 2, |_| composite_later, |_| true);
    assert_eq!(buf.len(), 1, "one attention candidate");
    let composite_score = buf[0].discovery_score;

    // Arm B — buy-pressure alone (money flat across emits).
    let mut b = build();
    let mut buf2 = Vec::new();
    b.emit_into(&mut buf2, 1, |_| FLAT_BP, |_| true);
    buf2.clear();
    for i in 8..16u64 {
        b.observe([7u8; 32], men(2_000 + i * 10, i, 800));
    }
    b.emit_into(&mut buf2, 2, |_| FLAT_BP, |_| true);
    assert_eq!(buf2.len(), 1);
    let buy_pressure_score = buf2[0].discovery_score;

    assert!(
        composite_score > buy_pressure_score,
        "the §70.1 composite money proxy must score a wallet/holder-led market above \
         the buy-pressure-only proxy ({composite_score} vs {buy_pressure_score})"
    );
}

// ---------------------------------------------------------------------------
// §70.7 platform-lead — mainstream-led runway, gated by platform_lead_enable.
// ---------------------------------------------------------------------------

#[test]
fn platform_lead_gives_a_mainstream_led_mint_more_runway() {
    fn run(enable: bool, lead: bool) -> u64 {
        let params = AttentionParams {
            platform_lead_enable: enable,
            platform_lead_tolerance_ns: 1,
            ..AttentionParams::standard()
        };
        let mut f = AttentionField::new(params);
        let m = [3u8; 32];
        let crypto = MentionProvenance::default();
        let mainstream = MentionProvenance {
            mainstream: true,
            ..MentionProvenance::default()
        };
        for s in 0..6u64 {
            let ts = 1_000 + s * 10;
            let prov = if lead && s == 0 { &mainstream } else { &crypto };
            f.observe_tagged(m, men(ts, s, 400), prov);
        }
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 1_000, |_| true);
        for s in 0..6u64 {
            f.observe_tagged(m, men(2_000 + s * 10, s, 800), &crypto);
        }
        buf.clear();
        f.emit_into(&mut buf, 2, |_| 1_000, |_| true);
        buf.first().map(|c| c.discovery_score).unwrap_or(0)
    }

    // Law OFF: platform provenance is inert — lead and saturated are identical.
    assert_eq!(
        run(false, true),
        run(false, false),
        "without the law the mainstream lead earns no runway"
    );
    // Law ON: the mainstream-led mint out-scores the crypto-saturated one.
    assert!(
        run(true, true) > run(true, false),
        "the §70.7 platform-lead runway must lift a mainstream-led mint above a \
         crypto-saturated one"
    );
}

// ---------------------------------------------------------------------------
// Signal-horizon matching — a class/lane mismatch and a too-slow feature reject.
// ---------------------------------------------------------------------------

#[test]
fn signal_horizon_rejects_class_mismatch_and_too_slow() {
    use pump_quant_strategy::signal_horizon::{
        admit_feature_to_lane, FeatureClass, HorizonVerdict, Lane,
    };

    // Structurally-late TikTok virality is FORBIDDEN at a latency-critical entry
    // lane, no matter how fast it is reported (class check runs first).
    assert_eq!(
        admit_feature_to_lane(
            0,
            FeatureClass::TikTokVirality,
            Lane::CreationSniper,
            1_000,
            10
        ),
        HorizonVerdict::ClassForbidden,
        "TikTok virality must be class-forbidden at an entry lane"
    );
    // A class-admissible but SLOW feature (latency + margin > horizon) is TooSlow.
    assert_eq!(
        admit_feature_to_lane(
            2_000,
            FeatureClass::OnChainFlow,
            Lane::CreationSniper,
            1_000,
            10
        ),
        HorizonVerdict::TooSlow,
        "a feature that cannot beat the lane horizon must be rejected TooSlow"
    );
    // On-chain flow that beats the horizon with margin is admissible (positive arm).
    assert_eq!(
        admit_feature_to_lane(
            500,
            FeatureClass::OnChainFlow,
            Lane::CreationSniper,
            1_000,
            10
        ),
        HorizonVerdict::Admissible,
        "fast on-chain flow must be admissible to an entry lane"
    );
}

// ---------------------------------------------------------------------------
// §24(d) burst-climax exit reason EXISTS and is terminal.
// ---------------------------------------------------------------------------

#[test]
fn burst_climax_into_strength_exit_reason_exists() {
    use pump_quant_app::position::ExitReason;
    // The IntoStrength (§24(d) climax harvest) reason must still exist with its
    // stable journal code 9 and be a whole-position (terminal) exit.
    assert_eq!(
        ExitReason::IntoStrength.code(),
        9,
        "the burst-climax exit must keep its pinned journal code 9"
    );
    assert!(
        ExitReason::IntoStrength.is_terminal(),
        "an into-strength climax exit closes the whole remaining position"
    );
    // The distinct exit taxonomy is intact (each reason has a unique code).
    let reasons = [
        ExitReason::RugPrecursor,
        ExitReason::HardStop,
        ExitReason::ThesisInvalidation,
        ExitReason::TakeProfitLadder,
        ExitReason::TrailingStop,
        ExitReason::TimeStop,
        ExitReason::ForceClose,
        ExitReason::CreatorDump,
        ExitReason::IntoStrength,
    ];
    let mut codes: Vec<u8> = reasons.iter().map(|r| r.code()).collect();
    codes.sort_unstable();
    let n = codes.len();
    codes.dedup();
    assert_eq!(
        codes.len(),
        n,
        "every ExitReason must have a distinct journal code"
    );
}

// ---------------------------------------------------------------------------
// §51 FDR/PBO promotion blocker is consulted and fails closed.
// ---------------------------------------------------------------------------

#[test]
fn promotion_blocker_consults_fdr_and_pbo_and_fails_closed() {
    use pump_quant_evaluator::fdr::Hypothesis;
    use pump_quant_evaluator::promotion_verdict::{promotion_verdict, PromotionBlockReason};

    // Skilled, dominant trial-0 performance ⇒ PBO 0 (no overfit).
    let skilled = vec![
        vec![100i64, 100, 100, 100],
        vec![10, 10, 10, 10],
        vec![20, 20, 20, 20],
        vec![30, 30, 30, 30],
    ];
    // A discovered candidate with low PBO clears BOTH gates.
    let fam = vec![Hypothesis::new(1, 5_000), Hypothesis::new(2, 500_000)];
    let clear = promotion_verdict(&fam, 50_000, 1, &skilled, 5_000);
    assert!(
        !clear.blocks(),
        "a discovered, non-overfit candidate must clear"
    );
    assert_eq!(clear.reason, PromotionBlockReason::Clear);

    // An UNDISCOVERED candidate (id absent from the BH discoveries) is FDR-blocked.
    let undiscovered = promotion_verdict(&fam, 50_000, 2, &skilled, 5_000);
    assert!(
        undiscovered.fdr_blocks,
        "a candidate not among the BH discoveries must be FDR-blocked"
    );
    assert!(undiscovered.blocks());

    // A mirror-image (noise) perf matrix ⇒ PBO 10_000 ⇒ PBO-blocked.
    let noise = vec![vec![100i64, -100], vec![-100, 100]];
    let overfit = promotion_verdict(&fam, 50_000, 1, &noise, 5_000);
    assert!(
        overfit.pbo_blocks,
        "a flip-flop (overfit) matrix must be PBO-blocked"
    );

    // Fail-closed: an inadmissible (single-row) matrix cannot be measured and must
    // BLOCK, never silently pass, with no pbo_bps reported.
    let inadmissible = promotion_verdict(&fam, 50_000, 1, &[vec![1i64, 2, 3]], 5_000);
    assert!(
        inadmissible.pbo_blocks && inadmissible.pbo_bps.is_none(),
        "an unmeasurable PBO matrix must fail closed (block, pbo_bps None)"
    );
}

// ---------------------------------------------------------------------------
// Quote-mint SOL-default identity — WSOL is the pinned default quote asset and
// the pool decoder faithfully returns the on-chain quote identity (keys off the
// field, never assumes).
// ---------------------------------------------------------------------------

#[test]
fn quote_mint_sol_default_identity() {
    use pump_quant_protocol::pumpswap::{decode_pool_account, POOL_FIXED_LEN, WSOL_MINT};
    use pump_quant_protocol::registry::{self, Venue, PUMPSWAP_ACCOUNT_DISCRIMINATOR};

    // The canonical wrapped-SOL mint is pinned and is the default quote for both
    // supported venues.
    assert_eq!(WSOL_MINT, "So11111111111111111111111111111111111111112");
    assert_eq!(registry::entry(Venue::PumpFun).quote_mint, WSOL_MINT);
    assert_eq!(registry::entry(Venue::PumpSwap).quote_mint, WSOL_MINT);

    // The full pool decoder returns the EXACT quote-mint bytes present on-chain
    // (offset 75), so a WSOL-quoted pool round-trips to the WSOL identity and a
    // USDC-quoted pool round-trips to a DIFFERENT identity — it never assumes SOL.
    let mut acct = vec![0u8; POOL_FIXED_LEN];
    acct[0..8].copy_from_slice(&PUMPSWAP_ACCOUNT_DISCRIMINATOR);
    let wsol_id = [0xABu8; 32]; // stand-in on-chain WSOL pubkey bytes
    let usdc_id = [0xCDu8; 32];
    acct[75..107].copy_from_slice(&wsol_id);
    let p = decode_pool_account(&acct).expect("well-formed pool must decode");
    assert_eq!(
        p.quote_mint, wsol_id,
        "decoder must return the on-chain quote identity"
    );

    acct[75..107].copy_from_slice(&usdc_id);
    let p2 = decode_pool_account(&acct).expect("well-formed pool must decode");
    assert_eq!(
        p2.quote_mint, usdc_id,
        "a USDC-quoted pool keys off the field, not SOL"
    );
    assert_ne!(
        p.quote_mint, p2.quote_mint,
        "distinct quote assets must decode distinctly"
    );
}
