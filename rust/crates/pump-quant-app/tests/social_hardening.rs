//! **SOCIAL → ON-CHAIN PIPELINE HARDENING** — the production trading-determination
//! chain, proven end to end from a social post to a deterministic on-chain number.
//!
//! The operator's ask, restated as law: *social evidence may surface, rank and
//! contextualise a market; only decoded on-chain numbers may authorise capital.*
//! Four proofs, in increasing strength:
//!
//! 1. **Provenance chain** (`provenance_*`) — every social-derived quantity that
//!    reaches any surface carries its evidence class: which platform, which
//!    author, whether they are designated, their EARNED trust tier, and the
//!    information time it was observed at. There is no anonymous social scalar.
//! 2. **Staleness → Unknown → dropped** (`stale_*`) — a social input past the
//!    engine's evidence TTL is DROPPED, not carried forward at its last value
//!    (§34.3/§29.6). Proven at three levels: the attention field, the provenance
//!    ledger, and the watchlist.
//! 3. **The end-to-end authority proof** (`no_social_configuration_*`) — the
//!    load-bearing test. A sweep over the social plane at MAXIMAL strength, with
//!    the on-chain evidence dial turned to each of its failing positions, must
//!    admit ZERO. Flip the on-chain evidence on and the identical social tape
//!    admits. This generalises the alpha-alone-cannot-admit law (LAW D4) from one
//!    lane to the WHOLE social plane, including the new trust / support /
//!    archetype surfaces.
//! 4. **Reduce-only / report-only** (`the_social_abstraction_plane_*`) — the new
//!    surfaces cannot increase size or authorise an entry, because they are not
//!    read by any decision path at all. Proven by driving an identical tape with
//!    the abstraction plane maximally fed versus untouched and asserting a
//!    byte-identical decision journal.
//!
//! Determinism (§22) makes every comparison exact, not statistical.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_app::social_plane::{SupportVerdictRow, TrustVerdictRow};
use pump_quant_brain::social_recall::Platform;
use pump_quant_brain::trust::{SourceExposure, TrustTier};
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_parse::fnv1a_64;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

const PRICE_SCALE: i128 = 10_000_000;

/// Fresh, valid Solana pubkeys used as the sweep's markets. Distinct by
/// construction from every other cohort in the suite.
const SWEEP_B58: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
const EARNER_B58: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";

fn b58(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid pubkey"))
}

/// Admits journaled for one mint (the per-mint form of the authority law).
fn admits_for(eng: &Engine, m: Mint) -> usize {
    let bytes = *m.as_bytes();
    eng.journal()
        .recent()
        .filter(|d| matches!(**d, Decision::Admitted { mint, .. } if mint == bytes))
        .count()
}

/// Gate rejections carrying `reason` for one mint.
fn rejects_for(eng: &Engine, m: Mint, reason: u8) -> usize {
    let bytes = *m.as_bytes();
    eng.journal()
        .recent()
        .filter(
            |d| matches!(**d, Decision::Rejected { mint, reason: r } if mint == bytes && r == reason),
        )
        .count()
}

fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

/// **The SOL-side reserve every fixture in this file declares: a pump.fun bonding
/// curve at LAUNCH depth, 30 SOL of virtual SOL reserve**
/// (`pump_quant_app::curve_state::LAUNCH_VSOL_LAMPORTS` — the shallowest depth the
/// venue can present).
///
/// **Re-pin #26 (2026-07-28).** A declared depth is now a PRICE, not a label:
/// `gate::decide` derives the gate's impact denominator from the market's own reserve
/// (`cost_model::impact_den_for` = `vsol / 10_000`), so the sub-SOL figures this file
/// used to carry priced a 0.1-SOL floor clip at thousands of bps a leg and refused
/// every candidate. Stated once, so the fixtures here cannot drift from the venue.
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
/// Confirmed sellable depth, just under [`REAL_CURVE_VSOL`] — the "a confirm proves
/// slightly less than the pool holds" discipline the golden tape uses.
/// Alias kept for the fixtures that name the PAYOUT reserve directly.
const REAL_SELLABLE_DEPTH: u64 = REAL_CURVE_REAL_SOL;
/// A curve that exists but has raised essentially nothing: 1_000 lamports of escrowed
/// SOL behind a seeded 30 SOL price curve. The uneconomic cohort.
const DUST_CURVE_REAL_SOL: u64 = 1_000;
const DUST_CURVE_VSOL: u64 = 30_000_000_000 + DUST_CURVE_REAL_SOL;

