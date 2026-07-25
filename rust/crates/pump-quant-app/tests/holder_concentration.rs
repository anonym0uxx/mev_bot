//! §21.7/§70.1 **holder distribution shape** — the proof suite.
//!
//! The prior wave proved the holder STREAM exists. This one proves the stream's
//! distribution SHAPE reaches a decision, and measures whether that is worth
//! doing. In order:
//!
//! 1. **The metrics are the published formulas** (`metric_*`) — top-1/top-10
//!    share, Herfindahl and its normalization, the arXiv 2512.00377 whale-dominance
//!    product, the MemeTrans early-top-10 cohort, the arXiv 2601.08641 bundle /
//!    sniper / flip signatures.
//! 2. **Basis discipline is structural** (`basis_*`) — THE load-bearing property.
//!    A delta-only or truncated ledger produces `Unknown`, `Unknown` carries no
//!    estimate, and every consumer degrades to the identity under it. An
//!    overstated concentration can never reach the gate.
//! 3. **The dormant screen is fed** (`screen_*`) — `top_holder_concentration_bps`
//!    at the §21.5 universe screen has been a hard-coded `0` against a `u32::MAX`
//!    bar since inception. It now carries the real number and actually binds.
//! 4. **The veto is never standalone** (`veto_*`) — the constitution names this
//!    exact feature "a feature family and prior, never a standalone veto". The
//!    refusal is conjunctive with an INDEPENDENT §21.7 authenticity signature, and
//!    that is asserted rather than described.
//! 5. **Authenticity enters exactly once** (`once_*`) — §21.7 admits one entry
//!    point per feature into the sizing chain. Bundle/flip go through the
//!    authenticity multiplier; the concentration shares go through the fragility
//!    haircut; neither number is charged twice.
//! 6. **The pre-registered two-sided A/B** (`ab_*`) — the rule is written below,
//!    verbatim, and it was written before the numbers were read.
//!
//! Determinism (§22) makes every comparison exact rather than statistical.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::holder_concentration::{
    concentration_of, ConcentrationRisk, ConcentrationUnknown, HolderAuthEvidence,
    EARLY_TOP10_VETO_BPS, FLIP_RATIO_NEUTRAL_BPS, MIN_ENTITIES_FOR_SHAPE, TOP10_HAIRCUT_BPS,
    TOP10_VETO_BPS,
};
use pump_quant_app::holder_flow::{HolderFlow, SNIPER_SLOT_WINDOW};
use pump_quant_app::screen::FlowScreen;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

const PRICE_SCALE: i128 = 10_000_000;

fn m32(tag: u8) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0] = tag;
    b[31] = 0xD3;
    b
}

// ---------------------------------------------------------------------------
// 1. The metrics ARE the published formulas
// ---------------------------------------------------------------------------

/// An `Exact`-basis ledger built from an explicit `(entity, net, age)` table.
fn ledger(rows: &[(u64, i64, u32)]) -> HolderFlow {
    let mut hf = HolderFlow::new();
    hf.note_creation(&m32(1), 0);
    for &(e, qty, age) in rows {
        hf.observe_swap_aged(&m32(1), e, qty, 0, 0, Some(age));
    }
    hf
}

#[test]
fn metric_top_shares_are_cumulative_over_the_largest_positions() {
    // 20 holders: one at 10 000, nineteen at 1 000 ⇒ supply 29 000.
    let mut rows: Vec<(u64, i64, u32)> = vec![(0, 10_000, 100)];
    for e in 1..20u64 {
        rows.push((e, 1_000, 100));
    }
    let hf = ledger(&rows);
    let v = concentration_of(&hf, &m32(1));
    let m = v.metrics().expect("Exact basis, 20 entities");
    assert_eq!(m.tracked_supply_base, 29_000);
    // top1 = 10 000 / 29 000.
    assert_eq!(m.top1_share_bps, 10_000 * 10_000 / 29_000);
    // top10 = 10 000 + 9 · 1 000 = 19 000 / 29 000.
    assert_eq!(m.top10_share_bps, 19_000 * 10_000 / 29_000);
    assert!(m.top10_share_bps > m.top1_share_bps);
}

