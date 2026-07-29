//! The four **closed gap leaves**, proven live through the ENGINE — not just in
//! their leaf crates.
//!
//! Before this wave the engine fed the brain's twenty-field fingerprint four
//! quantities it could not actually measure: `holder_growth_accel_bps` was the
//! literal `0` on every admit, `CreatorClass::Proven` was structurally
//! unreachable, `MetaSaturationState::Decaying` was structurally unreachable, and
//! four of the eight narrative slots were unreachable. A fabricated zero in a
//! recall key is worse than a missing field: it silently pools every unmeasured
//! market into one "class".
//!
//! These tests prove, through the engine's own public seams, that each estimator
//! is now REACHABLE (so the wiring is not dead code) and that each REFUSES below
//! its evidence floor (so the wiring did not replace a fabricated zero with a
//! fabricated estimate).

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, CreatorActionKind};
use pump_quant_app::measured_state::HOLDER_ACCEL_NEUTRAL_BPS;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_parse::fnv1a_64;
use pump_quant_market_state::meta_phase::MetaPhase;
use pump_quant_narrative::narrative_family::NarrativeFamily;
use pump_quant_wallet_graph::creator_ledger::{
    CreatorTrack, CREATOR_MIN_SURVIVED_FOR_PROVEN, CREATOR_SURVIVAL_HORIZON_SLOTS,
};

/// **DEPTH REALISM (re-pin #26).** The gate's price-impact model is now DERIVED from
/// the market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves START at 30 SOL; the sub-SOL depths these fixtures used to declare put the
/// operator's 0.1 SOL floor clip at 20-125% of the pool — a market in which no
/// strategy result means anything (Amendment A-13(1)).
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

const PRICE_SCALE: i128 = 10_000_000;

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xCD;
    Mint::from_bytes(b)
}

fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

fn trade(eng: &mut Engine, m: Mint, price_mult: i128, signed_base: i64, entity: u64) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: price_mult * PRICE_SCALE,
        quote_lamports: 800_000,
        liquidity_lamports: REAL_CURVE_VSOL,
        signed_base,
        buyer_entity: entity,
        age_slots: 30,
    });
}

// ---------------------------------------------------------------------------
// §70.1 holder-growth acceleration
// ---------------------------------------------------------------------------

#[test]
fn holder_growth_refuses_until_measured_then_reads_the_real_acceleration() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = mint(1);
    let id = fnv1a_64(m.as_bytes());

    // An engine that was never fed holder counts refuses, and the fingerprint
    // input falls back to the ladder's documented NEUTRAL rung.
    let ns = |e: &Engine| e.now().saturating_mul(400_000_000);
    assert_eq!(
        eng.measured().holder_growth_accel_bps(id, ns(&eng)),
        None,
        "an unfed holder series must refuse, not read zero"
    );
    assert_eq!(
        eng.measured().holder_growth_accel_input(id, ns(&eng)),
        HOLDER_ACCEL_NEUTRAL_BPS
    );

    // Two samples: still below the three-point floor.
    ticks(&mut eng, 5);
    assert!(eng.observe_holder_count(m.as_bytes(), 100));
    ticks(&mut eng, 5);
    assert!(eng.observe_holder_count(m.as_bytes(), 110));
    assert_eq!(
        eng.measured().holder_growth_accel_bps(id, ns(&eng)),
        None,
        "two samples cannot produce a SECOND difference"
    );

    // Third sample, with growth accelerating (+10% then +27%).
    ticks(&mut eng, 5);
    assert!(eng.observe_holder_count(m.as_bytes(), 140));
    let accel = eng
        .measured()
        .holder_growth_accel_bps(id, ns(&eng))
        .expect("three spaced samples ⇒ a measurement");
    assert!(
        accel > 0,
        "broadening accumulation must read as positive acceleration (got {accel})"
    );
    assert_eq!(
        eng.measured().holder_growth_accel_input(id, ns(&eng)),
        accel,
        "the fingerprint input is the MEASUREMENT once one exists"
    );

    // A non-advancing information time is dropped, never accepted out of order.
    assert!(
        !eng.observe_holder_count(m.as_bytes(), 999),
        "§20: information time never moves backwards"
    );
}