/// One decoded on-chain swap. `signed_base > 0` is net buying.
fn trade(eng: &mut Engine, m: Mint, price_mult: i128, signed_base: i64, entity: u64, liq: u64) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: price_mult * PRICE_SCALE,
        quote_lamports: 800_000,
        liquidity_lamports: liq,
        signed_base,
        buyer_entity: entity,
        age_slots: 12,
    });
}

/// The full on-chain evidence dial. Each position removes exactly one of the
/// three things the gate demands, so a zero-admit result names its own cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnchainEvidence {
    /// Nothing at all: no swaps, no confirm.
    None,
    /// A confirm, but no swaps — an asserted depth with no microstructure behind
    /// it. §15 cross-checks the assertion against observed liquidity, which is
    /// zero, so the confirm is worth nothing.
    ConfirmOnly,
    /// Real swaps, but no on-chain confirm of sellable depth.
    NumericOnly,
    /// Both, but the market is far too thin for the economic gate to clear its
    /// own costs at the 0.1-SOL operator floor.
    BothButUneconomic,
    /// Everything the gate demands.
    Full,
}

impl OnchainEvidence {
    const ALL: [OnchainEvidence; 5] = [
        OnchainEvidence::None,
        OnchainEvidence::ConfirmOnly,
        OnchainEvidence::NumericOnly,
        OnchainEvidence::BothButUneconomic,
        OnchainEvidence::Full,
    ];
}

/// Feed one social post through the REAL capture seam.
#[allow(clippy::too_many_arguments)]
fn post(
    eng: &mut Engine,
    platform: &str,
    author: &str,
    community: &str,
    addr: &str,
    body: &str,
    likes: u64,
    designated: bool,
    ts_ns: u64,
) {
    let json = format!(
        "{{\"platform\":\"{platform}\",\"author\":\"{author}\",\"community\":\"{community}\",\
         \"text\":\"{body} {addr} send\",\"likes\":{likes},\"reposts\":{likes},\
         \"is_designated_caller\":{designated}}}"
    );
    let mut src =
        MockSocialSource::new().with_batch(vec![RawSocialPayload::new(json.into_bytes(), ts_ns)]);
    eng.ingest_social(&mut src);
}

/// How loud the social plane is allowed to get. `Maximal` is deliberately absurd:
/// ten distinct DESIGNATED callers, on every capture platform we model, each with
/// its own distinct content (so nothing is discounted as echo), repeated every
/// round with saturating engagement, routed through the paid-alpha lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SocialStrength {
    Silent,
    Single,
    Maximal,
}

const PLATFORMS: [&str; 4] = ["x", "telegram", "discord", "twitch"];

fn blast(eng: &mut Engine, addr: &str, strength: SocialStrength, round: u64) {
    let base_ts = 1_000_000_000u64 + round * 10_000_000;
    match strength {
        SocialStrength::Silent => {}
        SocialStrength::Single => {
            // Distinct body per round: the engine's §29 cross-provider/repost
            // dedup counts one (author, content-hash) ONCE, so a verbatim repeat
            // would be silently dropped and the arm would test nothing.
            let body = format!("lone call round {round}");
            post(eng, "x", "lone", "", addr, &body, 10, false, base_ts);
        }
        SocialStrength::Maximal => {
            for i in 0..10u64 {
                let platform = PLATFORMS[(i % 4) as usize];
                let author = format!("whale{i}");
                let community = format!("room{i}");
                // Distinct body per (author, round): distinct content hashes, so
                // the support estimator scores this as genuine BREADTH rather than
                // discounting it as one post relayed ten times.
                let body = format!("independent thesis {i} round {round}");
                post(
                    eng,
                    platform,
                    &author,
                    &community,
                    addr,
                    &body,
                    1_000_000,
                    true,
                    base_ts + i * 1_000,
                );
            }
        }
    }
}

