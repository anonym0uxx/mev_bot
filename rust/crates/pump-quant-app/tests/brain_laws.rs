//! LAWs B1–B5 — the episodic recall memory wiring, proven law by law.
//!
//! Mirrors the `alpha_laws.rs` / `audit_wave2_laws.rs` discipline exactly: isolate
//! the law with a config toggle, drive the SAME deterministic event tape twice
//! (armed vs neutralized), and assert the armed arm wins in the law's own axis.
//! Determinism (§22) makes every comparison exact rather than statistical.
//!
//! * **B1** — an episode is sealed per completed trade, and its fingerprint is a
//!   function of ENTRY-time state only. Pinned by mutating the whole post-entry
//!   price path and asserting the recorded fingerprint is byte-identical: a
//!   fingerprint that moved would be reading the answer off the back of the card.
//! * **B2** — the reflection cadence produces grounded readouts (recalled setup
//!   classes with their realized medians, the meta lifecycle, measured author
//!   track records) instead of blind hypotheses.
//! * **B3** — on a tape where one setup class repeatedly bleeds, the armed
//!   reduce-only haircut/veto STRICTLY out-earns its absence.
//! * **B4** — an `Unknown` verdict changes nothing: with an insufficient brain the
//!   armed and disarmed runs produce a byte-identical `Report`, and a
//!   brain-disabled run produces a byte-identical DECISION stream.
//! * **B5** — recall verdicts are byte-identical after persist → "restart" →
//!   restore.

use pump_quant_app::brain::{AppBlobStore, BRAIN_MIN_SAMPLE_DEFAULT};
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_brain::fingerprint::SetupFingerprint;
use pump_quant_brain::persist::MemBlobStore;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

const PRICE_SCALE: i128 = 10_000_000;

/// Deterministic mint tag → 32-byte id, distinct from every other test's cohort.
fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xB1;
    Mint::from_bytes(b)
}

fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

/// Liquidity of the BLEEDING setup class: a thin ~4-SOL pool (signed decade 9).
const THIN_LIQUIDITY: u64 = 4_000_000_000;
/// Liquidity of the HEALTHY setup class: a ~400-SOL pool (signed decade 11), two
/// ladder buckets away, so the two classes never share a `liquidity_decade`.
const DEEP_LIQUIDITY: u64 = 400_000_000_000;
/// Market age of the BLEEDING class: seconds old (`token_age` bucket 0).
const FRESH_AGE_SLOTS: u32 = 12;
/// Market age of the HEALTHY class: ~33 minutes of information time (bucket 3).
const MATURE_AGE_SLOTS: u32 = 5_000;

/// One trade for `m`, fully parameterized so the two hazard classes can be made
/// to quantize far apart while sharing a discovery lane.
#[allow(clippy::too_many_arguments)]
fn one_at(
    eng: &mut Engine,
    m: Mint,
    price_mult: i128,
    signed_base: i64,
    entity: u64,
    liquidity_lamports: u64,
    age_slots: u32,
) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: price_mult * PRICE_SCALE,
        quote_lamports: 800_000,
        liquidity_lamports,
        signed_base,
        buyer_entity: entity,
        age_slots,
    });
}

fn one(eng: &mut Engine, m: Mint, price_mult: i128, signed_base: i64, entity: u64) {
    one_at(
        eng,
        m,
        price_mult,
        signed_base,
        entity,
        THIN_LIQUIDITY,
        FRESH_AGE_SLOTS,
    );
}

fn narrate(eng: &mut Engine, m: Mint) {
    eng.tick(AppEvent::NarrativeSample {
        mint: m,
        prior_active: 5,
        new_mentions: 9_000,
    });
}

/// Discover `m` through the narrative lane, prove on-chain depth, and admit it —
/// the BLEEDING class's opening script: a fresh, thin, narrow-breadth, flat market
/// on net-SELL flow. Every cohort member runs it verbatim, so every admit quantizes
/// to the SAME setup class; that identity is what makes the class RECUR, which is
/// what LAW B3 needs in order to have anything to recall.
fn seed_and_admit(eng: &mut Engine, m: Mint, entity_base: u64) {
    for _ in 0..4 {
        narrate(eng, m);
    }
    for i in 0..10u64 {
        // Only 3 distinct buyers ⇒ the narrow end of the breadth ladder.
        one(eng, m, 100, -500_000, entity_base + i % 3);
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: m,
        sellable_depth_lamports: 2_000_000_000,
    });
    narrate(eng, m);
    ticks(eng, 2);
}

/// The cohort's shared FATE: a −40% single-swap collapse (rug precursor) that books
/// the loss and closes the position.
fn crater(eng: &mut Engine, m: Mint, entity: u64) {
    one(eng, m, 60, -1_500_000, entity);
    ticks(eng, 4);
    // Let the position close and the discovery evidence go stale before the next
    // cohort member arrives, so each round is a clean sequential round trip.
    ticks(eng, 20);
}