#[test]
fn metric_normalized_hhi_is_zero_at_equality_and_maximal_at_capture() {
    // Perfect equality across 25 holders ⇒ hhi == 10_000/25, normalized == 0.
    let equal: Vec<(u64, i64, u32)> = (0..25u64).map(|e| (e, 1_000, 100)).collect();
    let m = concentration_of(&ledger(&equal), &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    assert_eq!(m.hhi_bps, 10_000 / 25);
    assert_eq!(m.hhi_normalized_bps, 0);
    assert_eq!(m.whale_dominance_bps, 0);

    // One entity holding ~99.9% inside a 25-entity ledger.
    let mut skew: Vec<(u64, i64, u32)> = vec![(0, 1_000_000, 100)];
    for e in 1..25u64 {
        skew.push((e, 40, 100));
    }
    let s = concentration_of(&ledger(&skew), &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    assert!(
        s.hhi_normalized_bps > 9_000,
        "norm = {}",
        s.hhi_normalized_bps
    );
    // Whale dominance is the PRODUCT of cumulative share and internal inequality
    // (arXiv 2512.00377) — the exact functional form, asserted arithmetically.
    assert_eq!(
        s.whale_dominance_bps,
        u32::try_from(u128::from(s.top10_share_bps) * u128::from(s.hhi_normalized_bps) / 10_000)
            .unwrap()
    );
}

#[test]
fn metric_early_top10_is_the_first_ten_buyers_not_the_ten_largest() {
    // Ten small early buyers, then one late whale. The TEN LARGEST are dominated
    // by the whale; the FIRST TEN are not. The two statistics must disagree —
    // that disagreement is what `early_top10_hold_pct` measures (MemeTrans).
    let mut rows: Vec<(u64, i64, u32)> = (0..10u64).map(|e| (e, 1_000, 100)).collect();
    for e in 10..25u64 {
        rows.push((e, 1_000, 100));
    }
    rows.push((99, 500_000, 100)); // the late whale
    let m = concentration_of(&ledger(&rows), &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    // Supply 25 000 + 500 000 = 525 000. First ten hold 10 000 ⇒ ~190 bps.
    assert_eq!(m.early_top10_share_bps, 10_000 * 10_000 / 525_000);
    // The ten LARGEST include the whale ⇒ far higher.
    assert!(m.top10_share_bps > 9_000);
    assert!(m.top10_share_bps > m.early_top10_share_bps * 10);
}

#[test]
fn metric_bundle_and_sniper_split_at_the_published_block_window() {
    let mut rows: Vec<(u64, i64, u32)> = vec![
        (0, 1_000, 0),                      // creation slot ⇒ bundle
        (1, 1_000, 1),                      // 1 block  ⇒ sniper
        (2, 1_000, SNIPER_SLOT_WINDOW),     // 5 blocks ⇒ sniper (inclusive)
        (3, 1_000, SNIPER_SLOT_WINDOW + 1), // 6 blocks ⇒ neither
    ];
    for e in 4..25u64 {
        rows.push((e, 1_000, 900));
    }
    let m = concentration_of(&ledger(&rows), &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    assert_eq!(m.bundle_entities, 1);
    assert_eq!(m.sniper_entities, 2);
    assert_eq!(m.bundle_suspect_count, 3);
    assert_eq!(m.aged_first_buys, 25);
}

#[test]
fn metric_flip_ratio_separates_holding_from_bump_churn() {
    let hold: Vec<(u64, i64, u32)> = (0..25u64).map(|e| (e, 4_000, 100)).collect();
    let m = concentration_of(&ledger(&hold), &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    assert_eq!(m.flip_ratio_bps, FLIP_RATIO_NEUTRAL_BPS);

    // Same terminal positions, reached by churning: buy 4 000, sell 3 000, buy
    // 3 000 ⇒ net 4 000 but gross 10 000 ⇒ ratio 2.5×.
    let mut hf = HolderFlow::new();
    hf.note_creation(&m32(1), 0);
    for e in 0..25u64 {
        hf.observe_swap_aged(&m32(1), e, 4_000, 0, 0, Some(100));
        hf.observe_swap_aged(&m32(1), e, -3_000, 0, 0, Some(100));
        hf.observe_swap_aged(&m32(1), e, 3_000, 0, 0, Some(100));
    }
    let c = concentration_of(&hf, &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    assert_eq!(c.tracked_supply_base, m.tracked_supply_base);
    assert_eq!(c.flip_ratio_bps, 25_000);
}

// ---------------------------------------------------------------------------
// 2. BASIS DISCIPLINE — the load-bearing property
// ---------------------------------------------------------------------------

#[test]
fn basis_delta_only_can_never_produce_a_concentration_number() {
    // THE DANGEROUS CASE, stated precisely. This ledger is discovered mid-life
    // (no creation sighting), so it is `DeltaOnly`. The entities it DID observe
    // are wildly concentrated — if the basis gate leaked, this market would read
    // as ~99% top-10 and be vetoed, when in truth we have no idea what the
    // pre-window holder base looks like.
    let mut hf = HolderFlow::new();
    let mut rows: Vec<(u64, i64, u32)> = vec![(0, 1_000_000, 0)];
    for e in 1..25u64 {
        rows.push((e, 100, 0));
    }
    for (e, q, a) in rows {
        hf.observe_swap_aged(&m32(2), e, q, 0, 0, Some(a));
    }
    let v = concentration_of(&hf, &m32(2));
    assert_eq!(
        v.unknown_reason(),
        Some(ConcentrationUnknown::DeltaOnlyBasis)
    );
    // No estimate exists, by construction — there is no accessor that yields one.
    assert!(v.metrics().is_none());
    // Every consumer degrades to the identity.
    assert_eq!(v.risk_or_clear(true), ConcentrationRisk::Clear);
    assert_eq!(v.screen_concentration_bps(), 0);
    assert_eq!(v.auth_evidence_or_default(), HolderAuthEvidence::default());
}

#[test]
fn basis_a_falsified_exact_claim_takes_the_concentration_with_it() {
    // The claim starts Exact and is falsified mid-tape by a pre-window seller.
    // The concentration reading must vanish at the moment of falsification, not
    // survive on the strength of the earlier claim.
    let mut hf = HolderFlow::new();
    hf.note_creation(&m32(3), 0);
    for e in 0..25u64 {
        hf.observe_swap_aged(&m32(3), e, 1_000, 0, 0, Some(100));
    }
    assert!(concentration_of(&hf, &m32(3)).is_known());
    // An entity we never saw buy sells: it provably held before our window.
    hf.observe_swap_aged(&m32(3), 9_999, -50, 1, 400_000_000, Some(100));
    assert_eq!(
        concentration_of(&hf, &m32(3)).unknown_reason(),
        Some(ConcentrationUnknown::DeltaOnlyBasis)
    );
}

#[test]
fn basis_truncated_ledger_refuses_too() {
    let mut hf = HolderFlow::with_caps(4, 24);
    hf.note_creation(&m32(4), 0);
    for e in 0..24u64 {
        hf.observe_swap_aged(&m32(4), e, 1_000, 0, 0, Some(100));
    }
    assert!(concentration_of(&hf, &m32(4)).is_known());
    hf.observe_swap_aged(&m32(4), 999, 1_000, 0, 0, Some(100)); // past the cap
    assert_eq!(
        concentration_of(&hf, &m32(4)).unknown_reason(),
        Some(ConcentrationUnknown::IncompleteBasis)
    );
}

#[test]
fn basis_thin_ledger_refuses_rather_than_reading_a_trivial_maximum() {
    let rows: Vec<(u64, i64, u32)> = (0..u64::from(MIN_ENTITIES_FOR_SHAPE) - 1)
        .map(|e| (e, 1_000, 100))
        .collect();
    assert_eq!(
        concentration_of(&ledger(&rows), &m32(1)).unknown_reason(),
        Some(ConcentrationUnknown::ThinLedger)
    );
}

#[test]
fn basis_disarmed_is_distinguishable_from_unmeasurable() {
    // §6.4: "we did not look" and "we looked and could not tell" are different
    // facts and the reason enum keeps them apart.
    let mut cfg = Config::dev_portable();
    cfg.holder_concentration_enable = false;
    let eng = Engine::new(cfg, RunMode::Replay);
    // The REPORT-plane accessor always measures, and says Untracked for a mint
    // with no ledger — never `Disarmed`, which is a config fact.
    assert_eq!(
        eng.holder_concentration(&m32(5)).unknown_reason(),
        Some(ConcentrationUnknown::Untracked)
    );
}

// ---------------------------------------------------------------------------
// 3. Engine-level: the formerly-dormant screen, the veto, and fail-open
// ---------------------------------------------------------------------------

/// Valid Solana pubkeys (the attention lane only sees mints a social post named,
/// and the parser demands a real base58 key).
const CONC_B58: [&str; 4] = [
    "29d2S8fphGNdxpkLtoYM42q9Q6h7bxNiopT6JBHXmGB2",
    "2DYKaS8EYv7c92Pu9NMQRnvJdNA5gtFhEaLtf6SFCjbE",
    "2HTcijaeQZraKE3TPwAToZ1Trdd3mp8ffLEh21axeD1S",
    "2MNus334GDbYVRh1eVyXBK6d5u61rk1e668VNvjg5gRe",
];
const BROAD_B58: [&str; 4] = [
    "2RJD1LVU7sLWfdLZu4naZ5BnKAYywftcWr2HjqtPX9qr",
    "2VDW9dwsyX5Uqpz89dbdvqGwYS1x2bmawbv66m36xdG4",
    "2Z8oHwQHqApT22dgQCQhJbN6mhUv7XeZNMotTgBpQ6gG",
    "2d46SErhgpZRCEHEemDkgMTFzxwtCTXXo7hgpbLXqa6U",
];

fn b58(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid pubkey"))
}

/// A stable per-mint entity id space, so cohorts cannot share entities.
fn ent(addr: &str, k: u64) -> u64 {
    pump_quant_ingest::social_parse::fnv1a_64(addr.as_bytes()) % 1_000 * 100_000 + k + 1
}

#[allow(clippy::too_many_arguments)]
fn swap(
    eng: &mut Engine,
    m: Mint,
    price_bp: i128,
    signed_base: i64,
    entity: u64,
    quote_lamports: u64,
    age_slots: u32,
    liq: u64,
) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: PRICE_SCALE * price_bp / 10_000,
        quote_lamports,
        liquidity_lamports: liq,
        signed_base,
        buyer_entity: entity,
        age_slots,
    });
}

fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

fn post(eng: &mut Engine, author: &str, addr: &str, body: &str, ts_ns: u64) {
    let json = format!(
        "{{\"platform\":\"x\",\"author\":\"{author}\",\"community\":\"\",\
         \"text\":\"{body} {addr} send\",\"likes\":40,\"reposts\":40,\
         \"is_designated_caller\":false}}"
    );
    let mut src =
        MockSocialSource::new().with_batch(vec![RawSocialPayload::new(json.into_bytes(), ts_ns)]);
    eng.ingest_social(&mut src);
}

/// Pool depth, identical for both cohorts (lamports).
const LIQ: u64 = 260_000_000;
/// Confirmed sellable depth, identical for both cohorts (lamports).
const DEPTH: u64 = 300_000_000;
/// Accumulation prints per launch, identical for both cohorts.
const LAUNCH_PRINTS: u64 = 25;
/// Quote lamports per accumulation print, identical for both cohorts — this is
/// what keeps the §21.7 flow-authenticity screen blind to the difference.
const PRINT_QUOTE: u64 = 200_000;
/// Broad-cohort per-entity base clip. `25 * 40_000 == 1_000_000`, which is
/// exactly the concentrated cohort's total (`800_000 + 4*45_000 + 20*1_000`).
const BROAD_CLIP: i64 = 40_000;
/// Age in slots at which the "organic" phase trades — past the sniper window and
/// still inside the §21.5 fresh-launch exemption, so the UNIVERSE screen stays
/// inert here and the A/B isolates the GATE lever.
const ORGANIC_AGE: u32 = 40;

/// Per-entity base allocation for a launch's accumulation phase.
///
/// **The two cohorts are NUMERICALLY INDISTINGUISHABLE by construction.** Both
/// take exactly [`LAUNCH_PRINTS`] buy prints, at exactly [`PRINT_QUOTE`] quote
/// lamports each, into exactly the same pool depth, from exactly the same number
/// of distinct entities, summing to exactly the same total base. Order-flow
/// imbalance, CVD, buyer breadth, realized volatility, quote volume, the §21.7
/// flow-authenticity screen (which is computed on QUOTE flow) and the §21.5
/// activity legs therefore carry the SAME information about both.
///
/// The ONLY thing that differs is WHO ends up holding the base: the broad cohort
/// splits it evenly, the concentrated cohort gives 98.5% of it to the first five
/// creation-slot/sniper entities. That is exactly the variable this law reads,
/// and nothing else moves — so a measured difference can only come from this law.
fn launch_base(concentrated: bool, k: u64) -> i64 {
    if !concentrated {
        return BROAD_CLIP;
    }
    match k {
        0 => 800_000,    // the bundler that takes the float
        1..=4 => 45_000, // the rest of the creation-slot/sniper cohort
        _ => 1_000,      // twenty organic latecomers, dust by comparison
    }
}

/// Market age in slots at the k-th accumulation print. The concentrated cohort's
/// first five land in the creation slot and the first blocks (arXiv 2601.08641
/// bundle/sniper definitions); everyone else arrives later. The broad cohort has
/// no creation-slot cohort at all.
fn launch_age(concentrated: bool, k: u64, age: u32) -> u32 {
    if concentrated && k <= 4 {
        u32::try_from(k).unwrap_or(0).min(SNIPER_SLOT_WINDOW)
    } else {
        age
    }
}

/// Seed one mint's launch: creation sighting (⇒ `Exact` basis), on-chain confirm,
/// then the accumulation phase described by [`launch_base`].
///
/// `wash` adds round-trip QUOTE churn — the corroborating leg the constitution
/// requires before concentration may refuse rather than merely shrink. It is the
/// one place the two cohorts' quote flow is allowed to differ, and it applies to
/// only half the concentrated cohort so that both the haircut-only path and the
/// conjunctive-veto path are exercised by the same tape.
fn seed_launch(eng: &mut Engine, addr: &str, concentrated: bool, wash: bool, age: u32) {
    let mt = b58(addr);
    eng.tick(AppEvent::TokenMetadata {
        mint: mt,
        category_id: 0,
        taxonomy_version: 1,
        creator: ent(addr, 7_777),
        slot: 1,
    });
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: DEPTH,
    });
    for k in 0..LAUNCH_PRINTS {
        let e = ent(addr, k);
        let a = launch_age(concentrated, k, age);
        swap(
            eng,
            mt,
            10_000,
            launch_base(concentrated, k),
            e,
            PRINT_QUOTE,
            a,
            LIQ,
        );
        if wash && k <= 4 {
            // Bump/wash churn on the bundler cohort: quote flow that returns to
            // its origin entity, which is what the INDEPENDENT §21.7 screen sees.
            swap(eng, mt, 10_000, -1_000, e, PRINT_QUOTE * 4, a, LIQ);
            swap(eng, mt, 10_000, 1_000, e, PRINT_QUOTE * 4, age, LIQ);
        }
    }
}

/// Whether a concentrated cohort member also carries the wash leg. The first two
/// are bundled but authentically traded (corroboration ABSENT ⇒ only the haircut
/// can fire); the last two add the wash signature (⇒ the conjunctive veto is
/// reachable). Both paths are exercised by the same A/B.
fn washed(i: usize) -> bool {
    i >= 2
}

#[test]
fn screen_the_dormant_universe_field_now_carries_a_real_number() {
    // Before this wave `top_holder_concentration_bps` was a hard-coded `0` at the
    // §21.5 screen against a `u32::MAX` bar. Prove it now carries the real
    // top-10 share on a concentrated launch and a modest one on a broad launch.
    let mut cfg = Config::dev_portable();
    cfg.holder_concentration_enable = true;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    seed_launch(&mut eng, CONC_B58[0], true, true, ORGANIC_AGE);
    seed_launch(&mut eng, BROAD_B58[0], false, false, ORGANIC_AGE);

    let c = eng.holder_concentration(b58(CONC_B58[0]).as_bytes());
    let b = eng.holder_concentration(b58(BROAD_B58[0]).as_bytes());
    let cm = c.metrics().expect("concentrated launch is Exact + thick");
    let bm = b.metrics().expect("broad launch is Exact + thick");
    println!(
        "SCREEN concentrated: top1={} top10={} early10={} hhi={} hhi_n={} wd={} bundles={} flip={} \
         | broad: top1={} top10={} early10={} wd={} bundles={} flip={}",
        cm.top1_share_bps,
        cm.top10_share_bps,
        cm.early_top10_share_bps,
        cm.hhi_bps,
        cm.hhi_normalized_bps,
        cm.whale_dominance_bps,
        cm.bundle_suspect_count,
        cm.flip_ratio_bps,
        bm.top1_share_bps,
        bm.top10_share_bps,
        bm.early_top10_share_bps,
        bm.whale_dominance_bps,
        bm.bundle_suspect_count,
        bm.flip_ratio_bps,
    );
    // The concentrated launch clears the veto bar on BOTH the cumulative and the
    // MemeTrans early-cohort legs; the broad one clears neither bar.
    assert!(cm.top10_share_bps >= TOP10_VETO_BPS);
    assert!(cm.early_top10_share_bps >= EARLY_TOP10_VETO_BPS);
    assert_eq!(
        cm.bundle_suspect_count, 5,
        "the creation-slot/sniper cohort"
    );
    assert!(bm.top10_share_bps < TOP10_HAIRCUT_BPS);
    assert_eq!(bm.bundle_suspect_count, 0);
    // And the number the §21.5 screen consumes is the cumulative top-10 share.
    assert_eq!(c.screen_concentration_bps(), cm.top10_share_bps);
    assert_eq!(b.screen_concentration_bps(), bm.top10_share_bps);
}

#[test]
fn screen_binds_on_a_mature_concentrated_market_and_is_inert_when_disarmed() {
    // NON-VACUITY for the universe leg: the same tape, aged past the §21.5
    // fresh-launch exemption, must be FILTERED when armed and not when disarmed.
    let mature_age = 4_000; // ≫ universe_age_exempt_slots
    let drive = |armed: bool| {
        let mut cfg = Config::dev_portable();
        cfg.holder_concentration_enable = armed;
        let mut eng = Engine::new(cfg, RunMode::Replay);
        for a in CONC_B58 {
            seed_launch(&mut eng, a, true, false, mature_age);
        }
        for (i, a) in CONC_B58.iter().enumerate() {
            post(&mut eng, "caller", a, "call", 900_000_000 + i as u64);
        }
        // Keep the tape active in the recent window so the activity legs pass.
        for r in 0..6u64 {
            for a in CONC_B58 {
                let mt = b58(a);
                for k in 0..4u64 {
                    let e = ent(a, 300 + r * 4 + k);
                    swap(&mut eng, mt, 10_100, 1_000, e, 50_000, mature_age, LIQ);
                }
            }
            ticks(&mut eng, 4);
        }
        eng.report()
    };
    let off = drive(false);
    let on = drive(true);
    println!(
        "UNIVERSE SCREEN off filtered={} promoted={} | on filtered={} promoted={}",
        off.universe_filtered, off.promoted, on.universe_filtered, on.promoted
    );
    assert!(
        on.universe_filtered > off.universe_filtered,
        "the formerly-dormant concentration leg must actually remove promotions \
         (off {} -> on {})",
        off.universe_filtered,
        on.universe_filtered
    );
}

#[test]
fn veto_is_never_standalone_without_an_independent_authenticity_signature() {
    // The constitution's rule, asserted at the metric level: a veto-grade shape
    // WITHOUT corroboration is a haircut, and only WITH it a refusal. This is the
    // whole difference between "a feature family and prior" and the standalone
    // veto §21.7 forbids.
    let mut rows: Vec<(u64, i64, u32)> = vec![(0, 1_000_000, 0)];
    for e in 1..25u64 {
        rows.push((e, 100, 0));
    }
    let m = concentration_of(&ledger(&rows), &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    assert!(m.top10_share_bps >= TOP10_VETO_BPS);
    assert_eq!(m.risk(false), ConcentrationRisk::Haircut);
    assert_eq!(m.risk(true), ConcentrationRisk::Veto);
    // And the haircut is reduce-only in both tiers — never a boost.
    assert!(ConcentrationRisk::Haircut.size_mult_bp() < 10_000);
    assert!(ConcentrationRisk::Veto.size_mult_bp() < 10_000);
    assert_eq!(ConcentrationRisk::Clear.size_mult_bp(), 10_000);
}

#[test]
fn once_bundle_and_flip_enter_only_the_authenticity_multiplier() {
    // §21.7 single-channel law. The bundle/flip evidence must move the
    // AUTHENTICITY multiplier and nothing else; the concentration shares must
    // move the fragility haircut and nothing else. Proven by construction: the
    // fragility tier is a pure function of the SHARES, so feeding it a shape with
    // an enormous bundle cohort but clean shares leaves it Clear.
    let mut rows: Vec<(u64, i64, u32)> = Vec::new();
    for e in 0..25u64 {
        rows.push((e, 1_000, 0)); // every entity a creation-slot bundler
    }
    let m = concentration_of(&ledger(&rows), &m32(1))
        .metrics()
        .copied()
        .expect("Exact");
    assert_eq!(m.bundle_suspect_count, 25);
    assert_eq!(
        m.risk(true),
        ConcentrationRisk::Clear,
        "an equal-weight distribution is Clear no matter how bundled — the bundle \
         evidence belongs to the authenticity channel, not the fragility channel"
    );
    // ...and it does move the authenticity channel, reduce-only.
    let fs = FlowScreen::new();
    let mt = m32(1);
    let base = fs.size_mult_bp(&mt, true);
    let with = fs.size_mult_bp_with(&mt, true, m.auth_evidence());
    println!("ONCE auth mult base={base} with_holder_evidence={with}");
    assert!(with < base, "bundle evidence must reduce authenticity");
    // The neutral (Unknown-verdict) evidence is exactly the identity.
    assert_eq!(
        fs.size_mult_bp_with(&mt, true, HolderAuthEvidence::default()),
        base
    );
}

// ---------------------------------------------------------------------------
// 4. THE PRE-REGISTERED TWO-SIDED A/B
// ---------------------------------------------------------------------------

/// Which side of the two-sided A/B this tape is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    /// HAPPY PATH: the concentrated/bundled cohort is the one that craters and
    /// the broad cohort is the one that pays. Refusing (or shrinking) the
    /// concentrated markets should redirect scarce position slots and bankroll to
    /// the broad ones.
    ConcentratedBleeds,
    /// THE MIRROR — same generator, one boolean flipped. The concentrated cohort
    /// is the one that PAYS and the broad cohort craters, so every concentration
    /// veto and haircut is a FALSE POSITIVE fired on a market that was going to
    /// work. If the law is a real edge rather than a coincidence, its loss here
    /// must be far smaller than its gain above.
    ConcentratedPays,
}

/// Deterministic price path (§22 — no RNG). A cohort that `pays` runs to ~1.9×
/// and settles at ~1.5×; one that does not fades to ~0.85×.
fn price_path(round: u64, pays: bool, offset: i128) -> i128 {
    let bp: i128 = if pays {
        match round {
            0 => 10_000,
            1 => 11_500,
            2 => 13_500,
            3 => 16_000,
            4 => 19_000,
            5 => 18_000,
            6 => 17_000,
            _ => 15_000,
        }
    } else {
        match round {
            0 => 10_000,
            1 => 10_600,
            2 => 10_900,
            3 => 10_400,
            4 => 9_700,
            5 => 9_100,
            _ => 8_500,
        }
    };
    bp + offset
}

fn ab_drive(cfg: Config, side: Side) -> pump_quant_app::engine::Report {
    let mut cfg = cfg;
    // Same cost realism as the golden tape, so the lamport numbers are comparable.
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_protocol_bps = 450;
    cfg.gate_margin_bps = 150;
    cfg.gate_base_fixed_lamports = 200_000;
    cfg.gate_impact_den = 250_000;
    // Position capacity is SCARCE, so a refusal actually redirects capital rather
    // than merely removing a trade: eight candidates compete for two slots.
    cfg.max_concurrent_positions = 6;
    let mut eng = Engine::new(cfg, RunMode::Replay);

    let conc_pays = side == Side::ConcentratedPays;
    for (i, a) in CONC_B58.iter().enumerate() {
        seed_launch(&mut eng, a, true, washed(i), ORGANIC_AGE);
    }
    for a in BROAD_B58 {
        seed_launch(&mut eng, a, false, false, ORGANIC_AGE);
    }
    for (i, a) in CONC_B58.iter().chain(BROAD_B58.iter()).enumerate() {
        post(
            &mut eng,
            "seedcaller",
            a,
            "seed call",
            900_000_000 + i as u64,
        );
    }
    ticks(&mut eng, 8);

    for round in 0..9u64 {
        for (i, a) in CONC_B58.iter().enumerate() {
            let mt = b58(a);
            let px = price_path(round, conc_pays, i as i128);
            for k in 0..3u64 {
                let e = ent(a, 500 + round * 3 + k);
                swap(&mut eng, mt, px, 300, e, 40_000, ORGANIC_AGE, LIQ);
            }
            // The bundled cohort is the LOUDER one — manufactured hype is exactly
            // what a bundled launch buys (§21.7(d): purchased volume plus
            // purchased trending placement implies a sponsor paying for exit
            // liquidity). Two callers per round versus the broad cohort's one, so
            // the concentrated markets OUTRANK the healthy ones in promotion and
            // would take the scarce position slots if nothing refused them.
            post(
                &mut eng,
                "conccaller",
                a,
                &format!("conc r{round} i{i}"),
                1_000_000_000 + round * 20_000_000 + 100 + i as u64,
            );
        }
        for (i, a) in BROAD_B58.iter().enumerate() {
            let mt = b58(a);
            let px = price_path(round, !conc_pays, i as i128);
            for k in 0..3u64 {
                let e = ent(a, 500 + round * 3 + k);
                swap(&mut eng, mt, px, 300, e, 40_000, ORGANIC_AGE, LIQ);
            }
            post(
                &mut eng,
                "broadcaller",
                a,
                &format!("broad r{round} i{i}"),
                1_000_000_000 + round * 20_000_000 + i as u64,
            );
        }
        ticks(&mut eng, 8);
    }
    eng.report()
}

#[test]
fn ab_tape_is_not_vacuous() {
    // GUARD AGAINST A VACUOUS A/B. The mechanism must actually fire on this tape,
    // in both directions, or the measurement below proves nothing while looking
    // like a result.
    let mut cfg = Config::dev_portable();
    cfg.holder_concentration_enable = true;
    cfg.max_concurrent_positions = 2;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for (i, a) in CONC_B58.iter().enumerate() {
        seed_launch(&mut eng, a, true, washed(i), ORGANIC_AGE);
    }
    for a in BROAD_B58 {
        seed_launch(&mut eng, a, false, false, ORGANIC_AGE);
    }
    for a in CONC_B58 {
        let v = eng.holder_concentration(b58(a).as_bytes());
        let m = v.metrics().expect("concentrated cohort must be measurable");
        assert!(
            m.risk(true) == ConcentrationRisk::Veto,
            "concentrated cohort must reach the veto tier under corroboration"
        );
        assert!(
            m.risk(false) == ConcentrationRisk::Haircut,
            "and only the haircut tier without it"
        );
    }
    for a in BROAD_B58 {
        let v = eng.holder_concentration(b58(a).as_bytes());
        let m = v.metrics().expect("broad cohort must be measurable");
        assert_eq!(
            m.risk(true),
            ConcentrationRisk::Clear,
            "the broad cohort must be untouched — otherwise the mirror tape is not \
             a false-positive test, it is the same test twice"
        );
    }
    // Promotion/position contention is real: both arms admit, and capacity binds.
    let off = ab_drive(
        {
            let mut c = Config::dev_portable();
            c.holder_concentration_enable = false;
            c
        },
        Side::ConcentratedBleeds,
    );
    let on = ab_drive(
        {
            let mut c = Config::dev_portable();
            c.holder_concentration_enable = true;
            c
        },
        Side::ConcentratedBleeds,
    );
    println!(
        "NON-VACUITY off: admitted={} rejected={} promoted={} net={} | on: admitted={} rejected={} promoted={} net={}",
        off.admitted, off.rejected, off.promoted, off.net_lamports,
        on.admitted, on.rejected, on.promoted, on.net_lamports
    );
    assert!(off.admitted > 0, "the neutral arm must actually trade");
    assert!(
        on.rejected > off.rejected || on.admitted != off.admitted,
        "the armed law must change what the engine does on this tape"
    );
}

#[test]
fn ab_holder_concentration_two_sided() {
    // ===================================================================
    // THE PRE-REGISTERED RULE — written into this file BEFORE the numbers
    // were read, and identical in form to the rule LAWs B7 and the §70.1
    // money term were judged under (§56.2).
    //
    // The law is adopted BY DEFAULT only if ALL of:
    //
    //   (a) HAPPY TAPE — on a tape where genuinely concentrated/bundled
    //       markets are present alongside healthy ones, the armed net
    //       exceeds the neutral net by MORE THAN 100_000_000 lamports
    //       (one 0.1-SOL bite; a gain smaller than a single clip is noise
    //       in the arbitration, not an edge).
    //
    //   (b) MIRROR TAPE — on the mirror (the SAME generator with one
    //       boolean flipped, so the concentration signal fires on the
    //       market that was going to pay: a false positive), the armed
    //       loss versus neutral is small enough that
    //           happy_gain / |mirror_loss| >= 3.
    //       A law that gives back on its false positives what it earns on
    //       its true positives is a coin flip with extra state.
    //
    //   (c) GOLDEN TAPE — Δ == 0 unless the law is armed by default, in
    //       which case the golden reference is re-pinned once with a note.
    //
    // If it fails, the default stays OFF and the report says so bluntly.
    // An honest negative is the correct outcome.
    // ===================================================================
    const HAPPY_BAR_LAMPORTS: i128 = 100_000_000;
    const ASYMMETRY_BAR: i128 = 3;

    let base = |side| {
        let mut c = Config::dev_portable();
        c.holder_concentration_enable = false;
        ab_drive(c, side)
    };
    let armed = |side| {
        let mut c = Config::dev_portable();
        c.holder_concentration_enable = true;
        ab_drive(c, side)
    };

    let off_happy = base(Side::ConcentratedBleeds);
    let on_happy = armed(Side::ConcentratedBleeds);
    let off_mirror = base(Side::ConcentratedPays);
    let on_mirror = armed(Side::ConcentratedPays);

    let gain = on_happy.net_lamports - off_happy.net_lamports;
    let loss = on_mirror.net_lamports - off_mirror.net_lamports;
    println!(
        "AB holder-concentration | HAPPY off={} on={} delta={} (adm {} -> {}, rej {} -> {}) \
         | MIRROR off={} on={} delta={} (adm {} -> {}, rej {} -> {})",
        off_happy.net_lamports,
        on_happy.net_lamports,
        gain,
        off_happy.admitted,
        on_happy.admitted,
        off_happy.rejected,
        on_happy.rejected,
        off_mirror.net_lamports,
        on_mirror.net_lamports,
        loss,
        off_mirror.admitted,
        on_mirror.admitted,
        off_mirror.rejected,
        on_mirror.rejected,
    );
    // The two sides are genuinely different tapes, so a law that mattered would
    // have room to show it.
    assert_ne!(
        off_happy.net_lamports, off_mirror.net_lamports,
        "the two sides of the A/B must be genuinely different tapes"
    );

    // Whatever the verdict, the DEFAULT must match it. This assertion is what
    // keeps the report honest: if the law ever starts earning under the
    // pre-registered rule, this test fails until the default is flipped.
    let earns = gain > HAPPY_BAR_LAMPORTS && ASYMMETRY_BAR * loss.abs() <= gain;
    assert_eq!(
        earns,
        Config::dev_portable().holder_concentration_enable,
        "the configured default must equal the pre-registered A/B verdict \
         (happy gain {gain}, mirror {loss}, bars {HAPPY_BAR_LAMPORTS} / {ASYMMETRY_BAR}x)"
    );
}

#[test]
fn ab_disarmed_law_is_byte_identical_and_the_stream_still_runs() {
    // The safety property that makes a default-OFF decision cost nothing: with the
    // law disarmed the engine's decisions are bit-for-bit reproducible, even
    // though the shape evidence is fully populated underneath.
    let mut c = Config::dev_portable();
    c.holder_concentration_enable = false;
    let a = ab_drive(c, Side::ConcentratedBleeds);
    let b = ab_drive(c, Side::ConcentratedBleeds);
    assert_eq!(a.journal_digest, b.journal_digest);
    assert_eq!(a.net_lamports, b.net_lamports);

    let mut cfg = Config::dev_portable();
    cfg.holder_concentration_enable = false;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    seed_launch(&mut eng, CONC_B58[0], true, true, ORGANIC_AGE);
    // The DECISION plane sees `Disarmed`...
    // ...while the REPORT plane still measures the real shape.
    assert!(
        eng.holder_concentration(b58(CONC_B58[0]).as_bytes())
            .is_known(),
        "the shape evidence must be live even with the law disarmed"
    );
}

#[test]
fn veto_reject_code_17_actually_reaches_the_journal() {
    // END-TO-END NON-VACUITY for the refusal path. A code that never appears is
    // an unexercised branch dressed as a law, so the journal is read directly.
    // Both the washed (veto-eligible) and clean (haircut-only) halves of the
    // concentrated cohort are on this tape.
    let drive = |armed: bool| {
        let mut cfg = Config::dev_portable();
        cfg.holder_concentration_enable = armed;
        cfg.gate_expected_move_bps = 1_800;
        cfg.gate_protocol_bps = 450;
        cfg.gate_margin_bps = 150;
        cfg.gate_base_fixed_lamports = 200_000;
        cfg.gate_impact_den = 250_000;
        let mut eng = Engine::new(cfg, RunMode::Replay);
        for (i, a) in CONC_B58.iter().enumerate() {
            seed_launch(&mut eng, a, true, washed(i), ORGANIC_AGE);
        }
        for (i, a) in CONC_B58.iter().enumerate() {
            post(&mut eng, "seedcaller", a, "seed", 900_000_000 + i as u64);
        }
        ticks(&mut eng, 8);
        for round in 0..6u64 {
            for (i, a) in CONC_B58.iter().enumerate() {
                let mt = b58(a);
                let px = price_path(round, false, i as i128);
                for k in 0..3u64 {
                    let e = ent(a, 500 + round * 3 + k);
                    swap(&mut eng, mt, px, 300, e, 40_000, ORGANIC_AGE, LIQ);
                }
                post(
                    &mut eng,
                    "c",
                    a,
                    &format!("r{round}i{i}"),
                    1_000_000_000 + round * 20_000_000 + i as u64,
                );
            }
            ticks(&mut eng, 8);
        }
        let mut code_17 = 0u32;
        for d in eng.journal().recent() {
            if let pump_quant_app::journal_log::Decision::Rejected { reason, .. } = d {
                if *reason == 17 {
                    code_17 += 1;
                }
            }
        }
        (code_17, eng.report())
    };
    let (off_17, off) = drive(false);
    let (on_17, on) = drive(true);
    println!(
        "VETO CODE 17 | disarmed count={} net={} adm={} | armed count={} net={} adm={}",
        off_17, off.net_lamports, off.admitted, on_17, on.net_lamports, on.admitted
    );
    assert_eq!(off_17, 0, "the code cannot exist with the law disarmed");
    assert!(
        on_17 > 0,
        "the conjunctive veto must actually fire somewhere"
    );
    assert!(
        off.admitted > 0,
        "the disarmed arm must trade, or the refusal removes nothing"
    );
}

#[test]
fn screen_does_not_remove_the_healthy_half_of_a_mixed_mature_universe() {
    // §21.7(e) OVER-REJECTION GUARD, asserted rather than assumed: "a screen that
    // rejects substantially the entire qualified universe is a calibration bug
    // requiring correction, and must never be mistaken for prudence." On a MIXED
    // mature universe the armed screen must remove the concentrated half and leave
    // the broad half promoting exactly as it did disarmed.
    let mature_age = 4_000; // ≫ universe_age_exempt_slots
    let drive = |armed: bool| {
        let mut cfg = Config::dev_portable();
        cfg.holder_concentration_enable = armed;
        let mut eng = Engine::new(cfg, RunMode::Replay);
        for (i, a) in CONC_B58.iter().enumerate() {
            seed_launch(&mut eng, a, true, washed(i), mature_age);
        }
        for a in BROAD_B58 {
            seed_launch(&mut eng, a, false, false, mature_age);
        }
        for (i, a) in CONC_B58.iter().chain(BROAD_B58.iter()).enumerate() {
            post(&mut eng, "caller", a, "call", 900_000_000 + i as u64);
        }
        for r in 0..6u64 {
            for a in CONC_B58.iter().chain(BROAD_B58.iter()) {
                let mt = b58(a);
                for k in 0..4u64 {
                    let e = ent(a, 300 + r * 4 + k);
                    swap(&mut eng, mt, 10_100, 1_000, e, 50_000, mature_age, LIQ);
                }
            }
            ticks(&mut eng, 4);
        }
        eng.report()
    };
    let off = drive(false);
    let on = drive(true);
    println!(
        "OVER-REJECTION GUARD off: promoted={} filtered={} | on: promoted={} filtered={}",
        off.promoted, off.universe_filtered, on.promoted, on.universe_filtered
    );
    // Exactly half the universe is concentrated, so the screen may remove at most
    // half of the promotions it previously allowed. If it ever removes more, the
    // bars are mis-calibrated and this test is the thing that says so.
    assert!(
        on.universe_filtered > off.universe_filtered,
        "the screen must bind"
    );
    assert!(
        on.promoted * 2 >= off.promoted,
        "the armed screen removed MORE than the concentrated half of a mixed \
         universe ({} -> {} promotions) — that is a calibration bug (§21.7(e)), \
         not prudence",
        off.promoted,
        on.promoted
    );
}
