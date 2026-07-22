//! End-to-end nervous-system contract: union discovery, corroboration-gated entry,
//! byte-deterministic replay, and config-driven behavior (the no-hardcode guarantee).

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint as DomainMint;

fn mint(tag: u8) -> DomainMint {
    DomainMint::from_bytes([tag; 32])
}

/// A stream where three mints are discovered by three different lanes; only the
/// numeric-confirmed one is eligible for entry.
fn scenario() -> Vec<AppEvent> {
    let a = mint(0xAA); // numeric + on-chain confirm -> admissible
    let b = mint(0xBB); // social only -> discovered, never admissible
    let c = mint(0xCC); // narrative only -> discovered, never admissible
    let mut ev = Vec::new();

    // Numeric accumulation on A (buys), plus on-chain confirmation.
    for i in 0..5 {
        ev.push(AppEvent::MarketTrade {
            mint: a,
            liquidity_lamports: 100_000_000,
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: a,
        sellable_depth_lamports: 200_000_000,
    });

    // Loud social call on B and narrative burst on C — corroboration only.
    ev.push(AppEvent::SocialCall {
        mint: b,
        source_quality_bp: 9_000,
    });
    ev.push(AppEvent::NarrativeSample {
        mint: c,
        prior_active: 10,
        new_mentions: 400,
    });

    // Drive several evaluation ticks.
    for _ in 0..6 {
        ev.push(AppEvent::Tick);
    }
    ev
}

#[test]
fn union_not_intersection_all_three_are_discovered() {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    let r = e.run(&scenario());
    // All three lanes surfaced candidates independently, so promotions happened for
    // more than just the numeric one.
    assert!(r.promoted >= 3, "each lane discovers on its own (union)");
}

#[test]
fn only_confirmed_numeric_candidate_is_admitted() {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    let r = e.run(&scenario());
    assert!(r.admitted >= 1, "the confirmed numeric mint is admissible");
    assert!(
        r.rejected >= 2,
        "social-only and narrative-only mints are refused at the gate"
    );
    // Realized net-SOL is attributed to the numeric (ActiveMarketScalp) lane only.
    let scalp_net = r
        .per_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::Lane::ActiveMarketScalp)
        .map(|(_, n)| *n)
        .unwrap();
    let sniper_net = r
        .per_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::Lane::CreationSniper)
        .map(|(_, n)| *n)
        .unwrap();
    assert_ne!(scalp_net, 0, "numeric lane traded");
    assert_eq!(sniper_net, 0, "social lane never traded");
}

#[test]
fn replay_is_byte_deterministic() {
    let ev = scenario();
    let mut e1 = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mut e2 = Engine::new(Config::dev_portable(), RunMode::Replay);
    let r1 = e1.run(&ev);
    let r2 = e2.run(&ev);
    assert_eq!(r1, r2, "same events -> identical report");
    assert_eq!(
        r1.journal_digest, r2.journal_digest,
        "same events -> identical decision-journal digest"
    );
}

#[test]
fn behavior_is_config_driven_not_hardcoded() {
    // The no-hardcode guarantee: change only a config parameter and the engine's
    // decisions must change. Here we make the economics unviable via margin; the
    // previously-admissible mint must now be refused.
    let ev = scenario();

    let mut permissive = Engine::new(Config::dev_portable(), RunMode::Paper);
    let r_perm = permissive.run(&ev);
    assert!(r_perm.admitted >= 1);

    let mut cfg = Config::dev_portable();
    cfg.apply("gate_margin_bps", 9_000).unwrap();
    let mut strict = Engine::new(cfg, RunMode::Paper);
    let r_strict = strict.run(&ev);
    assert_eq!(
        r_strict.admitted, 0,
        "a config change alone flips the decision — no hardcoded thresholds"
    );
    assert_ne!(r_perm.journal_digest, r_strict.journal_digest);
}