/// A config with the operator floor relaxed enough that a small hazard tape can
/// open many sequential positions off a 2-SOL bankroll, and with the discovery
/// evidence going stale fast so a freed slot does not silently re-admit an old
/// cohort member (which would confound the A/B axis).
fn hazard_cfg() -> Config {
    let mut cfg = Config::dev_portable();
    cfg.watchlist_ttl_ticks = 8;
    cfg.lane_evidence_ttl_ticks = 8;
    // 0.02 SOL clips: the hazard tape needs ~14 sequential round trips, which a
    // 0.1-SOL floor on a 2-SOL bankroll cannot fund once the losses start. The
    // floor is an operator policy knob (§102), not a law under test here.
    cfg.min_trade_size_lamports = 20_000_000;
    cfg.f_base_bp = 400;
    cfg.total_risk_cap_bp = 4_000;
    cfg.max_concurrent_positions = 8;
    cfg.x_min_promote_cap_bp = 500;
    // A cohort member that has already cratered must not squat a slot for the rest
    // of the tape: with only 3 concurrent slots a zombie position would starve the
    // recurrence the A/B is measuring (every later admit would be a MAX_CONCURRENT
    // reject in BOTH arms, making the comparison vacuous). Tight stall/time-stop
    // windows retire it. Both arms share these values, so the axis stays clean.
    cfg.lc_stall_ticks = 5;
    cfg.lc_max_hold_ticks = 18;
    // Tighten the similarity radius from the brain's default 12 to 6. The two
    // classes on the hazard tape sit 8 ordinal buckets apart (opposite ends of the
    // OFI ladder plus two liquidity decades), which the default radius would POOL —
    // and a pooled estimate over a profitable class and a bleeding one is exactly
    // the §100-style error the whole design exists to avoid. 6 keeps them separate
    // while still matching within-class variation (which is 1 bucket here).
    // Operator knob (§102); identical in both arms, so the axis stays clean.
    cfg.brain_recall_max_distance = 3;
    // Neutralize the §23 arbitration's minimum-expected-net floor in BOTH arms.
    // That floor is driven by the §24 EXPECTANCY_V1 estimator, which pools every
    // setup a LANE produces into one mean — a competing, lane-conditioned law. On
    // this tape it would refuse the whole lane once the bleeding class dragged the
    // pooled mean down, masking the axis under test and making the A/B vacuous.
    // Isolating one law by neutralizing its competitor IN BOTH ARMS is the same
    // discipline the §29.5 exit-pressure A/B uses; the measured delta is then
    // attributable to LAW B3 and nothing else.
    cfg.arb_min_expected_net_lamports = i64::MIN / 4;
    cfg
}

/// Total realized net across journalled exits.
fn journal_stream(eng: &Engine) -> Vec<Decision> {
    eng.journal().recent().copied().collect()
}

/// Every fingerprint the brain has sealed, oldest first.
fn recorded_fingerprints(eng: &Engine) -> Vec<SetupFingerprint> {
    eng.brain()
        .index()
        .iter_oldest_first()
        .map(|e| *e.fingerprint())
        .collect()
}

// ===========================================================================
// LAW B1 — one episode per completed trade, fingerprinted AT ENTRY.
// ===========================================================================

/// The shared B1 tape: open one position, then let the post-entry path be dictated
/// by `path`. The ENTRY script is byte-identical in both arms.
fn drive_one_trade(cfg: Config, path: &[i128]) -> Engine {
    let m = mint(9_001);
    let mut eng = Engine::new(cfg, RunMode::Replay);
    seed_and_admit(&mut eng, m, 300);
    for (i, &px) in path.iter().enumerate() {
        one(&mut eng, m, px, -600_000, 380 + (i as u64 % 5));
        ticks(&mut eng, 1);
    }
    ticks(&mut eng, 6);
    let _ = eng.report();
    eng
}

#[test]
fn b1_seals_one_episode_per_completed_trade() {
    let mut eng = Engine::new(hazard_cfg(), RunMode::Replay);
    for k in 0..3u64 {
        let m = mint(100 + k);
        seed_and_admit(&mut eng, m, 400 + k * 10);
        crater(&mut eng, m, 460 + k);
    }
    let r = eng.report();
    assert!(r.admitted > 0, "the tape must actually admit ({r:?})");
    assert_eq!(
        r.brain_episodes_recorded, r.admitted,
        "LAW B1: exactly one sealed episode per completed trade \
         (admitted {}, episodes {})",
        r.admitted, r.brain_episodes_recorded
    );
    assert_eq!(
        eng.brain().index().len() as u64,
        r.brain_episodes_recorded,
        "every sealed episode is live in the bounded index"
    );
    // Every episode carries a realized outcome and the admitted flag (§46: only
    // actually-traded setups may contribute to a recall estimate).
    for e in eng.brain().index().iter_oldest_first() {
        assert!(
            e.outcome().was_admitted,
            "a sealed episode is an ADMITTED trade"
        );
    }
}