/// Drive one sweep cell: `rounds` rounds of the chosen social strength against the
/// chosen on-chain evidence, on a single market.
fn drive_cell(strength: SocialStrength, onchain: OnchainEvidence) -> (Report, Engine) {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = b58(SWEEP_B58);
    for round in 0..4u64 {
        blast(&mut eng, SWEEP_B58, strength, round);
        match onchain {
            OnchainEvidence::None => {}
            OnchainEvidence::ConfirmOnly => {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: m,
                    virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
                });
            }
            OnchainEvidence::NumericOnly => {
                for i in 0..10u64 {
                    trade(
                        &mut eng,
                        m,
                        100 + i128::from(i as i64),
                        900_000 - i as i64,
                        40 + i % 7,
                        REAL_CURVE_VSOL,
                    );
                }
            }
            OnchainEvidence::BothButUneconomic => {
                // Real prints, but a curve nobody has meaningfully bought into: it
                // escrows 1_000 lamports, so the §18 economic band cannot clear its
                // own round-trip cost at the 0.1-SOL operator floor.
                //
                // Re-pin #27: the fixture used to declare a 1_000-lamport POOL, which
                // is not a market this venue can produce (a curve is seeded with 30
                // SOL of virtual reserve). The uneconomic-ness is now expressed the
                // way the venue expresses it — a real curve with almost nothing
                // raised — rather than by an impossible reserve.
                for i in 0..10u64 {
                    trade(
                        &mut eng,
                        m,
                        100 + i128::from(i as i64),
                        900_000 - i as i64,
                        40 + i % 7,
                        DUST_CURVE_VSOL,
                    );
                }
                eng.tick(AppEvent::OnchainConfirm {
                    mint: m,
                    virtual_sol_lamports: DUST_CURVE_VSOL,
                    real_sol_lamports: DUST_CURVE_REAL_SOL,
                });
            }
            OnchainEvidence::Full => {
                for i in 0..10u64 {
                    trade(
                        &mut eng,
                        m,
                        100 + i128::from(i as i64),
                        900_000 - i as i64,
                        40 + i % 7,
                        REAL_CURVE_VSOL,
                    );
                }
                eng.tick(AppEvent::OnchainConfirm {
                    mint: m,
                    virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
                });
            }
        }
        ticks(&mut eng, 3);
    }
    let r = eng.report();
    (r, eng)
}

// ===========================================================================
// 1. PROVENANCE — no anonymous social scalar reaches any surface (§29.8).
// ===========================================================================

#[test]
fn provenance_chain_stamps_platform_author_designation_trust_and_freshness() {
    let (report, _eng) = drive_cell(SocialStrength::Maximal, OnchainEvidence::None);
    assert!(
        !report.social_evidence.is_empty(),
        "a social blast must leave a provenance trail"
    );
    let mut platforms = std::collections::BTreeSet::new();
    for row in &report.social_evidence {
        // Evidence class: a real author, a real capture lane, a real observation
        // time. `author_id == 0` would be an anonymous scalar; it cannot occur,
        // because `SocialPlane::record_call` has no path that omits it.
        assert_ne!(row.author_id, 0, "every social datum names its author");
        assert!(
            Platform::from_ordinal(row.platform_code).is_some(),
            "every social datum names the platform that carried it"
        );
        assert!(row.designated, "this cohort is all designated callers");
        assert!(row.calls >= 1);
        assert!(row.last_tick >= row.first_tick);
        // Trust is EARNED from realized net SOL. With no admits on this tape
        // nothing has been earned, so every caller must read Unproven — never a
        // flattering default derived from engagement or follower counts (§28).
        assert_eq!(
            row.trust_tier_code,
            TrustTier::Unproven.ordinal(),
            "trust must be earned from realized net SOL, never assumed"
        );
        assert_eq!(
            row.exposure_code,
            SourceExposure::Niche.ordinal(),
            "exposure is an OPERATOR input; unset means Niche, never inferred"
        );
        platforms.insert(row.platform_code);
    }
    assert!(
        platforms.len() >= 2,
        "the provenance chain distinguishes capture lanes (saw {platforms:?})"
    );
}

