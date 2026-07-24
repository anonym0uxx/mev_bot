//! LAWs B6–B9 — the **brain → strategy-analysis** seam.
//!
//! Before these laws the brain observed and reported and nothing consumed it:
//! reflection reweighted on aggregates it could not partition, the exit tournament
//! raced a fixed grid it could not question, promotion never heard from the
//! episodic record, and no artifact left the process for a research consumer. This
//! file is the correctness authority for closing that, and specifically for the
//! four properties that make the closure safe rather than merely present:
//!
//! * **B6** the export is byte-deterministic, bounded, sorted, and NEVER emits an
//!   estimate for a verdict the brain refused to give;
//! * **B7** the brain-informed lane reweight is reduce-only, envelope-bounded and
//!   fail-closed — and its A/B is recorded here honestly, including the fact that
//!   it did not earn (the pre-registered two-sided experiment that decides the
//!   default lives in `tests/brain_reflect_twosided.rs`);
//! * **B8** exit proposals fail closed at small n and never auto-adopt;
//! * **B9** recall is an additional promotion blocker and never a promotion
//!   licence — and the §56 retirement flags retire nothing.

// Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85 (the
// helper stabilised in 1.87) — the same choice `engine.rs` documents.
#![allow(clippy::manual_is_multiple_of)]

use pump_quant_app::brain_analysis::{
    is_conditioned_negative, lane_decay, retirement_flags, AnalysisInputs, FlagSubject,
    ANALYSIS_CLASS_CAP, ANALYSIS_LENS_CAP, ANALYSIS_RETIREMENT_CAP,
};
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::shadow::{brain_exit_proposals, ProposalAxis};
use pump_quant_domain::ids::Mint;

// ===========================================================================
// A brain-rich tape.
//
// 384 markets over 6 rounds, each with real microstructure and an on-chain
// confirm, plus the four discovery lanes. The COHORT SPLIT is the point: mints in
// the "decay cohort" (`m % 5 == 0` — the ones the social lane surfaces) run hard
// in rounds 0–1 and then bleed from round 2 on, while everything else keeps a
// steady modest shape. That is the "one early runner carrying twenty later
// bleeders" pattern a lane AGGREGATE cannot see and a conditioned recall can.
//
// `decayed = false` produces the same tape with a uniform (non-decaying) shape,
// so the two arms differ in the market, not in the engine.
// ===========================================================================

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xC1;
    Mint::from_bytes(b)
}

/// Deterministic per-mint trajectory (no RNG, §22). Returns
/// `(price_fp, signed_base)` at `(round, i)`.
fn traj(m: u64, round: u64, i: u64, decayed: bool) -> (i128, i64) {
    let h = m
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x0F1E_2D3C_4B5A_6978)
        .rotate_left(23);
    let spread = h % 1_000;
    let in_cohort = m % 5 == 0;

    // Peak multiple, bps of entry.
    let peak_bp: u64 = if in_cohort && decayed {
        if round <= 1 {
            // The early runner: this is what leaves the lane's AGGREGATE positive.
            18_000 + spread * 4_000 / 1_000
        } else {
            // …and this is what the conditioned recall sees afterwards.
            9_300 + spread % 200
        }
    } else if spread < 450 {
        10_000 + spread % 250
    } else {
        11_500 + spread * 1_500 / 1_000
    };
    // Signed excursion from break-even, so a cohort that ends BELOW entry is
    // expressible without an unsigned underflow.
    let excursion = peak_bp as i64 - 10_000;
    let settle_bp: i64 = if excursion <= 300 {
        8_200 + (spread % 600) as i64
    } else {
        10_000 + excursion / 2
    };
    let mult_bp: i64 = match round {
        0 => 10_000,
        1 => 10_000 + excursion / 3,
        2 => 10_000 + excursion * 2 / 3,
        3 => 10_000 + excursion,
        _ => settle_bp,
    };
    let base = 1_000_000_000i128 * i128::from(mult_bp) / 10_000;
    let price_fp = base + (i as i128) * 400_000 + (m as i128 % 97) * 10_000;
    // Order flow follows the price path: buying into the peak, selling after.
    let buying = round <= 3;
    let signed_base = if buying {
        700_000 + (m as i64 % 11) * 5_000 - (i as i64) * 1_000
    } else {
        -(650_000 + (m as i64 % 13) * 4_000)
    };
    (price_fp, signed_base)
}