#[test]
fn b1_fingerprint_has_no_look_ahead() {
    // Two runs, byte-identical up to and including the admit; then WILDLY divergent
    // post-entry price paths — one craters, one rips to 3×. If the recorded
    // fingerprint depended on ANY post-entry information, these two would differ.
    let crash = drive_one_trade(hazard_cfg(), &[95, 80, 66, 55, 48, 40]);
    let rip = drive_one_trade(hazard_cfg(), &[120, 160, 210, 260, 300, 340]);

    let fp_crash = recorded_fingerprints(&crash);
    let fp_rip = recorded_fingerprints(&rip);
    assert!(
        !fp_crash.is_empty(),
        "the B1 tape must seal at least one episode"
    );
    // Compare the FIRST sealed episode: its entry script is byte-identical in both
    // arms by construction. Later episodes on this tape are genuine RE-ADMITS at
    // later ticks, off state the divergent paths legitimately produced — comparing
    // those would test nothing about look-ahead.
    assert_eq!(
        fp_crash[0], fp_rip[0],
        "LAW B1: the recorded fingerprint is a function of ENTRY-time state ONLY — \
         mutating the entire post-entry price path must not move a single bucket"
    );
    assert_eq!(
        fp_crash[0].signature(),
        fp_rip[0].signature(),
        "and the packed signature recall actually matches on is identical too"
    );
    // Sanity: the two runs really did diverge — otherwise the assertion above would
    // be vacuously true and would prove nothing.
    let net_crash: i128 = crash
        .journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Filled {
                net_pnl_lamports, ..
            } => Some(net_pnl_lamports),
            _ => None,
        })
        .sum();
    let net_rip: i128 = rip
        .journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Filled {
                net_pnl_lamports, ..
            } => Some(net_pnl_lamports),
            _ => None,
        })
        .sum();
    assert_ne!(
        net_crash, net_rip,
        "the two post-entry paths must genuinely produce different outcomes, \
         otherwise the no-look-ahead assertion is vacuous"
    );
    assert!(
        net_crash < net_rip,
        "the cratering path must realize less than the ripping one \
         ({net_crash} vs {net_rip})"
    );
    // And the OUTCOMES recorded against those identical fingerprints DO differ —
    // which is the whole point: same setup, different result, honest memory.
    let out_crash: Vec<i128> = crash
        .brain()
        .index()
        .iter_oldest_first()
        .map(|e| e.outcome().realized_net_lamports)
        .collect();
    let out_rip: Vec<i128> = rip
        .brain()
        .index()
        .iter_oldest_first()
        .map(|e| e.outcome().realized_net_lamports)
        .collect();
    assert_ne!(
        out_crash, out_rip,
        "identical fingerprints, different realized outcomes — the memory records \
         the world, not the prediction"
    );
}

// ===========================================================================
// LAW B2 — grounded reflection readouts.
// ===========================================================================

/// Feed one social call naming `addr` from `author`, through the real capture seam.
fn social_call(eng: &mut Engine, author: &str, addr: &str, ts_ns: u64) {
    let json = format!(
        "{{\"platform\":\"telegram\",\"author\":\"{author}\",\"community\":\"tg-b2\",\
         \"text\":\"call {addr} send it\",\"likes\":42,\"is_designated_caller\":true}}"
    );
    let mut src =
        MockSocialSource::new().with_batch(vec![RawSocialPayload::new(json.into_bytes(), ts_ns)]);
    eng.ingest_social(&mut src);
}

/// Base58 cohort keys for the B2 social tape (valid pubkeys, distinct from every
/// `mint(tag)` which are `tag_le ++ 0xB1 ++ 0…`).
const B2_KEYS: [&str; 10] = [
    "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr",
    "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
    "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R",
    "So11111111111111111111111111111111111111112",
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
    "mSoLzYCxHdYgdzU16g5QSh3i5K3z3KZK7ytfqcJm7So",
    "7dHbWXmci3dT8UFYWYZweBLXgycu7Y3iL6trKn1Y7ARj",
    "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
    "9n4nbM75f5Ui33ZbPYXn59EwSgE8CGsHtAeTH5YFeJ9E",
];

fn b58(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid pubkey"))
}