// ---------------------------------------------------------------------------
// §29.9 creator track record — `Proven` is now reachable
// ---------------------------------------------------------------------------

#[test]
fn creator_class_proven_is_reachable_from_engine_observations_alone() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let creator = 0xC0FFEEu64;
    let horizon = CREATOR_SURVIVAL_HORIZON_SLOTS;
    let n = u64::from(CREATOR_MIN_SURVIVED_FOR_PROVEN);

    // Launch + graduate `n` tokens, using only the engine's own decoded events.
    for i in 0..n {
        let m = mint(100 + i);
        eng.tick(AppEvent::TokenMetadata {
            mint: m,
            category_id: 1,
            taxonomy_version: 1,
            creator,
            slot: 10 + i,
        });
        eng.tick(AppEvent::Migration {
            mint: m,
            slot: 20 + i,
        });
    }
    assert_eq!(
        eng.measured().creator_track(creator),
        CreatorTrack::Unknown,
        "graduation alone is not survival — nothing may be asserted early"
    );

    // Let the survival horizon elapse (observed via any slot-bearing event).
    let late = mint(999);
    eng.tick(AppEvent::TokenMetadata {
        mint: late,
        category_id: 1,
        taxonomy_version: 1,
        creator: 0xBEEF,
        slot: 30 + horizon,
    });
    assert_eq!(
        eng.measured().creator_track(creator),
        CreatorTrack::Proven,
        "a deployer whose launches migrated and SURVIVED un-rugged is Proven — a \
         class the fingerprint could never previously express"
    );

    // A confirmed creator dump on ANY of their launches dominates: risk first.
    let dumped = mint(100);
    eng.tick(AppEvent::CreatorAction {
        mint: dumped,
        kind: CreatorActionKind::Init {
            initial_tokens: 10_000,
            total_supply: 10_000,
        },
        slot: 31 + horizon,
    });
    eng.tick(AppEvent::CreatorAction {
        mint: dumped,
        kind: CreatorActionKind::Sell {
            tokens: 10_000,
            quote_lamports: 5_000_000,
        },
        slot: 32 + horizon,
    });
    assert_eq!(
        eng.measured().creator_track(creator),
        CreatorTrack::Toxic,
        "a recorded rug dominates a survival record — a deployer who has rugged \
         is not 'also proven'"
    );
}

#[test]
fn an_unobserved_creator_stays_unknown_and_the_field_can_carry_that() {
    let eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    assert_eq!(eng.measured().creator_track(42), CreatorTrack::Unknown);
    assert_eq!(eng.measured().chain_slot(), 0, "no slot-bearing fact seen");
}

// ---------------------------------------------------------------------------
// §21.4 meta lifecycle — `Decaying` is now reachable
// ---------------------------------------------------------------------------

#[test]
fn meta_phase_decaying_is_reachable_from_the_reflection_sampler() {
    let mut cfg = Config::dev_portable();
    // Sample often enough to build a series inside a short tape.
    cfg.reflect_every_ticks = 5;
    let mut eng = Engine::new(cfg, RunMode::Replay);

    // Three category-1 launches from three distinct creators (participation), then
    // heavy activity that decays away round over round.
    let markets: Vec<Mint> = (0..3u64).map(|i| mint(200 + i)).collect();
    for (i, m) in markets.iter().enumerate() {
        eng.tick(AppEvent::TokenMetadata {
            mint: *m,
            category_id: 1,
            taxonomy_version: 1,
            creator: 500 + i as u64,
            slot: 1 + i as u64,
        });
    }
    // Rising phase: dense two-sided flow.
    for round in 0..2u64 {
        for m in &markets {
            for i in 0..12u64 {
                trade(&mut eng, *m, 100 + i128::from(round as i64), 400_000, i % 5);
                trade(
                    &mut eng,
                    *m,
                    100 + i128::from(round as i64),
                    -300_000,
                    i % 5,
                );
            }
        }
        ticks(&mut eng, 6);
    }
    // Decaying phase: activity collapses and flow turns net-sell.
    for _ in 0..6u64 {
        for m in &markets {
            trade(&mut eng, *m, 100, -900_000, 1);
        }
        ticks(&mut eng, 6);
    }
    assert_eq!(
        eng.measured().meta_phase_of(1, eng.now()),
        Some(MetaPhase::Decaying),
        "participation and activity BOTH falling off a prior peak is Decaying — \
         the state the app's old rotation vocabulary (emerging / saturating / \
         running) had no way to express at all, and the state in which new \
         entrants are exit liquidity"
    );
    // An untracked category still refuses — no fabricated neutral phase.
    assert_eq!(eng.measured().meta_phase_of(9_999, eng.now()), None);
}