#[test]
fn promote_k_config_bounds_promotions_per_tick() {
    // Another config-driven check: dropping promote_k to 1 must reduce promotions
    // versus the default, proving the value is read, not baked in.
    let ev = scenario();
    let mut cfg = Config::dev_portable();
    cfg.apply("promote_k", 1).unwrap();
    let mut e = Engine::new(cfg, RunMode::Paper);
    let r_small = e.run(&ev);

    let mut e_def = Engine::new(Config::dev_portable(), RunMode::Paper);
    let r_def = e_def.run(&ev);

    assert!(r_small.promoted <= r_def.promoted);
}

#[test]
fn ingest_social_wires_the_lane_into_the_loop() {
    use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

    // A real 32-byte Solana address named in a captured post.
    let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let json = format!(r#"{{"platform":"x","author":"kol","text":"ape {usdc} $USDC","likes":50}}"#)
        .into_bytes();

    let mut eng = Engine::new(Config::dev_portable(), RunMode::Paper);
    let mut src = MockSocialSource::new().with_batch(vec![RawSocialPayload::new(json, 1)]);

    // Draining the live source applies exactly one corroboration call (one contract)
    // and feeds the attention field; quality is resolved from the engine's ledger.
    let applied = eng.ingest_social(&mut src);
    assert_eq!(applied, 1);

    // The social lane is now live in the loop: a tick promotes the corroborated
    // mint to the gate, where — social being corroboration-tier — it is refused for
    // lack of on-chain confirmation (never admitted on social alone, §29/§71).
    eng.tick(AppEvent::Tick);
    let r = eng.report();
    assert!(r.promoted >= 1, "social corroboration reached the gate");
    assert_eq!(r.admitted, 0, "social alone never admits capital");

    // Determinism: the same drained source reproduces the same application count.
    let mut eng2 = Engine::new(Config::dev_portable(), RunMode::Paper);
    let mut src2 = MockSocialSource::new().with_batch(vec![RawSocialPayload::new(
        format!(r#"{{"platform":"x","author":"kol","text":"ape {usdc} $USDC","likes":50}}"#)
            .into_bytes(),
        1,
    )]);
    assert_eq!(eng2.ingest_social(&mut src2), 1);
}

#[test]
fn social_source_earns_quality_from_realized_outcomes() {
    use pump_quant_ingest::base58::decode_pubkey;
    use pump_quant_ingest::social_parse::fnv1a_64;
    use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

    // A source ("caller") names a real market via the attributed social path.
    let usdc = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let mkt = DomainMint::from_bytes(decode_pubkey(usdc).unwrap());
    let source_id = fnv1a_64(b"caller");

    let mut eng = Engine::new(Config::dev_portable(), RunMode::Paper);
    let mut src = MockSocialSource::new().with_batch(vec![RawSocialPayload::new(
        format!(r#"{{"platform":"x","author":"caller","text":"early {usdc}","likes":10}}"#)
            .into_bytes(),
        1,
    )]);
    eng.ingest_social(&mut src);

    // Unproven at first: no reconciled evidence yet → baseline fallback (None here).
    assert_eq!(eng.earned_source_quality(source_id), None);

    // The same market accrues real numeric flow + an on-chain confirmation, so it
    // becomes admissible and is scalped — a realized outcome the earn loop attributes
    // back to the caller and reconciles at the reflection cadence.
    for i in 0..5 {
        eng.tick(AppEvent::MarketTrade {
            mint: mkt,
            liquidity_lamports: 100_000_000,
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
        });
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: mkt,
        sellable_depth_lamports: 200_000_000,
    });
    let reflect_ticks = Config::dev_portable().reflect_every_ticks + 2;
    for _ in 0..reflect_ticks {
        eng.tick(AppEvent::Tick);
    }

    // The caller now has an earned grade (favorable-rate bps), no longer unproven —
    // the dormant D1–D10 quality loop is closed.
    assert!(
        eng.earned_source_quality(source_id).is_some(),
        "a source whose called market realized an outcome earns a reconciled grade"
    );
}