#[test]
fn b2_reflection_exposes_grounded_recall_and_measured_author_records() {
    // One author calls a cohort of mints; each is admitted and each craters. The
    // reflection cadence must then be able to say (a) which setup classes were
    // traded and what they paid, and (b) that this author's calls lose money —
    // neither of which is a stub: both are computed off realized markouts.
    let mut cfg = hazard_cfg();
    cfg.reflect_every_ticks = 20;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let mut ts = 1_000_000_000u64;
    for (k, key) in B2_KEYS.iter().enumerate() {
        let m = b58(key);
        social_call(&mut eng, "bleeding-caller", key, ts);
        ts += 60_000_000_000;
        seed_and_admit(&mut eng, m, 700 + (k as u64) * 10);
        crater(&mut eng, m, 790 + k as u64);
    }
    let r = eng.report();
    assert!(
        r.brain_episodes_recorded >= BRAIN_MIN_SAMPLE_DEFAULT as u64,
        "the B2 tape must seal at least the §46 sample floor of episodes (got {})",
        r.brain_episodes_recorded
    );
    // (a) The reflection readout names the setup classes the engine actually traded,
    // with their realized distribution — grounded, not hypothesised.
    assert!(
        !r.brain_setup_classes.is_empty(),
        "LAW B2: reflection must surface at least one recalled traded setup class"
    );
    let strongest = r.brain_setup_classes[0];
    assert!(
        strongest.n_matched >= BRAIN_MIN_SAMPLE_DEFAULT,
        "a surfaced class cleared the §46 sample floor (n={})",
        strongest.n_matched
    );
    assert!(
        strongest.median_net_lamports < 0,
        "the cohort bled, so the recalled median must be negative ({})",
        strongest.median_net_lamports
    );
    assert!(
        strongest.nearest_episode_id > 0,
        "the readout carries an audit anchor into the actual nearest episode"
    );
    // (b) The author's track record is MEASURED off attributed markouts, and it is
    // fail-closed: it only appears because the sample floor was cleared.
    assert!(
        !r.brain_author_records.is_empty(),
        "LAW B2: 'who called this, and do they earn' must be answerable \
         (records: {:?})",
        r.brain_author_records
    );
    let rec = r.brain_author_records[0];
    assert!(
        rec.n_markouts >= BRAIN_MIN_SAMPLE_DEFAULT,
        "the record cleared the §46 sample floor (n={})",
        rec.n_markouts
    );
    assert!(
        rec.median_net_lamports < 0,
        "this caller's calls lost money — the record must say so ({})",
        rec.median_net_lamports
    );
    assert_eq!(
        rec.win_rate_bp, 0,
        "every attributed markout was a loss, so the decisive win rate is 0 bp"
    );
    // The recall counters are populated: the engine asked the memory at every admit.
    assert!(
        r.brain_recall_known + r.brain_recall_unknown > 0,
        "the engine must have consulted recall at admit time"
    );
}

#[test]
fn b2_meta_lifecycle_is_recorded_when_categories_exist() {
    // Feeding TokenMetadata gives mints a category, which the reflection cadence
    // snapshots onto the brain's meta timeline — the "state of the meta" readout.
    let mut cfg = hazard_cfg();
    cfg.reflect_every_ticks = 10;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for k in 0..6u64 {
        let m = mint(2_000 + k);
        eng.tick(AppEvent::TokenMetadata {
            mint: m,
            category_id: 7,
            // v1 is the shipped taxonomy version (see `META_TAXONOMY_VERSION_DEFAULT`);
            // an assignment stamped with any other version is left UNKNOWN, never
            // retroactively remapped (criterion 81).
            taxonomy_version: 1,
            creator: 5_000 + k,
            slot: 10 + k,
        });
        seed_and_admit(&mut eng, m, 900 + k * 10);
        crater(&mut eng, m, 980 + k);
    }
    let r = eng.report();
    assert!(
        !r.brain_meta_state.is_empty(),
        "LAW B2: with categories fed, the meta lifecycle timeline must be populated"
    );
    assert!(
        eng.brain().meta_timeline().len() >= r.brain_meta_state.len(),
        "the report shows a bounded head of the full timeline"
    );
}

// ===========================================================================
// LAW B3 — the reduce-only recall haircut/veto, A/B on a bleeding-class tape.
// ===========================================================================

// ---------------------------------------------------------------------------
// The B3 hazard tape: TWO setup classes inside ONE discovery lane.
//
// This shape is the whole quant argument for episodic recall over the §24
// per-lane conditional expectancy the engine already runs. EXPECTANCY_V1 pools
// every setup a lane produces into one mean; on a tape where the lane's OTHER
// setups pay well, a lane-pooled estimator stays happily positive while one
// specific SETUP CLASS bleeds on every single occurrence. Recall conditions on the
// setup, so it can see what the pooled estimator structurally cannot.
//
//   * class HEALTHY — deep (~40 SOL) pool, net-BUY flow, price rips to 2.5x.
//   * class BLEEDER — thin (~0.4 SOL) pool, net-SELL flow, price craters 40%.
//
// The two quantize far apart (opposite ends of the OFI and CVD ladders, two
// liquidity decades apart), so recall never pools them — while the WATCHLIST lane
// and the §71.2 discovery lane are identical for both, so the lane-pooled
// expectancy sees one blended, profitable stream.
//
// Rounds alternate HEALTHY, BLEEDER, HEALTHY, BLEEDER…. Until the bleeding class
// clears the §46 sample floor both arms are byte-identical; after it does, the
// armed arm must refuse every further BLEEDER while still taking every HEALTHY.
// ---------------------------------------------------------------------------