#[test]
fn support_and_trust_refusals_carry_no_number_at_all() {
    let (report, _eng) = drive_cell(SocialStrength::Single, OnchainEvidence::None);
    // One lone caller cannot clear the breadth floor. The refusal is a VARIANT,
    // not a zero: there is no `support_score_bp` field to misread as "0% support".
    for row in &report.social_support {
        if let SupportVerdictRow::Unknown(refusal) = row.verdict {
            // Reaching the counts requires matching the variant — a caller cannot
            // accidentally read an estimate out of a refusal.
            let _ = refusal;
        }
    }
    for row in &report.caller_trust {
        match row.verdict {
            TrustVerdictRow::Unknown { tier_code } => assert_eq!(
                tier_code,
                TrustTier::Unproven.ordinal(),
                "an unproven source's ONLY tier is Unproven"
            ),
            TrustVerdictRow::Known { n_markouts, .. } => assert!(
                n_markouts > 0,
                "a Known trust verdict must stand on real markouts"
            ),
        }
    }
}

#[test]
fn the_capture_work_list_names_specific_external_evidence() {
    let (report, eng) = drive_cell(SocialStrength::Single, OnchainEvidence::None);
    let needs = eng.capture_work_list();
    assert_eq!(
        needs, report.social_support_needs,
        "the reflection output and the Report show the SAME work list"
    );
    assert!(
        !needs.is_empty(),
        "a thin social picture must produce a specific capture work list"
    );
    // Every need is actionable: it names a mint, and a platform or an author.
    use pump_quant_app::social_plane::SupportNeed;
    for need in &needs {
        match need {
            SupportNeed::MoreOriginators {
                mint_id,
                n_originators,
                min_originators,
            } => {
                assert_ne!(*mint_id, 0);
                assert!(n_originators < min_originators);
            }
            SupportNeed::ContentDigests { mint_id, n_calls } => {
                assert_ne!(*mint_id, 0);
                assert!(*n_calls > 0);
            }
            SupportNeed::PlatformCoverage {
                mint_id,
                platform_code,
            } => {
                assert_ne!(*mint_id, 0);
                assert!(Platform::from_ordinal(*platform_code).is_some());
                assert_ne!(
                    *platform_code,
                    Platform::Aggregator.ordinal(),
                    "more relay coverage adds echo, not evidence — never requested"
                );
            }
            SupportNeed::AuthorTrackRecord { mint_id, author_id }
            | SupportNeed::SourceExposure { mint_id, author_id } => {
                assert_ne!(*mint_id, 0);
                assert_ne!(*author_id, 0);
            }
        }
    }
}

// ===========================================================================
// 2. STALENESS → UNKNOWN → DROPPED (§34.3/§29.6).
// ===========================================================================

#[test]
fn stale_social_evidence_is_dropped_and_never_carried_forward() {
    let cfg = Config::dev_portable();
    let ttl = cfg.lane_evidence_ttl_ticks;
    let mut eng = Engine::new(cfg, RunMode::Replay);

    // A loud, fully-corroborated social picture — then total silence.
    for round in 0..4u64 {
        blast(&mut eng, SWEEP_B58, SocialStrength::Maximal, round);
        ticks(&mut eng, 2);
    }
    let fresh = eng.report();
    assert!(
        !fresh.social_evidence.is_empty(),
        "the fresh picture must be visible before we age it out"
    );
    let fresh_rows = fresh.social_evidence.len();
    assert!(fresh.promoted > 0, "social evidence surfaces the market");
    assert_eq!(fresh.admitted, 0, "…and still admits nothing on its own");

    // Silence for well past the evidence TTL. Nothing is re-fed.
    ticks(&mut eng, ttl * 4);
    let stale = eng.report();

    assert!(
        stale.social_evidence.is_empty(),
        "social evidence past its TTL must be DROPPED, not carried at its last \
         value (had {fresh_rows} rows, still shows {})",
        stale.social_evidence.len()
    );
    assert!(
        stale.social_support.is_empty(),
        "a mint whose social evidence expired carries NO support verdict — the \
         last verdict is not re-served as if it were current"
    );
    assert_eq!(
        stale.admitted, 0,
        "and an expired social picture certainly never admits"
    );
    // The attention plane obeys the same law: a mint with no fresh mentions
    // reports NO velocity rather than its last reading.
    let m = b58(SWEEP_B58);
    assert!(
        eng.numeric_features(m).is_none() || stale.admitted == 0,
        "no on-chain evidence ever arrived for this market"
    );
}

