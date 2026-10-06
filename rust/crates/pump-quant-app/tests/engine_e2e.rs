//! End-to-end nervous-system contract: union discovery, corroboration-gated entry,
//! byte-deterministic replay, and config-driven behavior (the no-hardcode guarantee).

#![allow(dead_code)] // test scaffolding: helper/fixture chains not every #[test] exercises (consolidation N2)

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, CreatorActionKind};
use pump_quant_domain::ids::Mint as DomainMint;

/// **DEPTH REALISM (re-pin #26).** The gate's price-impact model is now DERIVED from
/// the market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves START at 30 SOL; the 2 SOL these fixtures used to declare put the
/// operator's 0.1 SOL floor clip at 5% of the pool (Amendment A-13(1)).
/// **A REAL BONDING CURVE THAT HAS BEEN BOUGHT INTO (corrected 2026-07-28).**
///
/// pump.fun seeds a curve with **30 SOL of VIRTUAL reserve and ZERO real SOL**, and
/// escrows `real_sol = virtual_sol - 30 SOL` thereafter. This constant used to be the
/// bare seed reserve (30 SOL) paired with a "sellable depth" of 29-30 SOL — a market
/// that cannot exist, since a curve nobody has bought into can pay out nothing at all.
/// It is now a curve with 0.3 SOL genuinely raised: the price reserve is close enough
/// to the seed that own-impact on a 0.1 SOL floor clip is unchanged at 33 bps a leg,
/// and the payout reserve is the 0.3 SOL that was actually paid in.
/// See `curve_state::real_sol_for`.
const REAL_CURVE_VSOL: u64 = 30_300_000_000;
/// The SOL this curve actually escrows — `REAL_CURVE_VSOL - LAUNCH_VSOL_LAMPORTS`,
/// the identity, not a choice. This is what caps `size_band`'s `x_max`.
const REAL_CURVE_REAL_SOL: u64 = 300_000_000;
/// A curve barely off its seed: 0.15 SOL raised. Re-pin #27 replaces the 0.08-0.2 SOL
/// "pools" these harnesses declared, which are not markets this venue can produce —
/// every curve starts at 30 SOL of VIRTUAL reserve.
const SHALLOW_CURVE_REAL_SOL: u64 = 150_000_000;
const SHALLOW_CURVE_VSOL: u64 = 30_000_000_000 + SHALLOW_CURVE_REAL_SOL;

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
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: a,
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
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
                        liquidity_lamports: SHALLOW_CURVE_VSOL,
                        signed_base: 2_000_000,
                        buyer_entity: (i + round) % 7,
                        age_slots: 20,
                        recv_unix_ms: None,
                        trader_pubkey: None,
                        slot: None,
                        fee_lamports: None,
                        cu_consumed: None,
                        venue: None,
                        event_id: None,
                    });
                }
                e.tick(AppEvent::OnchainConfirm {
                    mint: m,
                    virtual_sol_lamports: SHALLOW_CURVE_VSOL,
                    real_sol_lamports: SHALLOW_CURVE_REAL_SOL,
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
                recv_unix_ms: None,
                trader_pubkey: None,
                slot: None,
                fee_lamports: None,
                cu_consumed: None,
                venue: None,
                event_id: None,
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
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: 1_000_000,
            buyer_entity: i,
            age_slots: 30,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: m,
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    ev.push(AppEvent::Tick);
    ev
}

/// A deep, admissible market so bankroll-derived sizes are small relative to the
/// venue (the §34.4/§21.7 exit-cost law correctly vetoes bankroll-scale sizes against
/// toy-sized pools).
///
/// Re-pin #27: this declared a 100 SOL price reserve with 200 SOL of "proven depth" —
/// twice the whole curve, on a venue where the curve escrows `virtual_sol − 30 SOL`
/// and can never hold more than 85.005 SOL. The reserve is unchanged; the payout it
/// escrows is now the 70 SOL it actually raised.
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
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: m,
        virtual_sol_lamports: 100_000_000_000,
        real_sol_lamports: 70_000_000_000,
    });
    ev.push(AppEvent::Tick);
    ev
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
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
        });
    }
    // 66 tiny-quote buys with big base: the trade ring now holds ONLY buys (CVD>0,
    // OFI strongly positive, price rising -> the lane gate re-qualifies)...
    for i in 0..66u64 {
        e.tick(AppEvent::MarketTrade {
            mint: m,
            price_fp: 995_000_000 + (i as i128) * 500_000,
            quote_lamports: 50,
            liquidity_lamports: SHALLOW_CURVE_VSOL,
            signed_base: 1_000_000,
            buyer_entity: i % 9,
            age_slots: 31,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
        });
    }
    e.tick(AppEvent::OnchainConfirm {
        mint: m,
        virtual_sol_lamports: SHALLOW_CURVE_VSOL,
        real_sol_lamports: SHALLOW_CURVE_REAL_SOL,
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