/// Seed + admit one HEALTHY-class member.
///
/// SAME discovery lane and SAME net-SELL flow sign as the bleeding class — that is
/// deliberate and load-bearing: if the two classes fell in different watchlist
/// lanes, the engine's existing §24 per-lane conditional expectancy would already
/// separate them and LAW B3 would be measuring nothing. They are separated ONLY on
/// axes the lane-pooled estimator cannot see: pool depth (2 liquidity decades),
/// market age (fresh vs ~33 min), buyer breadth (3 vs 24 distinct entities) and
/// realized volatility (flat vs choppy pre-entry).
fn seed_healthy(eng: &mut Engine, m: Mint, entity_base: u64) {
    for _ in 0..4 {
        narrate(eng, m);
    }
    for i in 0..12u64 {
        // Choppy pre-entry tape (realized-vol ladder) and broad participation.
        let px = 100 + i128::from(i as i64 % 4) * 3;
        one_at(
            eng,
            m,
            px,
            -500_000,
            entity_base + i * 2,
            DEEP_LIQUIDITY,
            MATURE_AGE_SLOTS,
        );
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: m,
        sellable_depth_lamports: 200_000_000_000,
    });
    narrate(eng, m);
    ticks(eng, 2);
}

/// The HEALTHY class's fate: a clean rip that banks the take-profit ladder.
fn rip(eng: &mut Engine, m: Mint, entity: u64) {
    for px in [130i128, 170, 210, 250] {
        one_at(
            eng,
            m,
            px,
            -500_000,
            entity,
            DEEP_LIQUIDITY,
            MATURE_AGE_SLOTS,
        );
        ticks(eng, 1);
    }
    ticks(eng, 24);
}

fn drive_two_class_hazard(cfg: Config, rounds: u64) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for k in 0..rounds {
        let healthy = mint(5_000 + k);
        seed_healthy(&mut eng, healthy, 1_000 + k * 20);
        rip(&mut eng, healthy, 1_015 + k * 20);
        let bleeder = mint(6_000 + k);
        seed_and_admit(&mut eng, bleeder, 40_000 + k * 20);
        crater(&mut eng, bleeder, 40_015 + k * 20);
    }
    let r = eng.report();
    (r, eng)
}

/// Journalled rejections carrying the LAW B3 reject code.
const REJECT_BRAIN_BLED: u8 = 16;

fn brain_bled_rejects(eng: &Engine) -> usize {
    eng.journal()
        .recent()
        .filter(|d| matches!(**d, Decision::Rejected { reason, .. } if reason == REJECT_BRAIN_BLED))
        .count()
}

/// Realized net attributed to the mints of one cohort (`tag_base..tag_base+n`).
fn cohort_net(eng: &Engine, tag_base: u64, n: u64) -> i128 {
    let tags: Vec<[u8; 32]> = (0..n).map(|k| *mint(tag_base + k).as_bytes()).collect();
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Filled {
                mint: mb,
                net_pnl_lamports,
                ..
            } if tags.contains(&mb) => Some(net_pnl_lamports),
            _ => None,
        })
        .sum()
}

#[test]
fn b3_armed_recall_haircut_strictly_out_earns_its_absence() {
    const ROUNDS: u64 = 40;
    let mut acfg = hazard_cfg();
    acfg.brain_haircut_enable = true; // B3 armed
    let (armed, aeng) = drive_two_class_hazard(acfg, ROUNDS);

    let mut ncfg = hazard_cfg();
    ncfg.brain_haircut_enable = false; // B3 neutralized (recall still computed)
    let (neut, neng) = drive_two_class_hazard(ncfg, ROUNDS);

    eprintln!(
        "B3 armed:   admitted={} rejected={} net={} episodes={} known={} unknown={} \
         haircuts={} vetoes={} bled_rejects={} healthy_net={} bleeder_net={}",
        armed.admitted,
        armed.rejected,
        armed.net_lamports,
        armed.brain_episodes_recorded,
        armed.brain_recall_known,
        armed.brain_recall_unknown,
        armed.brain_haircuts_applied,
        armed.brain_vetoes,
        brain_bled_rejects(&aeng),
        cohort_net(&aeng, 5_000, ROUNDS),
        cohort_net(&aeng, 6_000, ROUNDS),
    );
    eprintln!(
        "B3 neutral: admitted={} rejected={} net={} episodes={} known={} unknown={} \
         haircuts={} vetoes={} bled_rejects={} healthy_net={} bleeder_net={}",
        neut.admitted,
        neut.rejected,
        neut.net_lamports,
        neut.brain_episodes_recorded,
        neut.brain_recall_known,
        neut.brain_recall_unknown,
        neut.brain_haircuts_applied,
        neut.brain_vetoes,
        brain_bled_rejects(&neng),
        cohort_net(&neng, 5_000, ROUNDS),
        cohort_net(&neng, 6_000, ROUNDS),
    );
    eprintln!(
        "B3 LAMPORT DELTA (armed - neutral) = {}",
        armed.net_lamports - neut.net_lamports
    );
    // Class-level audit: the two setup classes recall separates, and what each paid.
    {
        let mut seen: Vec<([u8; 20], i128, usize)> = Vec::new();
        for e in neng.brain().index().iter_oldest_first() {
            let b = *e.fingerprint().buckets();
            match seen.iter_mut().find(|(x, _, _)| *x == b) {
                Some(row) => {
                    row.1 += e.outcome().realized_net_lamports;
                    row.2 += 1;
                }
                None => seen.push((b, e.outcome().realized_net_lamports, 1)),
            }
        }
        for (b, net, n) in &seen {
            eprintln!("B3   class buckets={b:?} n={n} net={net}");
        }
        // The two classes must sit STRICTLY outside the similarity radius, or the
        // estimate would be a pool of a winner and a loser — the exact error §100
        // forbids across phases and the same error across setups.
        for i in 0..seen.len() {
            for j in (i + 1)..seen.len() {
                let d: u32 = (0..20)
                    .map(|k| u32::from(seen[i].0[k].abs_diff(seen[j].0[k])))
                    .sum();
                eprintln!("B3   ordinal-L1 distance class{i} vs class{j} = {d}");
                assert!(
                    d > 3,
                    "the hazard classes must sit outside the configured radius (d={d})"
                );
            }
        }
    }

    // The tape must contain the hazard: a class that recurs and bleeds every time.
    assert!(
        neut.admitted > BRAIN_MIN_SAMPLE_DEFAULT as u64,
        "the neutral arm must admit well past the §46 sample floor so the class \
         genuinely recurs after it is known to bleed (admitted {})",
        neut.admitted
    );
    assert!(
        cohort_net(&neng, 6_000, ROUNDS) < 0,
        "the bleeding cohort must actually bleed without the law ({})",
        cohort_net(&neng, 6_000, ROUNDS)
    );
    // The law fired on the bleeding class.
    assert!(
        armed.brain_vetoes + armed.brain_haircuts_applied > 0,
        "the armed arm must act on a Known bleeding class (vetoes {}, haircuts {})",
        armed.brain_vetoes,
        armed.brain_haircuts_applied
    );
    assert!(
        armed.admitted < neut.admitted,
        "the reduce-only law must refuse recurrences the neutral arm takes \
         ({} vs {})",
        armed.admitted,
        neut.admitted
    );
    // And it earns: loss avoided is lamports kept (§52 spirit).
    assert!(
        armed.net_lamports > neut.net_lamports,
        "LAW B3: the armed reduce-only recall haircut/veto must STRICTLY out-earn \
         its absence on a tape where the setup class demonstrably bleeds ({} vs {})",
        armed.net_lamports,
        neut.net_lamports
    );
}