// ---------------------------------------------------------------------------
// §21.4 narrative family — the previously unreachable slots
// ---------------------------------------------------------------------------

#[test]
fn narrative_family_reaches_the_slots_the_four_way_class_never_could() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);

    let animal =
        eng.observe_launch_metadata(mint(300).as_bytes(), "Doge Killer", "DOGE", None, None);
    assert_eq!(animal, NarrativeFamily::Animal);

    let stream =
        eng.observe_launch_metadata(mint(301).as_bytes(), "Whatever", "WTV", Some(true), None);
    assert_eq!(stream, NarrativeFamily::Stream);

    let seasonal =
        eng.observe_launch_metadata(mint(302).as_bytes(), "Santa Rally", "XMAS", None, None);
    assert_eq!(seasonal, NarrativeFamily::Seasonal);

    let derivative = eng.observe_launch_metadata(
        mint(303).as_bytes(),
        "Doge Killer",
        "DOGE",
        None,
        Some(9_500),
    );
    assert_eq!(
        derivative,
        NarrativeFamily::Derivative,
        "a measured metadata clone outranks whatever theme it copied"
    );

    // No evidence stays UNCLASSIFIED — a refusal this nominal field CAN carry.
    let none = eng.observe_launch_metadata(mint(304).as_bytes(), "Zorble", "ZRB", None, None);
    assert_eq!(none, NarrativeFamily::Unclassified);

    // First classification wins: a launch's family is a launch-time fact and a
    // later re-read must not re-label an already-fingerprinted mint (§81).
    let again = eng.observe_launch_metadata(mint(304).as_bytes(), "Doge", "DOGE", None, None);
    assert_eq!(again, NarrativeFamily::Unclassified);

    // An unobserved mint has no family at all.
    assert!(eng.measured().family_of(mint(999).as_bytes()).is_none());
}

// ---------------------------------------------------------------------------
// Decision inertness of the whole measured plane
// ---------------------------------------------------------------------------

#[test]
fn the_measured_plane_is_decision_inert() {
    // Identical tape; the second arm feeds every new estimator. The fingerprint
    // fields these change are read ONLY by the episodic memory, which is
    // decision-inert unless the reduce-only haircut is armed (default OFF).
    let drive = |feed: bool| {
        let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
        let m = mint(400);
        eng.tick(AppEvent::TokenMetadata {
            mint: m,
            category_id: 1,
            taxonomy_version: 1,
            creator: 77,
            slot: 1,
        });
        for round in 0..4u64 {
            for i in 0..10u64 {
                trade(
                    &mut eng,
                    m,
                    100 + i128::from(i as i64),
                    900_000 - i as i64,
                    i % 7,
                );
            }
            eng.tick(AppEvent::OnchainConfirm {
                mint: m,
                virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_CURVE_REAL_SOL,
            });
            if feed {
                eng.observe_holder_count(m.as_bytes(), 50 + round * 25);
                eng.observe_launch_metadata(m.as_bytes(), "Doge Santa", "DOGE", Some(true), None);
            }
            ticks(&mut eng, 4);
        }
        eng.report()
    };
    let plain = drive(false);
    let fed = drive(true);
    assert_eq!(
        fed.journal_digest, plain.journal_digest,
        "the measured estimators must not reach the DECISION JOURNAL"
    );
    assert_eq!(fed.admitted, plain.admitted);
    assert_eq!(fed.rejected, plain.rejected);
    assert_eq!(fed.promoted, plain.promoted);
    assert_eq!(fed.net_lamports, plain.net_lamports);
    assert!(
        fed.brain_episodes_recorded > 0,
        "the fed arm must actually seal episodes, else this is vacuous"
    );
}