fn drive(cfg: Config, decayed: bool) -> Engine {
    let mut cfg = cfg;
    // Same cost-realism overrides the golden tape uses, so the gate is the real
    // §18 economic gate rather than a permissive one.
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_protocol_bps = 450;
    cfg.gate_margin_bps = 150;
    cfg.gate_base_fixed_lamports = 200_000;
    cfg.gate_impact_den = 250_000;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let n = 384u64;
    for round in 0..6u64 {
        for m in 0..n {
            let mt = mint(m);
            for i in 0..3u64 {
                let (price_fp, signed_base) = traj(m, round, i, decayed);
                eng.tick(AppEvent::MarketTrade {
                    mint: mt,
                    price_fp,
                    quote_lamports: 420_000 + (m % 17) * 1_000,
                    liquidity_lamports: 130_000_000 + (m % 300) * 1_000_000 + round * 11,
                    signed_base,
                    buyer_entity: (m + i) % 89,
                    age_slots: 10 + (m as u32 % 35),
                });
            }
            if round == m % 4 {
                if m % 2 == 0 {
                    eng.tick(AppEvent::OnchainConfirm {
                        mint: mt,
                        sellable_depth_lamports: 160_000_000 + m * 400,
                    });
                }
                if m % 5 == 0 {
                    eng.tick(AppEvent::SocialCall {
                        mint: mt,
                        source_quality_bp: 2_200 + (m as u32 % 400),
                    });
                }
                if m % 3 == 0 {
                    eng.tick(AppEvent::NarrativeSample {
                        mint: mt,
                        prior_active: 4 + m % 9,
                        new_mentions: 120 + m * 3,
                    });
                }
                if m % 7 == 0 {
                    eng.tick(AppEvent::WalletAction {
                        mint: mt,
                        followable: m % 2 == 0,
                        size_lamports: 12_000_000 + m * 1_500,
                    });
                }
            }
        }
        for _ in 0..12 {
            eng.tick(AppEvent::Tick);
        }
    }
    eng
}

// ===========================================================================
// LAW B6 — the export artifact.
// ===========================================================================

/// Determinism: the same tape produces the byte-identical artifact. This is the
/// property a research consumer diffing two runs depends on absolutely.
#[test]
fn export_is_byte_identical_across_identical_runs() {
    let mut a = drive(Config::dev_portable(), true);
    let _ = a.report();
    let mut b = drive(Config::dev_portable(), true);
    let _ = b.report();
    let ja = a.brain_analysis_json();
    let jb = b.brain_analysis_json();
    assert_eq!(ja, jb, "same tape -> byte-identical brain_analysis JSON");
    // And stable within one engine: the artifact is a pure read, not a consuming
    // one.
    assert_eq!(ja, a.brain_analysis_json());
}

/// The record header is exactly the schema the consumer was built against.
#[test]
fn export_carries_the_pinned_record_tag_and_schema_version() {
    let mut e = drive(Config::dev_portable(), true);
    let _ = e.report();
    let j = e.brain_analysis_json();
    assert!(
        j.starts_with("{\"record\":\"brain_analysis_v1\",\"schema_version\":1,\"info_time_ns\":"),
        "header drifted: {}",
        &j[..120.min(j.len())]
    );
    for key in [
        "\"episodes_total\":",
        "\"episodes_admitted\":",
        "\"setup_classes\":[",
        "\"lens_scoreboard\":[",
        "\"best_paying_lens\":",
        "\"meta_state\":[",
        "\"past_meta_matches\":[",
        "\"caller_trust\":[",
        "\"follow_recommendations\":[",
        "\"unfollow_candidates\":[",
        "\"support_inputs_needed\":[",
        "\"retirement_flags\":[",
    ] {
        assert!(j.contains(key), "missing {key}");
    }
}