#[test]
fn b3_is_reduce_only_and_never_enlarges_a_winning_class() {
    // A class that WINS every time. Armed, LAW B3 must be a complete no-op: no
    // haircut, no veto, and an identical decision stream against the disarmed arm.
    // Historical-winner sizing-up is exactly where episodic recall overfits (§46);
    // the verdict type has no boost variant and this pins the behaviour end to end.
    fn drive_winners(cfg: Config) -> (Report, Engine) {
        let mut eng = Engine::new(cfg, RunMode::Replay);
        for k in 0..14u64 {
            let m = mint(7_500 + k);
            seed_healthy(&mut eng, m, 2_000 + k * 20);
            rip(&mut eng, m, 2_015 + k * 20);
        }
        let r = eng.report();
        (r, eng)
    }
    let mut acfg = hazard_cfg();
    acfg.brain_haircut_enable = true;
    let mut ncfg = hazard_cfg();
    ncfg.brain_haircut_enable = false;
    let (armed, aeng) = drive_winners(acfg);
    let (neut, neng) = drive_winners(ncfg);
    assert!(neut.admitted > 0, "the winners tape must admit");
    assert!(
        armed.brain_recall_known > 0,
        "recall must actually reach Known on this tape, else the test is vacuous \
         (known={})",
        armed.brain_recall_known
    );
    assert_eq!(
        armed.brain_haircuts_applied, 0,
        "a winning class is never haircut"
    );
    assert_eq!(armed.brain_vetoes, 0, "a winning class is never vetoed");
    assert_eq!(
        armed.net_lamports, neut.net_lamports,
        "LAW B3 is reduce-only: over a class that historically WON, arming it must \
         be a complete no-op — never a size-up"
    );
    assert_eq!(
        journal_stream(&aeng),
        journal_stream(&neng),
        "and not a single decision differs"
    );
}

// ===========================================================================
// LAW B4 — fail-closed: an Unknown verdict changes nothing. PINNED, no toggle.
// ===========================================================================

/// A tape too short for recall to ever clear the §46 sample floor: every admit-time
/// verdict is structurally `Unknown`.
fn drive_thin_brain(cfg: Config) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for k in 0..3u64 {
        let m = mint(7_000 + k);
        seed_and_admit(&mut eng, m, 3_000 + k * 10);
        crater(&mut eng, m, 3_900 + k);
    }
    let r = eng.report();
    (r, eng)
}