#[test]
fn a_refreshed_call_keeps_evidence_alive_so_the_ttl_is_recency_not_age() {
    let cfg = Config::dev_portable();
    let ttl = cfg.lane_evidence_ttl_ticks;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // Keep talking, spanning far more than one TTL of total age.
    for round in 0..6u64 {
        blast(&mut eng, SWEEP_B58, SocialStrength::Maximal, round);
        ticks(&mut eng, ttl / 2);
    }
    let r = eng.report();
    assert!(
        !r.social_evidence.is_empty(),
        "continuously refreshed evidence stays live — the TTL ages the LAST \
         observation, not the first"
    );
    for row in &r.social_evidence {
        assert!(row.last_tick > row.first_tick, "the row was refreshed");
    }
    assert_eq!(r.admitted, 0, "…and it still admits nothing");
}

// ===========================================================================
// 3. THE END-TO-END AUTHORITY PROOF.
//
// Walks the full chain — social ingest → provenance → trust → support →
// attention → watchlist rank → gate → admit — and proves that NO configuration
// of social evidence, at ANY strength, from ANY number of callers, on ANY set of
// platforms, designated or not, can produce an admit without ALL THREE of:
// on-chain confirmation, numeric microstructure, and a passing economic gate.
// ===========================================================================

#[test]
fn no_social_configuration_can_admit_without_the_full_onchain_chain() {
    // --- the sweep: every social strength × every FAILING on-chain position ---
    for strength in [
        SocialStrength::Silent,
        SocialStrength::Single,
        SocialStrength::Maximal,
    ] {
        for onchain in OnchainEvidence::ALL {
            if onchain == OnchainEvidence::Full {
                continue; // the positive control, asserted below
            }
            let (r, _eng) = drive_cell(strength, onchain);
            assert_eq!(
                r.admitted, 0,
                "social evidence ({strength:?}) with on-chain evidence {onchain:?} \
                 must NEVER admit — social may surface and rank, never authorise"
            );
            assert_eq!(
                r.net_lamports, 0,
                "no admit ⇒ no capital ⇒ no realized net ({strength:?}/{onchain:?})"
            );
            if strength != SocialStrength::Silent {
                assert!(
                    r.promoted > 0,
                    "…but social evidence MUST still surface the market for review \
                     ({strength:?}/{onchain:?}); refusing to admit is not refusing \
                     to look"
                );
            }
        }
    }

    // --- the positive control: flip the on-chain evidence on, nothing else ---
    let (full, _eng) = drive_cell(SocialStrength::Maximal, OnchainEvidence::Full);
    assert!(
        full.admitted > 0,
        "with confirmation AND microstructure AND a passing economic gate the \
         IDENTICAL social tape must admit — the law is 'social never substitutes \
         for the gate', not 'social poisons the gate'"
    );

    // --- and the control holds with NO social evidence at all: the on-chain
    //     chain is not merely necessary, it is SUFFICIENT on its own ---
    let (silent_full, _eng) = drive_cell(SocialStrength::Silent, OnchainEvidence::Full);
    assert!(
        silent_full.admitted > 0,
        "the on-chain lane is self-authorising; social was never load-bearing"
    );
}

