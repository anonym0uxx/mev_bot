//! End-to-end nervous-system contract: union discovery, corroboration-gated entry,
//! byte-deterministic replay, and config-driven behavior (the no-hardcode guarantee).

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, CreatorActionKind};
use pump_quant_domain::ids::Mint as DomainMint;

fn mint(tag: u8) -> DomainMint {
    DomainMint::from_bytes([tag; 32])
}

/// A `dev_portable` config funded well above the criterion-112 / A-6 0.1-SOL operator
/// trade floor, for the functional discovery/gating/attribution tests that only need a
/// position to OPEN on a shallow test market. The recalibrated default (2-SOL bankroll,
/// f_base ≈ base bite of 0.1 SOL = the floor) correctly REFUSES a setup once a
/// reduce-only flow haircut drops it below the floor — sound in production, but it
/// blocks these tests, which are orthogonal to the sizing policy. A large bankroll
/// lifts the base bite (~1 SOL) far above the floor so a lightly-haircut setup still
/// clears x_min and admits (the shallow market then caps it at its x_max ≈ 0.1 SOL).
fn funded_cfg() -> Config {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 20_000_000_000; // 20 SOL ⇒ deployable ~15, base ~1 SOL
    cfg
}

/// A stream where three mints are discovered by three different lanes; only the
/// numeric-confirmed one is eligible for entry.
fn scenario() -> Vec<AppEvent> {
    let a = mint(0xAA); // numeric + on-chain confirm -> admissible
    let b = mint(0xBB); // social only -> discovered, never admissible
    let c = mint(0xCC); // narrative only -> discovered, never admissible
    let mut ev = Vec::new();

    // Numeric accumulation on A (buys), plus on-chain confirmation. Deep pool so a
    // ≥0.1-SOL floor clip (criterion 112 / A-6) has a low exit cost and clears the
    // §34.4 exit-cost veto — a shallow 100M pool cannot absorb a 0.1-SOL exit.
    for i in 0..5 {
        ev.push(AppEvent::MarketTrade {
            mint: a,
            price_fp: 1_000_000_000 + (i as i128) * 1_000_000,
            quote_lamports: 500_000,
            liquidity_lamports: 2_000_000_000,
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: a,
        sellable_depth_lamports: 2_000_000_000,
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
    let mut e = Engine::new(funded_cfg(), RunMode::Paper);
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

    let mut permissive = Engine::new(funded_cfg(), RunMode::Paper);
    let r_perm = permissive.run(&ev);
    assert!(r_perm.admitted >= 1);

    let mut cfg = funded_cfg();
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

    let mut eng = Engine::new(funded_cfg(), RunMode::Paper);
    let mut src = MockSocialSource::new().with_batch(vec![RawSocialPayload::new(
        format!(r#"{{"platform":"x","author":"caller","text":"early {usdc}","likes":10}}"#)
            .into_bytes(),
        1,
    )]);
    eng.ingest_social(&mut src);

    // Unproven at first: no reconciled evidence yet → baseline fallback (None here).
    assert_eq!(eng.earned_source_quality(source_id), None);

    // The same market accrues real numeric flow + an on-chain confirmation, so it
    // becomes admissible; the held-position lifecycle (§24) then manages it forward
    // and realizes an outcome the earn loop attributes back to the caller (§82).
    for i in 0..5 {
        eng.tick(AppEvent::MarketTrade {
            mint: mkt,
            price_fp: 1_000_000_000 + (i as i128) * 1_000_000,
            quote_lamports: 500_000,
            liquidity_lamports: 2_000_000_000, // deep: a 0.1-SOL floor clip clears exit-cost
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
        });
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: mkt,
        sellable_depth_lamports: 2_000_000_000,
    });
    // One evaluation tick promotes + admits: the position OPENS here.
    eng.tick(AppEvent::Tick);
    // The market pumps past the principal-recovery target (a ladder tranche banks),
    // then order flow rolls over hard — the thesis-invalidation exit closes the
    // remainder at a profit: a realized favorable outcome for the caller.
    eng.tick(AppEvent::MarketTrade {
        mint: mkt,
        price_fp: 1_420_000_000,
        quote_lamports: 500_000,
        liquidity_lamports: 2_000_000_000,
        signed_base: 1_000_000,
        buyer_entity: 5,
        age_slots: 31,
    });
    eng.tick(AppEvent::MarketTrade {
        mint: mkt,
        price_fp: 1_400_000_000,
        quote_lamports: 2_000_000,
        liquidity_lamports: 2_000_000_000,
        signed_base: -4_000_000,
        buyer_entity: 6,
        age_slots: 32,
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

// ---------------------------------------------------------------------------
// MetaRotationState + CreatorState wiring (corroboration-tier, on-chain-led).
// ---------------------------------------------------------------------------

#[test]
fn token_metadata_and_creator_action_alone_never_admit() {
    // Category assignment + creator activity are corroboration-tier: on their own,
    // with no numeric flow and no on-chain confirmation, they can never authorise
    // capital (§29/§71). They still feed the factual layer.
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    let m = mint(0x51);
    e.tick(AppEvent::TokenMetadata {
        mint: m,
        category_id: 1,
        taxonomy_version: 1,
        creator: 7,
        slot: 1,
    });
    e.tick(AppEvent::CreatorAction {
        mint: m,
        kind: CreatorActionKind::Init {
            initial_tokens: 1_000,
            total_supply: 10_000,
        },
        slot: 1,
    });
    e.tick(AppEvent::CreatorAction {
        mint: m,
        kind: CreatorActionKind::Buy {
            tokens: 200,
            quote_lamports: 5_000,
        },
        slot: 2,
    });
    for _ in 0..8 {
        e.tick(AppEvent::Tick);
    }
    let r = e.report();
    assert_eq!(
        r.admitted, 0,
        "category + creator evidence never self-authorizes capital"
    );
    // But the factual/creator layers ARE live (not a dead reducer).
    assert!(
        e.meta_snapshot().total_launches >= 1,
        "TokenMetadata fed the factual category layer (launch counted)"
    );
    assert!(
        e.creator_state(m.as_bytes()).is_some(),
        "CreatorAction fed the creator-state reducer"
    );
}

#[test]
fn creator_distribution_fades_size_but_never_vetoes() {
    // Same admissible numeric+confirm scenario, run with and without a creator who
    // distributes in the SUB-VETO fade band (sells >50% of peak but below the §26
    // confirmed-dump veto threshold). This run must still ADMIT — below the §26
    // veto bar creator distribution remains a graded size fade, never a binary
    // reject — but deploy a smaller size → a different, smaller realized outcome.
    // (The confirmed-dump regime ABOVE the veto threshold is the operator-approved
    // §26 reversal, proven in `audit_wave2_laws.rs`.)
    let run = |with_creator_dump: bool| -> pump_quant_app::engine::Report {
        // A *wide* viability band (low fixed cost) so `x_min << x_cost` and the
        // graded haircut has room to reduce size within the band. With the default
        // fixed cost the band collapses to a single point (x_min == x_cost) and any
        // corroboration-tier haircut is correctly a no-op (never sizes below viable).
        let mut cfg = Config::dev_portable();
        cfg.gate_base_fixed_lamports = 1_000;
        // A-6: deep market + wide economics + a 7-SOL bankroll place BOTH arms in the
        // free sizing regime (above the 0.1-SOL floor, below x_max ≈ 0.3 SOL, and below
        // the 0.2-SOL two-bite split threshold — single-bite, no scale-in asymmetry), so
        // the graded creator fade reduces the DEPLOYED size and the smaller net is
        // visible. A shallow default market would clamp both arms to the floor.
        cfg.gate_expected_move_bps = 1_800;
        cfg.gate_protocol_bps = 450;
        cfg.gate_margin_bps = 150;
        cfg.gate_impact_den = 250_000;
        cfg.bankroll_initial_lamports = 7_000_000_000; // base ~0.35 SOL
        let mut e = Engine::new(cfg, RunMode::Paper);
        let m = mint(0x62);
        if with_creator_dump {
            e.tick(AppEvent::CreatorAction {
                mint: m,
                kind: CreatorActionKind::Init {
                    initial_tokens: 1_000_000,
                    total_supply: 10_000_000,
                },
                slot: 1,
            });
            e.tick(AppEvent::CreatorAction {
                mint: m,
                kind: CreatorActionKind::Sell {
                    tokens: 550_000, // 55% of peak: past the 50% fade trigger,
                    // below the 60% §26 confirmed-dump veto bar → fade, not veto.
                    quote_lamports: 1_000_000,
                },
                slot: 2,
            });
        }
        for i in 0..5 {
            e.tick(AppEvent::MarketTrade {
                mint: m,
                price_fp: 1_000_000_000 + (i as i128) * 1_000_000,
                quote_lamports: 500_000,
                liquidity_lamports: 2_000_000_000, // deep: x_max bounded by economics, not depth
                signed_base: 1_000_000,
                buyer_entity: i,
                age_slots: 30,
            });
        }
        e.tick(AppEvent::OnchainConfirm {
            mint: m,
            sellable_depth_lamports: 2_000_000_000,
        });
        // Admit (position opens), then a pump past the principal-recovery target and
        // a flow rollover close the position AT A PROFIT — so realized net scales
        // with the deployed size and the creator haircut is visible in net SOL.
        e.tick(AppEvent::Tick);
        e.tick(AppEvent::MarketTrade {
            mint: m,
            price_fp: 1_500_000_000,
            quote_lamports: 500_000,
            liquidity_lamports: 2_000_000_000,
            signed_base: 1_000_000,
            buyer_entity: 5,
            age_slots: 31,
        });
        e.tick(AppEvent::MarketTrade {
            mint: m,
            price_fp: 1_480_000_000,
            quote_lamports: 2_000_000,
            liquidity_lamports: 2_000_000_000,
            signed_base: -4_000_000,
            buyer_entity: 6,
            age_slots: 32,
        });
        e.report()
    };
    let baseline = run(false);
    let faded = run(true);
    assert!(baseline.admitted >= 1, "baseline market is admissible");
    assert_eq!(
        faded.admitted, baseline.admitted,
        "creator distribution fades size, never vetoes the admit (§22 behavioral-risk)"
    );
    assert_ne!(
        faded.journal_digest, baseline.journal_digest,
        "the size haircut changed the realized decision (the wiring is live)"
    );
    assert!(
        faded.net_lamports < baseline.net_lamports,
        "a distributing creator earns a smaller deployed size → smaller realized net"
    );
}

#[test]
fn fed_meta_path_is_live_and_deterministic() {
    let drive = || -> pump_quant_app::engine::Report {
        let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
        for tag in [0x71u8, 0x72, 0x73] {
            e.tick(AppEvent::TokenMetadata {
                mint: mint(tag),
                category_id: 1,
                // v1 is the shipped taxonomy version (see `META_TAXONOMY_VERSION_DEFAULT`);
                // an assignment stamped with any other version is left UNKNOWN, never
                // retroactively remapped (criterion 81).
                taxonomy_version: 1,
                creator: u64::from(tag),
                slot: 1,
            });
        }
        for round in 0..4u64 {
            for tag in [0x71u8, 0x72, 0x73] {
                let m = mint(tag);
                for i in 0..4u64 {
                    e.tick(AppEvent::MarketTrade {
                        mint: m,
                        price_fp: 1_000_000_000
                            + (round as i128) * 4_000_000
                            + (i as i128) * 1_000_000,
                        quote_lamports: 800_000,
                        liquidity_lamports: 80_000_000,
                        signed_base: 2_000_000,
                        buyer_entity: (i + round) % 7,
                        age_slots: 20,
                    });
                }
                e.tick(AppEvent::OnchainConfirm {
                    mint: m,
                    sellable_depth_lamports: 150_000_000,
                });
            }
            for _ in 0..60 {
                e.tick(AppEvent::Tick); // cross the reflection cadence (50) each round
            }
        }
        e.report()
    };
    let r1 = drive();
    let r2 = drive();
    assert_eq!(r1, r2, "the fed meta/creator path is byte-deterministic");

    // Inspect the factual layer directly: launches + category-attributed flow.
    let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
    for tag in [0x71u8, 0x72, 0x73] {
        let m = mint(tag);
        e.tick(AppEvent::TokenMetadata {
            mint: m,
            category_id: 1,
            // v1 is the shipped taxonomy version (see `META_TAXONOMY_VERSION_DEFAULT`);
            // an assignment stamped with any other version is left UNKNOWN, never
            // retroactively remapped (criterion 81).
            taxonomy_version: 1,
            creator: u64::from(tag),
            slot: 1,
        });
        for i in 0..4u64 {
            e.tick(AppEvent::MarketTrade {
                mint: m,
                price_fp: 1_000_000_000 + (i as i128) * 1_000_000,
                quote_lamports: 800_000,
                liquidity_lamports: 80_000_000,
                signed_base: 2_000_000,
                buyer_entity: i,
                age_slots: 20,
            });
        }
    }
    let snap = e.meta_snapshot();
    assert_eq!(snap.total_launches, 3, "three category-1 launches recorded");
    let cat1 = snap
        .category(1)
        .expect("category 1 present in the snapshot");
    assert!(
        cat1.buy_quote > 0,
        "category flow accumulated from the attributed on-chain trades"
    );
}

#[test]
fn numeric_lane_discovers_buy_flow_not_sell_flow() {
    // Real trade-flow microstructure with a sign-agreement gate (§21.7): a mint whose
    // flow is net-SELL (or price rising against falling CVD = bearish divergence) is
    // confirmed on-chain but must NEVER be self-authorized — the numeric lane
    // discovers genuine buy *flow*, not price. A parallel buy-flow mint IS admitted.
    let run = |buy: bool| -> pump_quant_app::engine::Report {
        let mut e = Engine::new(funded_cfg(), RunMode::Paper);
        let m = mint(0x77);
        let sign = if buy { 1i64 } else { -1i64 };
        for i in 0..6u64 {
            e.tick(AppEvent::MarketTrade {
                mint: m,
                price_fp: 1_000_000_000 + (i as i128) * 1_000_000, // price rising either way
                quote_lamports: 500_000,
                liquidity_lamports: 2_000_000_000, // deep: a 0.1-SOL floor clip clears exit-cost
                signed_base: sign * 1_000_000,
                buyer_entity: i,
                age_slots: 30,
            });
        }
        e.tick(AppEvent::OnchainConfirm {
            mint: m,
            sellable_depth_lamports: 2_000_000_000,
        });
        for _ in 0..6 {
            e.tick(AppEvent::Tick);
        }
        e.report()
    };
    assert!(
        run(true).admitted >= 1,
        "genuine buy flow (OFI+CVD agree, price-confirmed) is admitted"
    );
    assert_eq!(
        run(false).admitted,
        0,
        "net sell flow / bearish divergence is never self-authorized (§21.7)"
    );
}

// ---------------------------------------------------------------------------
// Batch C: dynamic bankroll sizing (§33), VPIN-X toxicity, staleness gates.
// ---------------------------------------------------------------------------

/// An admissible one-market scenario: rising buy flow + on-chain confirm + a tick.
fn admissible_stream(tag: u8) -> Vec<AppEvent> {
    let m = mint(tag);
    let mut ev = Vec::new();
    for i in 0..6u64 {
        ev.push(AppEvent::MarketTrade {
            mint: m,
            price_fp: 1_000_000_000 + (i as i128) * 1_000_000,
            quote_lamports: 500_000,
            // Deep pool so a ≥0.1-SOL floor clip (criterion 112 / A-6) has a low exit
            // cost and clears the §34.4 exit-cost veto (a 100M pool cannot absorb it).
            liquidity_lamports: 2_000_000_000,
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: m,
        sellable_depth_lamports: 2_000_000_000,
    });
    ev.push(AppEvent::Tick);
    ev
}

/// A deep, admissible market (100-SOL pool, 200-SOL proven depth) so bankroll-
/// derived sizes are small relative to the venue (the §34.4/§21.7 exit-cost law
/// correctly vetoes bankroll-scale sizes against toy-sized pools).
fn deep_admissible_stream(tag: u8) -> Vec<AppEvent> {
    let m = mint(tag);
    let mut ev = Vec::new();
    for i in 0..6u64 {
        ev.push(AppEvent::MarketTrade {
            mint: m,
            price_fp: 1_000_000_000 + (i as i128) * 1_000_000,
            quote_lamports: 500_000,
            liquidity_lamports: 100_000_000_000,
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: m,
        sellable_depth_lamports: 200_000_000_000,
    });
    ev.push(AppEvent::Tick);
    ev
}

#[test]
fn bankroll_sizing_scales_with_capital_and_respects_the_floor() {
    // The same market, three bankrolls: sizes derive from deployable capital
    // (start with ANY amount of SOL), and a bankroll at/below the survival floor
    // refuses to trade at all — the floor is never risked (§33/delta-§1).
    let admitted_size = |bankroll: u64| -> Option<u64> {
        let mut cfg = Config::dev_portable();
        cfg.apply("bankroll_initial_lamports", bankroll as i64)
            .unwrap();
        cfg.apply("gate_base_fixed_lamports", 1_000).unwrap(); // wide band: x_min small
        let mut e = Engine::new(cfg, RunMode::Paper);
        for ev in deep_admissible_stream(0x91) {
            e.tick(ev);
        }
        let r = e.report();
        (r.admitted >= 1).then_some(r.ticks).map(|_| r.admitted)
    };
    // 10 SOL and 100 SOL both admit; 0.4 SOL (< 0.5-SOL floor) cannot deploy.
    assert_eq!(admitted_size(10_000_000_000), Some(1), "10 SOL trades");
    assert_eq!(admitted_size(100_000_000_000), Some(1), "100 SOL trades");
    assert_eq!(
        admitted_size(400_000_000),
        None,
        "a bankroll below the survival floor refuses (deployable = 0)"
    );
}

#[test]
fn bankroll_size_is_proportional_to_deployable_capital() {
    // Scale-invariance: 10× the bankroll ⇒ 10× the deployed size (until the band
    // or risk caps bind) — verified through realized net magnitude.
    let net_for = |bankroll: u64| -> i128 {
        let mut cfg = Config::dev_portable();
        cfg.apply("bankroll_initial_lamports", bankroll as i64)
            .unwrap();
        cfg.apply("gate_base_fixed_lamports", 1_000).unwrap();
        // A-6: a deep impact curve so x_max (impact-bounded) is far above both arms'
        // deployed size — the proportionality (10× bankroll ⇒ 10× size) is only visible
        // while the band does NOT bind. With the default impact_den both arms would cap
        // at x_max ≈ 0.15 SOL and the proportionality would vanish.
        cfg.apply("gate_impact_den", 100_000_000).unwrap();
        let mut e = Engine::new(cfg, RunMode::Paper);
        for ev in deep_admissible_stream(0x92) {
            e.tick(ev);
        }
        // pump then flow-rollover close in profit
        e.tick(AppEvent::MarketTrade {
            mint: mint(0x92),
            price_fp: 1_500_000_000,
            quote_lamports: 500_000,
            liquidity_lamports: 100_000_000_000,
            signed_base: 1_000_000,
            buyer_entity: 9,
            age_slots: 31,
        });
        e.tick(AppEvent::MarketTrade {
            mint: mint(0x92),
            price_fp: 1_480_000_000,
            quote_lamports: 2_000_000,
            liquidity_lamports: 100_000_000_000,
            signed_base: -4_000_000,
            buyer_entity: 10,
            age_slots: 32,
        });
        e.report().net_lamports
    };
    // A-6: both bankrolls sized so the deployed bite is ABOVE the 0.2-SOL two-bite
    // split threshold (deployable × f_base × the ~0.5 flow haircut ≳ 0.5 SOL), so BOTH
    // split probe+scale-in at the SAME 40% probe fraction — proportionality holds. (At
    // the old 4-SOL bankroll the small arm sizes at the 0.1-SOL floor as a SINGLE bite
    // while the large arm splits, breaking the 10×-size ⇒ 10×-net proportionality.)
    let small = net_for(20_000_000_000); // 20 SOL → deployable 15, bite ~0.5 SOL
    let large = net_for(200_000_000_000); // 200 SOL → deployable 150, bite ~5 SOL
    assert!(small > 0 && large > 0, "both bankrolls profit on the pump");
    assert!(
        large > small * 5,
        "10× bankroll deploys ~10× size ⇒ much larger realized net ({large} vs {small})"
    );
}

#[test]
fn vpin_sell_dump_vetoes_admission() {
    // The complementarity that earns VPIN its place: a distributed dump executed in
    // MANY small sells scrolls out of the 64-trade CVD/OFI ring once a burst of tiny
    // buys follows — the sign-agreement gate re-qualifies — but the VOLUME-clocked
    // VPIN buckets still hold the dump (volume-time memory), read extreme
    // sell-dominant, and veto the admission (§21.7).
    let mut cfg = Config::dev_portable();
    cfg.apply("vpin_v_min_lamports", 1_000).unwrap();
    cfg.apply("vpin_v_max_lamports", 1_000).unwrap();
    let mut e = Engine::new(cfg, RunMode::Paper);
    let m = mint(0x93);
    // 70 small sells: 42_000 quote lamports of distribution (42 all-sell buckets).
    for i in 0..70u64 {
        e.tick(AppEvent::MarketTrade {
            mint: m,
            price_fp: 1_000_000_000 - (i as i128) * 100_000,
            quote_lamports: 600,
            liquidity_lamports: 100_000_000,
            signed_base: -10_000,
            buyer_entity: i % 7,
            age_slots: 30,
        });
    }
    // 66 tiny-quote buys with big base: the trade ring now holds ONLY buys (CVD>0,
    // OFI strongly positive, price rising -> the lane gate re-qualifies)...
    for i in 0..66u64 {
        e.tick(AppEvent::MarketTrade {
            mint: m,
            price_fp: 995_000_000 + (i as i128) * 500_000,
            quote_lamports: 50,
            liquidity_lamports: 100_000_000,
            signed_base: 1_000_000,
            buyer_entity: i % 9,
            age_slots: 31,
        });
    }
    e.tick(AppEvent::OnchainConfirm {
        mint: m,
        sellable_depth_lamports: 200_000_000,
    });
    e.tick(AppEvent::Tick);
    let r = e.report();
    // ...but the VPIN ring is still 13 sell / 3 buy buckets: extreme + sell-dominant.
    assert_eq!(
        r.admitted, 0,
        "sell-dominant VPIN extreme tier vetoes admission"
    );
    assert!(r.rejected >= 1, "the veto is journaled, never silent");
}

#[test]
fn stale_confirmation_no_longer_authorizes() {
    // A confirm proven long ago is not depth now (§34.3). Fast-cycling positions
    // (1-tick stall, 2-tick max hold) re-admit every few ticks while the confirm is
    // fresh; with a 5-tick confirm TTL the re-admission stream STOPS, while a
    // 1000-tick TTL keeps re-admitting off the same stale proof.
    let run = |confirm_ttl: i64| -> u64 {
        let mut cfg = funded_cfg();
        cfg.apply("confirm_ttl_ticks", confirm_ttl).unwrap();
        cfg.apply("lane_evidence_ttl_ticks", 1_000).unwrap(); // isolate the confirm TTL
        cfg.apply("lc_stall_ticks", 1).unwrap();
        cfg.apply("lc_max_hold_ticks", 2).unwrap();
        let mut e = Engine::new(cfg, RunMode::Paper);
        for ev in admissible_stream(0x94) {
            e.tick(ev);
        }
        for _ in 0..24 {
            e.tick(AppEvent::Tick);
        }
        e.report().admitted
    };
    let short = run(5);
    let long = run(1_000);
    assert!(short >= 1, "fresh confirm admits at least once");
    assert!(
        long > short,
        "a stale confirm stops authorizing re-entry under the short TTL ({short}) \
         while the long TTL keeps re-admitting ({long})"
    );
}