#[test]
fn b4_unknown_recall_is_byte_identical_armed_or_not() {
    let mut acfg = hazard_cfg();
    acfg.brain_haircut_enable = true;
    let (armed, aeng) = drive_thin_brain(acfg);
    let mut ncfg = hazard_cfg();
    ncfg.brain_haircut_enable = false;
    let (neut, neng) = drive_thin_brain(ncfg);

    assert!(armed.admitted > 0, "the thin-brain tape must admit");
    assert_eq!(
        armed.brain_recall_known, 0,
        "the tape is deliberately below the §46 sample floor — every verdict must \
         be Unknown (known={})",
        armed.brain_recall_known
    );
    assert!(
        armed.brain_recall_unknown > 0,
        "recall must actually have been consulted and refused"
    );
    // The journal DIGEST is seeded with the whole config's §19 strategy identity,
    // so flipping ANY config field moves it by construction — that is the identity
    // law working, not a decision changing. Normalize the seed away and require the
    // rest of the Report to be byte-identical.
    let mut armed_n = armed.clone();
    let mut neut_n = neut.clone();
    armed_n.journal_digest = 0;
    neut_n.journal_digest = 0;
    assert_eq!(
        armed_n, neut_n,
        "LAW B4: an Unknown recall verdict must change NOTHING — the entire Report \
         (modulo the §19 config-identity seed) is byte-identical whether the \
         haircut law is armed or not"
    );
    assert_eq!(
        journal_stream(&aeng),
        journal_stream(&neng),
        "LAW B4: and the whole decision stream is byte-identical too"
    );
    assert_eq!(armed.brain_haircuts_applied, 0);
    assert_eq!(armed.brain_vetoes, 0);
}

#[test]
fn b4_the_brain_plane_itself_is_decision_inert() {
    // Stronger form: turning the WHOLE plane on must not move a single decision.
    // The journal DIGEST necessarily differs — §19 folds the entire config's
    // identity into the seed, so any new config field moves it — but the decision
    // STREAM that digest is computed over must be byte-identical.
    let mut on = hazard_cfg();
    on.brain_enable = true;
    let mut off = hazard_cfg();
    off.brain_enable = false;
    let (r_on, e_on) = drive_thin_brain(on);
    let (r_off, e_off) = drive_thin_brain(off);

    assert_eq!(
        journal_stream(&e_on),
        journal_stream(&e_off),
        "LAW B1/B2 are decision-inert: enabling the memory plane must not change a \
         single journalled decision"
    );
    assert_eq!(r_on.net_lamports, r_off.net_lamports, "net-SOL unchanged");
    assert_eq!(r_on.admitted, r_off.admitted, "admissions unchanged");
    assert_eq!(r_on.rejected, r_off.rejected, "rejections unchanged");
    assert_eq!(r_on.promoted, r_off.promoted, "promotions unchanged");
    assert_eq!(
        r_on.per_lane_net, r_off.per_lane_net,
        "per-lane attribution unchanged"
    );
    assert_eq!(
        r_on.final_weights, r_off.final_weights,
        "reflection weights unchanged"
    );
    // And the plane really was doing work in the ON arm (otherwise vacuous).
    assert!(r_on.brain_episodes_recorded > 0);
    assert_eq!(r_off.brain_episodes_recorded, 0);
}

// ===========================================================================
// LAW B5 — persistence: recall verdicts survive a restart.
// ===========================================================================

#[test]
fn b5_recall_verdicts_are_identical_after_persist_and_restore() {
    let mut cfg = hazard_cfg();
    cfg.brain_persist_enable = true;
    cfg.brain_path =
        pump_quant_app::config::CfgPath::from_str_checked("brain-b5").expect("path within cap");

    // ---- Session 1: arm an in-memory durable store, trade a bleeding cohort,
    // snapshot, then hand the raw blob store to a "restarted" engine.
    let mut eng = Engine::new(cfg, RunMode::Replay);
    eng.attach_brain_store(AppBlobStore::Mem(MemBlobStore::new()))
        .expect("attach");
    for k in 0..10u64 {
        let m = mint(8_000 + k);
        seed_and_admit(&mut eng, m, 4_000 + k * 10);
        crater(&mut eng, m, 4_900 + k);
    }
    let r1 = eng.report();
    assert!(
        r1.brain_episodes_recorded >= BRAIN_MIN_SAMPLE_DEFAULT as u64,
        "session 1 must seal enough episodes for recall to speak ({})",
        r1.brain_episodes_recorded
    );
    eng.snapshot_brain().expect("snapshot");
    let before: Vec<_> = eng
        .brain()
        .index()
        .iter_oldest_first()
        .map(|e| (*e.fingerprint(), *e.context(), *e.outcome(), e.episode_id()))
        .collect();
    // Capture the verdicts BEFORE detaching — `detach` disarms persistence and
    // leaves the plane with a fresh empty index (correct restart semantics).
    let params = *eng.brain().params();
    let before_verdicts: Vec<_> = before
        .iter()
        .map(|(fp, _, _, _)| eng.brain().index().recall(fp, &params))
        .collect();
    assert!(
        before_verdicts.iter().any(|v| v.is_known()),
        "session 1 must produce at least one Known verdict, else the restart proof \
         is vacuous"
    );
    let store = eng.detach_brain_store();

    // ---- Session 2: a brand-new Engine restores from the same blob store.
    let mut eng2 = Engine::new(cfg, RunMode::Replay);
    let report = eng2.attach_brain_store(store).expect("restore");
    assert!(
        report.admitted() > 0,
        "the restore must actually re-admit episodes ({report:?})"
    );
    assert!(
        !report.saw_damage(),
        "a clean snapshot+journal must restore without damage ({report:?})"
    );
    let after: Vec<_> = eng2
        .brain()
        .index()
        .iter_oldest_first()
        .map(|e| (*e.fingerprint(), *e.context(), *e.outcome(), e.episode_id()))
        .collect();
    assert_eq!(
        before, after,
        "LAW B5: the restored episodic history is byte-identical"
    );

    // And the thing that actually matters: the VERDICTS are identical. Replay the
    // same query fingerprints through the restored index.
    let after_verdicts: Vec<_> = before
        .iter()
        .map(|(fp, _, _, _)| eng2.brain().index().recall(fp, &params))
        .collect();
    assert_eq!(
        before_verdicts, after_verdicts,
        "LAW B5: recall verdicts must be byte-identical across a restart"
    );
}