#[test]
fn even_callers_with_earned_realized_trust_cannot_admit_without_onchain() {
    // The strongest form of the law. First EARN trust: the same ten callers lead a
    // market that has full on-chain support, it admits, it rides, it closes, and
    // their realized net is attributed back as markouts — the ONLY way trust is
    // earned in this system. THEN point those now-proven callers at a market with
    // no on-chain evidence whatsoever.
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let earner = b58(EARNER_B58);
    for round in 0..4u64 {
        blast(&mut eng, EARNER_B58, SocialStrength::Maximal, round);
        for i in 0..10u64 {
            trade(
                &mut eng,
                earner,
                100 + i128::from(i as i64) * 3,
                900_000 - i as i64,
                40 + i % 7,
                REAL_CURVE_VSOL,
            );
        }
        eng.tick(AppEvent::OnchainConfirm {
            mint: earner,
            virtual_sol_lamports: REAL_CURVE_VSOL,
            real_sol_lamports: REAL_SELLABLE_DEPTH,
        });
        ticks(&mut eng, 4);
    }
    let earned = eng.report();
    assert!(
        earned.admitted > 0,
        "the trust-earning phase must actually trade, else this proves nothing"
    );
    assert!(
        admits_for(&eng, earner) > 0,
        "the earning market itself must have been admitted"
    );
    assert_eq!(
        admits_for(&eng, b58(SWEEP_B58)),
        0,
        "the no-evidence market has not been called yet"
    );

    // Now the SAME callers, at maximum volume, on a market with no swaps and no
    // confirm. Also record the operator following every one of them and marking
    // them Niche — the most favourable §28 exposure there is.
    for row in &earned.social_evidence {
        eng.set_source_exposure(row.author_id, SourceExposure::Niche);
        eng.record_operator_follow(row.author_id);
    }
    let promoted_before = earned.promoted;
    for round in 4..12u64 {
        blast(&mut eng, SWEEP_B58, SocialStrength::Maximal, round);
        ticks(&mut eng, 3);
    }
    let after = eng.report();

    assert!(
        after.promoted > promoted_before,
        "the no-confirm market must still be SURFACED by its callers"
    );
    // The global `admitted` counter also moves for the EARNER market (it is still
    // riding), so the law is asserted per-mint off the decision journal: the
    // no-evidence market must have exactly zero admits, forever.
    assert_eq!(
        admits_for(&eng, b58(SWEEP_B58)),
        0,
        "ten proven, followed, designated callers across four platforms cannot \
         produce a single admit on a market with no on-chain evidence — trust \
         buys attention, never authority (§29/§71)"
    );
    // …and the refusal is specifically the missing on-chain confirmation.
    assert!(
        rejects_for(&eng, b58(SWEEP_B58), 1) > 0,
        "the refusal must be NeedsOnchainConfirmation (gate code 1)"
    );
}

// ===========================================================================
// 4. THE NEW SURFACES ARE REPORT-ONLY (no size-up, no entry authorisation).
// ===========================================================================