/// **§46, the load-bearing one.** Every object that declares
/// `"confidence":"unknown"` must carry a refusal reason and `null` in EVERY
/// estimate field. A consumer must not be able to read a number the brain refused
/// to give.
#[test]
fn export_never_emits_an_estimate_for_an_unknown_verdict() {
    let mut e = drive(Config::dev_portable(), true);
    let _ = e.report();
    let j = e.brain_analysis_json();

    let mut unknowns = 0usize;
    // Walk each `{...}` object that contains a confidence field.
    for obj in split_objects(&j) {
        if !obj.contains("\"confidence\":") {
            continue;
        }
        if obj.contains("\"confidence\":\"known\"") {
            // A known row must NOT carry a refusal reason.
            assert!(
                obj.contains("\"unknown_reason\":null"),
                "known row carries a refusal reason: {obj}"
            );
            continue;
        }
        assert!(
            obj.contains("\"confidence\":\"unknown\""),
            "confidence is a closed vocabulary: {obj}"
        );
        unknowns += 1;
        assert!(
            !obj.contains("\"unknown_reason\":null"),
            "an unknown row must SAY why: {obj}"
        );
        for field in [
            "n",
            "median_net_lamports",
            "mean_net_lamports",
            "win_rate_bp",
            "p25_net_lamports",
            "p75_net_lamports",
            "median_hold_ns",
            "nearest_distance",
        ] {
            let key = format!("\"{field}\":");
            if let Some(pos) = obj.find(&key) {
                let tail = &obj[pos + key.len()..];
                assert!(
                    tail.starts_with("null"),
                    "unknown row leaked an estimate in `{field}`: {obj}"
                );
            }
        }
    }
    assert!(
        unknowns > 0,
        "this tape must exercise the refusal path, or the law is untested"
    );
}

/// Bounded (§99) and totally ordered (§22): every array respects its named cap and
/// its documented sort key.
#[test]
fn export_arrays_are_bounded_and_sorted() {
    let mut e = drive(Config::dev_portable(), true);
    let _ = e.report();
    let a = e.brain_analysis();

    assert!(a.setup_classes.len() <= ANALYSIS_CLASS_CAP);
    assert!(a.lens_scoreboard.len() <= ANALYSIS_LENS_CAP);
    assert!(a.retirement_flags.len() <= ANALYSIS_RETIREMENT_CAP);

    // setup_classes: sample DESC, then median DESC, then signature ASC.
    let key = |c: &pump_quant_app::brain_analysis::ClassRow| {
        let (n, m) = c
            .stats
            .map_or((0u32, 0i128), |s| (s.n_matched, s.median_net_lamports));
        (std::cmp::Reverse(n), std::cmp::Reverse(m), c.signature)
    };
    for w in a.setup_classes.windows(2) {
        assert!(key(&w[0]) <= key(&w[1]), "setup_classes order drifted");
    }
    // lens_scoreboard: the fixed (phase, lens) grid — every slot present, so a
    // missing lens is visibly a refusal and never an absence.
    assert_eq!(a.lens_scoreboard.len(), ANALYSIS_LENS_CAP);
    assert_eq!(a.lens_scoreboard[0].venue_phase, "curve");
    assert_eq!(a.lens_scoreboard[ANALYSIS_LENS_CAP - 1].venue_phase, "pool");
    // caller_trust: ascending author id.
    for w in a.caller_trust.windows(2) {
        assert!(w[0].author_id <= w[1].author_id);
    }
    // retirement_flags: worst realized net first.
    for w in a.retirement_flags.windows(2) {
        assert!(w[0].realized_net_lamports <= w[1].realized_net_lamports);
    }
    // meta_state: ascending category.
    for w in a.meta_state.windows(2) {
        assert!(w[0].meta_category <= w[1].meta_category);
    }
}

/// Integers only (§22): no decimal point, no exponent form, anywhere.
#[test]
fn export_contains_no_float_syntax() {
    let mut e = drive(Config::dev_portable(), true);
    let _ = e.report();
    let j = e.brain_analysis_json();
    assert!(!j.contains('.'), "a decimal point reached the artifact");
    for bad in ["e+", "e-", "E+", "E-", "NaN", "Infinity"] {
        assert!(
            !j.contains(bad),
            "float syntax `{bad}` reached the artifact"
        );
    }
}