#[test]
fn b5_a_fresh_store_restores_to_an_empty_fail_closed_brain() {
    // The other half of the law: restoring from nothing yields an EMPTY index whose
    // every verdict is Unknown — a restart never manufactures evidence.
    let mut cfg = hazard_cfg();
    cfg.brain_path =
        pump_quant_app::config::CfgPath::from_str_checked("brain-empty").expect("path");
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let report = eng
        .attach_brain_store(AppBlobStore::Mem(MemBlobStore::new()))
        .expect("attach");
    assert_eq!(report.admitted(), 0, "nothing to restore");
    assert!(eng.brain().index().is_empty());
}

// ===========================================================================
// Config surface: the brain's toggles, thresholds and path parse and validate.
// ===========================================================================

#[test]
fn brain_config_keys_parse_and_the_reduce_only_envelope_is_enforced() {
    use pump_quant_app::config::{CfgPath, ConfigError, BRAIN_PATH_CAP};

    // Integer toggles + thresholds parse through the ordinary key = value grammar.
    let doc = "brain_enable = 1\n\
               brain_haircut_enable = 1\n\
               brain_min_sample = 12\n\
               brain_recall_max_distance = 4\n\
               brain_haircut_win_rate_bp = 4000\n\
               brain_veto_win_rate_bp = 1000\n\
               brain_haircut_mult_bp = 6000\n\
               brain_persist_enable = 1\n\
               brain_path = data/brain\n";
    let cfg = Config::from_str_over_default(doc).expect("parse");
    assert!(cfg.brain_enable && cfg.brain_haircut_enable && cfg.brain_persist_enable);
    assert_eq!(cfg.brain_min_sample, 12);
    assert_eq!(cfg.brain_recall_max_distance, 4);
    assert_eq!(cfg.brain_haircut_win_rate_bp, 4_000);
    assert_eq!(cfg.brain_veto_win_rate_bp, 1_000);
    assert_eq!(cfg.brain_haircut_mult_bp, 6_000);
    assert_eq!(cfg.brain_path.as_str(), "data/brain");

    // LAW B3 is reduce-only: a "haircut" above 100% is refused, not clamped.
    let mut bad = Config::dev_portable();
    bad.brain_haircut_mult_bp = 10_001;
    assert_eq!(
        bad.validate(),
        Err(ConfigError::Inconsistent(
            "brain_haircut_mult_bp exceeds 100% (LAW B3 is reduce-only)"
        ))
    );
    // The veto bar must be strictly harsher evidence than the haircut bar.
    let mut inverted = Config::dev_portable();
    inverted.brain_veto_win_rate_bp = 9_000;
    inverted.brain_haircut_win_rate_bp = 3_500;
    assert_eq!(
        inverted.validate(),
        Err(ConfigError::Inconsistent(
            "brain_veto_win_rate_bp exceeds brain_haircut_win_rate_bp"
        ))
    );
    // §46: a zero sample floor would let a single episode move risk.
    let mut zero = Config::dev_portable();
    zero.brain_min_sample = 0;
    assert_eq!(
        zero.validate(),
        Err(ConfigError::Inconsistent(
            "brain_min_sample must be positive (§46 fail-closed)"
        ))
    );
    // An over-long path is REFUSED, never truncated — a truncated path is a
    // different path, and silently journaling to it would be worse than failing.
    let long = "x".repeat(BRAIN_PATH_CAP + 1);
    assert!(CfgPath::from_str_checked(&long).is_none());
    assert_eq!(
        Config::from_str_over_default(&format!("brain_path = {long}")),
        Err(ConfigError::PathTooLong("brain_path".to_string()))
    );
    // And the defaults ship the way the manifest pins them.
    let d = Config::dev_portable();
    assert!(d.brain_enable, "B1/B2 record+readout default ON");
    assert!(
        !d.brain_haircut_enable,
        "LAW B3 is exactly neutral on the golden tape, so it defaults OFF"
    );
    assert!(!d.brain_persist_enable, "LAW B5 is an operator opt-in");
    assert!(d.brain_path.is_empty());
}