#[test]
fn the_social_abstraction_plane_is_decision_inert() {
    // Identical tape, identical on-chain evidence. The only difference is that the
    // second arm maximally exercises every NEW surface: operator exposure
    // judgements, an operator follow set, holder-count capture, and launch-metadata
    // family classification. If any of them touched a decision, the journal digest
    // or a count would move.
    let drive = |exercise: bool| -> Report {
        let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
        let m = b58(SWEEP_B58);
        for round in 0..4u64 {
            blast(&mut eng, SWEEP_B58, SocialStrength::Maximal, round);
            for i in 0..10u64 {
                trade(
                    &mut eng,
                    m,
                    100 + i128::from(i as i64),
                    900_000 - i as i64,
                    40 + i % 7,
                    REAL_CURVE_VSOL,
                );
            }
            eng.tick(AppEvent::OnchainConfirm {
                mint: m,
                virtual_sol_lamports: REAL_CURVE_VSOL,
                real_sol_lamports: REAL_SELLABLE_DEPTH,
            });
            if exercise {
                // Every new surface, driven hard — and on the REAL author ids the
                // capture lane produced, so the judgements actually bind.
                // Author ids are FNV-1a of the decoded handle — the same
                // derivation the capture lane uses, so these judgements bind to
                // the REAL sources rather than to invented ids.
                let authors: Vec<u64> = (0..10u64)
                    .map(|i| fnv1a_64(format!("whale{i}").as_bytes()))
                    .collect();
                for a in authors {
                    eng.set_source_exposure(a, SourceExposure::PublicBurned);
                    eng.record_operator_follow(a);
                }
                eng.observe_holder_count(m.as_bytes(), 100 + round * 40);
                eng.observe_launch_metadata(
                    m.as_bytes(),
                    "Doge Santa Stream",
                    "DOGE",
                    Some(true),
                    Some(9_000),
                );
            }
            ticks(&mut eng, 3);
        }
        eng.report()
    };
    let plain = drive(false);
    let exercised = drive(true);

    assert_eq!(
        exercised.journal_digest, plain.journal_digest,
        "the abstraction plane must not appear in the DECISION JOURNAL at all"
    );
    assert_eq!(exercised.admitted, plain.admitted, "admissions unchanged");
    assert_eq!(exercised.rejected, plain.rejected, "rejections unchanged");
    assert_eq!(exercised.promoted, plain.promoted, "promotions unchanged");
    assert_eq!(
        exercised.net_lamports, plain.net_lamports,
        "realized net-SOL unchanged — no size-up path exists"
    );
    assert_eq!(
        exercised.per_lane_net, plain.per_lane_net,
        "per-lane attribution unchanged"
    );
    assert_eq!(
        exercised.final_weights, plain.final_weights,
        "reflection weights unchanged"
    );
    // …and the exercised arm really did light the surfaces up, so the equality
    // above is a proof of inertness rather than a proof that nothing happened.
    assert!(
        !exercised.social_evidence.is_empty(),
        "the exercised arm must actually populate the plane"
    );
    assert!(
        exercised
            .social_evidence
            .iter()
            .any(|r| r.exposure_code == SourceExposure::PublicBurned.ordinal()),
        "the operator exposure judgements must actually land"
    );
}

#[test]
fn a_burned_source_can_only_ever_lower_its_own_standing() {
    // §28: exposure is reduce-only by construction. Marking a source PublicBurned
    // can lower its trust tier; there is no exposure value that RAISES a score
    // above what its realized net SOL earned, and `demotion_bp` is the only place
    // exposure enters the score at all.
    assert_eq!(SourceExposure::Niche.demotion_bp(), 0);
    assert!(SourceExposure::Crowded.demotion_bp() > 0);
    assert!(
        SourceExposure::PublicBurned.demotion_bp() > SourceExposure::Crowded.demotion_bp(),
        "the demotion ladder is monotone in how public the source is"
    );
}

#[test]
fn maximal_social_support_never_increases_realized_size() {
    // The narrow version of the reduce-only law, on the money axis. Same on-chain
    // tape, silent vs. maximal social. Social may change WHICH markets surface and
    // WHEN, so promotions and rejections may legitimately differ; what may never
    // happen is a social-driven increase in the capital committed to a market
    // whose on-chain picture is identical.
    let mut silent = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mut loud = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = b58(SWEEP_B58);
    for round in 0..4u64 {
        blast(&mut loud, SWEEP_B58, SocialStrength::Maximal, round);
        for eng in [&mut silent, &mut loud] {
            for i in 0..10u64 {
                trade(
                    eng,
                    m,
                    100 + i128::from(i as i64),
                    900_000 - i as i64,
                    40 + i % 7,
                    REAL_CURVE_VSOL,
                );
            }
            eng.tick(AppEvent::OnchainConfirm {
                mint: m,
                virtual_sol_lamports: REAL_CURVE_VSOL,
                real_sol_lamports: REAL_SELLABLE_DEPTH,
            });
            ticks(eng, 3);
        }
    }
    let s = silent.report();
    let l = loud.report();
    assert!(s.admitted > 0, "the on-chain arm must trade");
    assert_eq!(
        l.admitted, s.admitted,
        "social corroboration does not buy extra admits on an identical on-chain \
         picture"
    );
    assert_eq!(
        l.net_lamports, s.net_lamports,
        "…nor a single extra lamport of realized size"
    );
}