/// The file sink and the in-memory seam agree byte for byte.
#[test]
fn export_file_and_memory_seams_agree() {
    let mut cfg = Config::dev_portable();
    let dir = std::env::temp_dir().join(format!("pq_brain_analysis_{}", std::process::id()));
    let path = dir.join("brain_analysis.json");
    cfg.brain_analysis_path =
        pump_quant_app::config::CfgPath::from_str_checked(path.to_str().unwrap())
            .expect("path fits");
    let mut e = drive(cfg, true);
    let _ = e.report();
    e.brain_analysis()
        .write_to_path(&path)
        .expect("write artifact");
    let read = std::fs::read_to_string(&path).expect("read artifact");
    assert_eq!(read.trim_end(), e.brain_analysis_json());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The artifact is REPORT PLANE: producing it changes nothing about the run.
#[test]
fn producing_the_export_is_decision_inert() {
    let mut on = drive(Config::dev_portable(), true);
    let r_on = on.report();
    // Sample the artifact repeatedly mid-report; the report must not move.
    let _ = on.brain_analysis_json();
    let _ = on.brain_analysis_json();
    let r_again = on.report();
    assert_eq!(r_on.journal_digest, r_again.journal_digest);
    assert_eq!(r_on.net_lamports, r_again.net_lamports);

    let mut cfg = Config::dev_portable();
    cfg.brain_analysis_enable = false;
    let mut off = drive(cfg, true);
    let r_off = off.report();
    // The switch is a §19 identity field, so the DIGEST is expected to differ;
    // every DECISION-plane number must not.
    assert_eq!(r_on.net_lamports, r_off.net_lamports);
    assert_eq!(r_on.admitted, r_off.admitted);
    assert_eq!(r_on.promoted, r_off.promoted);
    assert_eq!(r_on.rejected, r_off.rejected);
}

// ===========================================================================
// LAW B6/§56 — retirement flags.
// ===========================================================================

/// **Fail-closed.** With the decay floor raised above anything the tape can
/// supply, there is no flag at all — not a weak one, not a provisional one.
#[test]
fn retirement_flags_fail_closed_at_small_n() {
    let mut cfg = Config::dev_portable();
    cfg.brain_decay_min_sample = 100_000;
    let mut e = drive(cfg, true);
    let _ = e.report();
    let a = e.brain_analysis();
    assert!(
        a.retirement_flags
            .iter()
            .all(|f| f.subject == FlagSubject::Source),
        "only the per-source ledger (which carries no count) may survive an \
         impossible sample floor; everything conditioned must refuse"
    );
    // …and the recall evidence the promotion report consults refuses too.
    assert_eq!(e.recall_evidence().conditioned_negative, 0);
    assert!(!e.recall_evidence().blocks());
}

/// Every emitted flag stands on a sample at or above the floor (the per-source
/// family excepted, which reports `n = 0` precisely because its ledger carries no
/// count and says so in its reason string).
#[test]
fn every_conditioned_flag_clears_the_sample_floor() {
    let cfg = Config::dev_portable();
    let floor = cfg.brain_decay_min_sample;
    let mut e = drive(cfg, true);
    let _ = e.report();
    for f in e.brain_analysis().retirement_flags {
        match f.subject {
            FlagSubject::Source => assert_eq!(f.n, 0),
            _ => assert!(
                f.n >= floor,
                "flag {} stood on n={} below the floor {floor}",
                f.key,
                f.n
            ),
        }
    }
}

/// **The boundary.** A retirement flag retires nothing: the engine's own
/// §56.11 retirement verdicts and its realized net are identical whether the
/// flags are computed or not.
#[test]
fn retirement_flags_retire_nothing() {
    let mut e = drive(Config::dev_portable(), true);
    let before = e.report();
    let flags = e.brain_analysis().retirement_flags;
    let after = e.report();
    assert_eq!(before.net_lamports, after.net_lamports);
    assert_eq!(before.journal_digest, after.journal_digest);
    assert_eq!(before.admitted, after.admitted);
    // The flags exist as a NOMINATION list and nothing consumed them.
    println!(
        "RETIREMENT-FLAGS n={} {:?}",
        flags.len(),
        flags
            .iter()
            .map(|f| (&f.key, f.reason, f.n))
            .collect::<Vec<_>>()
    );
}

/// **The boundary, structurally.** A flag hands governance a NOMINATION, and a
/// nomination cannot become a retirement without the §51 statistical verdict AND
/// the §52 baseline verdict. There is no function that turns episodic evidence
/// alone into a retirement, and this proves the one bridge that exists does not.
#[test]
fn a_retirement_flag_is_a_nomination_not_a_retirement() {
    use pump_quant_governance::retirement_review::{review, ReviewOutcome};

    let mut e = drive(Config::dev_portable(), true);
    let _ = e.report();
    let flags = e.brain_analysis().retirement_flags;
    assert!(!flags.is_empty(), "this tape must produce nominations");
    for f in &flags {
        let nom = f.as_nomination();
        assert_eq!(nom.n, f.n);
        assert_eq!(nom.realized_net_lamports, f.realized_net_lamports);
        assert_eq!(nom.subject.name(), f.subject.name());
        // Episodic evidence alone: KEEP, whatever the flag says.
        assert_eq!(review(&nom, 1, false, false), ReviewOutcome::Keep);
        assert_eq!(review(&nom, 1, true, false), ReviewOutcome::Keep);
        assert_eq!(review(&nom, 1, false, true), ReviewOutcome::Keep);
        assert!(!review(&nom, 1, true, false).retires());
        // …and the governed path still works when both verdicts concur.
        assert!(review(&nom, 1, true, true).retires());
    }
}

// ===========================================================================
// LAW B7 — the reduce-only lane reweight A/B.
// ===========================================================================

/// The mechanism, at the unit level: a lane whose AGGREGATE is positive because of
/// a few early runners but whose conditioned classes bleed is flagged; the same
/// lane with healthy classes is not; and both fail closed below the floor.
#[test]
fn lane_decay_flags_the_runner_carried_lane_and_fails_closed() {
    use pump_quant_app::brain::{BrainPlane, ConditionedClass};
    use pump_quant_brain::episode::DiscoveryLane as BrainLane;
    use pump_quant_brain::fingerprint::VenuePhase;
    use pump_quant_brain::recall::{RecallStats, RecallUnknown, RecallVerdict};
    use pump_quant_watchlist::candidate::Lane;

    let stats = |n: u32, median: i128| RecallStats {
        n_matched: n,
        median_net_lamports: median,
        mean_net_lamports: median,
        win_count: 0,
        loss_count: n,
        win_rate_bp: 0,
        p25_net_lamports: median,
        p75_net_lamports: median,
        median_hold_ns: 1,
        nearest_distance: 0,
        nearest_weighted_distance: 0,
        nearest_episode_id: 1,
    };
    let class = |lane: BrainLane, v: RecallVerdict| ConditionedClass {
        signature: 1,
        venue_phase: VenuePhase::Curve,
        meta_category_id: 0,
        discovery_lane: lane,
        verdict: v,
    };

    // Twenty bleeders on the social lane, plus a refused (n=3) runner class that
    // contributes nothing: flagged.
    let bleeding = [
        class(
            BrainLane::SocialCall,
            RecallVerdict::Known(stats(20, -400_000)),
        ),
        class(
            BrainLane::SocialCall,
            RecallVerdict::Unknown(RecallUnknown::InsufficientSample {
                n_matched: 3,
                min_sample: 12,
            }),
        ),
    ];
    let d = lane_decay(&bleeding, 12);
    assert!(d.is_decayed(Lane::CreationSniper));
    assert!(!d.is_decayed(Lane::ActiveMarketScalp));

    // Healthy classes: never flagged. There is no up-weight path to reach.
    let healthy = [class(
        BrainLane::SocialCall,
        RecallVerdict::Known(stats(20, 400_000)),
    )];
    assert!(lane_decay(&healthy, 12).is_empty());

    // Below the floor: fail closed, however bad the median.
    assert!(lane_decay(&bleeding, 21).is_empty());
    // Refusals alone are never evidence of decay.
    let only_refusals = [class(
        BrainLane::SocialCall,
        RecallVerdict::Unknown(RecallUnknown::EmptyIndex),
    )];
    assert!(lane_decay(&only_refusals, 1).is_empty());

    // The `BrainPlane` accessor the engine actually reads is consistent with the
    // pure function above on an empty plane (nothing traded ⇒ nothing flagged).
    let plane = BrainPlane::new(8, 8);
    assert!(lane_decay(&plane.conditioned_classes(), 12).is_empty());
}

/// **The A/B.** Armed vs neutral on the decayed tape and on the uniform tape.
///
/// The armed arm is expected to be reduce-only and envelope-bounded; whether it
/// EARNS is an empirical question and the answer is printed, asserted only where
/// a law (not a hope) binds. On THIS tape it is exactly neutral (the flagged lane's
/// weight moves but no admission does). The definitive, pre-registered two-sided
/// experiment — a tape where the mechanism genuinely CAN act, plus its
/// false-positive mirror image and the golden neutral control — lives in
/// `tests/brain_reflect_twosided.rs`; it is why `brain_reflect_enable` still
/// defaults OFF.
#[test]
fn reflect_ab_armed_vs_neutral() {
    for decayed in [true, false] {
        let mut neutral = drive(Config::dev_portable(), decayed);
        let r_neutral = neutral.report();
        let decay_flags = lane_decay(
            &neutral.brain_conditioned_classes(),
            Config::dev_portable().brain_decay_min_sample,
        );

        let mut cfg = Config::dev_portable();
        cfg.brain_reflect_enable = true;
        let mut armed = drive(cfg, decayed);
        let r_armed = armed.report();

        println!(
            "REFLECT-AB decayed={decayed} flagged_lanes={} neutral_net={} armed_net={} \
             delta={} neutral_admitted={} armed_admitted={} neutral_weights={:?} armed_weights={:?}",
            decay_flags.count(),
            r_neutral.net_lamports,
            r_armed.net_lamports,
            r_armed.net_lamports - r_neutral.net_lamports,
            r_neutral.admitted,
            r_armed.admitted,
            r_neutral.final_weights,
            r_armed.final_weights
        );

        // The law that binds regardless of the economics: REDUCE-ONLY. No lane's
        // final weight under the armed arm may exceed the neutral arm's.
        for (i, (lane, w_armed)) in r_armed.final_weights.iter().enumerate() {
            let (lane_n, w_neutral) = r_neutral.final_weights[i];
            assert_eq!(*lane, lane_n);
            assert!(
                w_armed <= &w_neutral,
                "LAW B7 is reduce-only: {lane:?} {w_armed} > {w_neutral}"
            );
        }
    }
}

// ===========================================================================
// LAW B8 — brain-grounded exit proposals.
// ===========================================================================

/// Fail-closed: with the floor above anything the tape supplies, there is no
/// proposal at all.
#[test]
fn shadow_proposals_fail_closed_at_small_n() {
    use pump_quant_app::position::LifecycleParams;
    use pump_quant_brain::fingerprint::VenuePhase;

    let mut cfg = Config::dev_portable();
    cfg.brain_decay_min_sample = 100_000;
    let mut e = drive(cfg, true);
    let _ = e.report();
    assert!(
        e.exit_proposals().is_empty(),
        "§46: below the floor there is no proposal, not a weak one"
    );

    // …and directly, over an empty index.
    let plane = pump_quant_app::brain::BrainPlane::new(8, 8);
    let params = LifecycleParams::default();
    assert!(
        brain_exit_proposals(plane.index(), &params, VenuePhase::Curve, 1, 400_000_000).is_empty()
    );
    // A zero tick scale is a degenerate projection: refuse rather than divide.
    assert!(brain_exit_proposals(plane.index(), &params, VenuePhase::Curve, 1, 0).is_empty());
}

/// The derivation itself, on a cohort that DOES clear the floor: median hold of
/// winners becomes a time stop, p75 MFE becomes a target, median heat becomes a
/// trail — each on one axis, each inside its envelope, and none of them adopted.
#[test]
fn shadow_proposals_derive_from_the_winners_distribution() {
    use pump_quant_app::brain::{BrainEntry, BrainPlane};
    use pump_quant_app::position::LifecycleParams;
    use pump_quant_app::shadow::{PROPOSAL_TP1_MFE_SHARE_BP, PROPOSAL_TRAIL_MAE_SLACK_BP};
    use pump_quant_brain::episode::{DiscoveryLane, EpisodeContext, ExitReason};
    use pump_quant_brain::fingerprint::{SetupFingerprint, SetupInputs, VenuePhase};

    const TICK_NS: u64 = 400_000_000;
    let inputs = SetupInputs {
        venue_phase: VenuePhase::Curve,
        ..SetupInputs::default()
    };
    let entry = BrainEntry {
        fingerprint: SetupFingerprint::from_inputs(&inputs),
        context: EpisodeContext {
            mint_id: 1,
            venue_phase: VenuePhase::Curve,
            meta_category_id: 0,
            discovery_lane: DiscoveryLane::NewMint,
            info_time_ns: 0,
            slot: 0,
        },
    };
    let mut plane = BrainPlane::new(8, 8);
    // 16 winners with a known hold / MFE / MAE distribution, plus 4 losers that
    // must be EXCLUDED from every derivation (the proposals are grounded in what
    // PAID, not in what happened).
    for k in 0..16u64 {
        plane.record_exit(
            &entry,
            1_000_000,
            (600 + k * 50) * TICK_NS,
            ExitReason::TakeProfit,
            2_000 + (k as i64) * 100,
            -(400 + (k as i64) * 20),
        );
    }
    for _ in 0..4 {
        plane.record_exit(
            &entry,
            -9_000_000,
            10 * TICK_NS,
            ExitReason::StopLoss,
            0,
            -9_000,
        );
    }
    let incumbent = LifecycleParams::default();
    let props = brain_exit_proposals(plane.index(), &incumbent, VenuePhase::Curve, 12, TICK_NS);
    println!("DERIVED-PROPOSALS {props:?}");
    assert!(!props.is_empty(), "16 winners must clear a floor of 12");

    // Time stop = winners' median hold in ticks. Holds are 600..1350 step 50 over
    // 16 samples; nearest-rank p50 (index 8) is 1000.
    let ts = props
        .iter()
        .find(|p| p.axis == ProposalAxis::TimeStopTicks)
        .expect("a time-stop proposal");
    assert_eq!(ts.value, 1_000);
    assert_eq!(ts.derived_from_n, 16, "losers must not inflate the sample");

    // TP1 = 10_000 + share × p75 MFE. MFEs are 2000..3500 step 100; nearest-rank
    // p75 (index 12) is 3200.
    let tp1 = props
        .iter()
        .find(|p| p.axis == ProposalAxis::Tp1Bps)
        .expect("a TP1 proposal");
    assert_eq!(
        tp1.value,
        10_000 + 3_200 * PROPOSAL_TP1_MFE_SHARE_BP / 10_000
    );

    // Trail = slack × winners' median |MAE|. |MAE| is 400..700 step 20; p50
    // (index 8) is 560.
    let trail = props
        .iter()
        .find(|p| p.axis == ProposalAxis::TrailBps)
        .expect("a trail proposal");
    assert_eq!(trail.value, 560 * PROPOSAL_TRAIL_MAE_SLACK_BP / 10_000);

    // The OTHER phase has no evidence at all: fail closed, not "borrow the curve".
    assert!(
        brain_exit_proposals(plane.index(), &incumbent, VenuePhase::Pool, 12, TICK_NS).is_empty()
    );

    // NOTHING was adopted: the incumbent params are untouched by the derivation.
    assert_eq!(incumbent, LifecycleParams::default());
}

/// Proposals are report-only and single-axis, and every value sits inside its
/// named envelope. Nothing here adopts anything.
#[test]
fn shadow_proposals_are_bounded_single_axis_and_inert() {
    use pump_quant_app::shadow::{
        PROPOSAL_HOLD_MAX_TICKS, PROPOSAL_HOLD_MIN_TICKS, PROPOSAL_TP1_MAX_BPS,
        PROPOSAL_TP1_MIN_BPS, PROPOSAL_TRAIL_MAX_BPS, PROPOSAL_TRAIL_MIN_BPS,
    };
    let mut e = drive(Config::dev_portable(), true);
    let before = e.report();
    let props = e.exit_proposals();
    println!("EXIT-PROPOSALS {props:?}");
    for p in &props {
        assert_ne!(
            p.value, p.incumbent_value,
            "a proposal identical to the incumbent is not a challenger"
        );
        match p.axis {
            ProposalAxis::TimeStopTicks => {
                assert!((PROPOSAL_HOLD_MIN_TICKS..=PROPOSAL_HOLD_MAX_TICKS).contains(&p.value));
            }
            ProposalAxis::Tp1Bps => {
                assert!(
                    (u64::from(PROPOSAL_TP1_MIN_BPS)..=u64::from(PROPOSAL_TP1_MAX_BPS))
                        .contains(&p.value)
                );
            }
            ProposalAxis::TrailBps => {
                assert!(
                    (u64::from(PROPOSAL_TRAIL_MIN_BPS)..=u64::from(PROPOSAL_TRAIL_MAX_BPS))
                        .contains(&p.value)
                );
            }
        }
    }
    // One axis per proposal, and at most one proposal per (phase, axis).
    let mut seen: Vec<(u8, ProposalAxis)> = Vec::new();
    for p in &props {
        let k = (p.venue_phase_code, p.axis);
        assert!(!seen.contains(&k), "duplicate axis {k:?}");
        seen.push(k);
    }
    // Report-only: computing them moved nothing, and the live tournament grid is
    // untouched (the incumbent standings are identical).
    let after = e.report();
    assert_eq!(before.journal_digest, after.journal_digest);
    assert_eq!(before.net_lamports, after.net_lamports);
}

// ===========================================================================
// LAW B9 — recall as a promotion INPUT.
// ===========================================================================

/// Recall never grants promotion and never masks an earlier blocker; a paper run
/// stays blocked for the reason it was blocked for before this law existed.
#[test]
fn recall_is_an_additional_blocker_never_a_licence() {
    let mut e = drive(Config::dev_portable(), true);
    let _ = e.report();
    let r = e.promotion_readiness();
    assert!(
        !r.live_probe_eligible,
        "no paper run is ever probe-eligible (§38/§64)"
    );
    // The pre-existing labels still bind first.
    assert!(
        r.blocked_on == "mode_c_required"
            || r.blocked_on.starts_with("probe_gate:")
            || r.blocked_on.starts_with("promotion_verdict:")
            || r.blocked_on == "recall:conditioned_negative",
        "unexpected blocker label {}",
        r.blocked_on
    );
    println!(
        "PROMOTION blocked_on={} recall_evidence={:?}",
        r.blocked_on, r.recall_evidence
    );
    // With the brain disarmed the evidence is empty and blocks nothing.
    let mut cfg = Config::dev_portable();
    cfg.brain_enable = false;
    let mut off = drive(cfg, true);
    let _ = off.report();
    assert!(!off.recall_evidence().blocks());
    assert_eq!(off.recall_evidence().classes_examined, 0);
}

/// The conditioned-negative predicate needs BOTH a negative median and a negative
/// mean — a lottery shape and a fat-left-tail shape are exit-ladder problems, not
/// existence problems.
#[test]
fn conditioned_negative_requires_median_and_mean() {
    use pump_quant_brain::recall::RecallStats;
    let s = RecallStats {
        n_matched: 20,
        median_net_lamports: -1,
        mean_net_lamports: 1,
        win_count: 1,
        loss_count: 19,
        win_rate_bp: 500,
        p25_net_lamports: -2,
        p75_net_lamports: 3,
        median_hold_ns: 1,
        nearest_distance: 0,
        nearest_weighted_distance: 0,
        nearest_episode_id: 1,
    };
    assert!(!is_conditioned_negative(&s, 12));
    let both = RecallStats {
        mean_net_lamports: -1,
        ..s
    };
    assert!(is_conditioned_negative(&both, 12));
    assert!(!is_conditioned_negative(&both, 21));
}

/// Retirement flags computed over an EMPTY engine are empty, and the whole
/// analysis builder is total on an engine that has traded nothing.
#[test]
fn an_untraded_engine_produces_an_empty_but_valid_artifact() {
    let e = Engine::new(Config::dev_portable(), RunMode::Replay);
    let a = e.brain_analysis();
    assert_eq!(a.episodes_total, 0);
    assert_eq!(a.episodes_admitted, 0);
    assert!(a.setup_classes.is_empty());
    assert!(a.retirement_flags.is_empty());
    assert!(a.best_paying_lens.is_none());
    // The lens grid is still fully populated, every slot a refusal.
    assert_eq!(a.lens_scoreboard.len(), ANALYSIS_LENS_CAP);
    assert!(a.lens_scoreboard.iter().all(|l| l.stats.is_none()));
    let j = a.to_canonical_json();
    assert!(j.contains("\"best_paying_lens\":null"));
    assert!(j.contains("\"retirement_flags\":[]"));
    // Sanity: the empty-state builder is deterministic too.
    assert_eq!(j, e.brain_analysis_json());
    // The retirement-flag builder itself is total over an empty engine.
    let plane = pump_quant_app::brain::BrainPlane::new(8, 8);
    let social = pump_quant_app::social_plane::SocialPlane::new();
    let lp = pump_quant_watchlist::lane_performance::LanePerformance::new();
    let dp = pump_quant_watchlist::lane_performance::DiscoveryLanePerformance::new();
    let inputs = AnalysisInputs {
        info_time_ns: 0,
        tick: 0,
        brain: &plane,
        social: &social,
        lane_perf: &lp,
        disc_perf: &dp,
        alpha_source_net: &[],
        min_sample: 8,
        decay_min_sample: 12,
    };
    assert!(retirement_flags(&inputs, &[]).is_empty());
}

/// Print one real artifact for the build record (`cargo test -- --nocapture`).
#[test]
fn emit_a_real_sample_artifact() {
    let mut e = drive(Config::dev_portable(), true);
    let r = e.report();
    println!(
        "SAMPLE-CONTEXT admitted={} net={} episodes={}",
        r.admitted, r.net_lamports, r.brain_episodes_recorded
    );
    println!("SAMPLE-JSON {}", e.brain_analysis_json());
}

// ---------------------------------------------------------------------------
// A minimal object splitter for the schema assertions. Not a JSON parser — it
// only needs to hand back each innermost `{...}` run, which is enough to check
// per-row invariants without pulling in a dependency (the workspace has none).
// ---------------------------------------------------------------------------
fn split_objects(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => {
                depth += 1;
                if depth == 2 {
                    start = i;
                }
            }
            '}' => {
                if depth == 2 {
                    out.push(s[start..=i].to_string());
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    out
}
