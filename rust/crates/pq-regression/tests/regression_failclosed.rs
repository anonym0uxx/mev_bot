//! REGRESSION CLASS 3 — fail-closed invariants.
//!
//! The safety properties that must NEVER regress:
//!   * no `RunMode::Live` variant exists (live capital is Tier-0 human-gated and
//!     unrepresentable in this binary);
//!   * `promotion_readiness` never reports `live_probe_eligible` on a pure
//!     paper/replay run;
//!   * absent / thin evidence stays UNKNOWN on the sentiment-aggregator (source
//!     classification) and creator (classifier) paths (§6.4 UNKNOWN discipline);
//!   * bounded state (§99) stays ≤ cap when fed far past capacity.
//!
//! All integer, deterministic, no wall-clock, no RNG (§22).

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};

// ---------------------------------------------------------------------------
// No RunMode::Live variant.
//
// This exhaustive match has NO wildcard arm. If a `Live` (or any other) variant
// is ever added to `RunMode`, this stops compiling — a compile-time tripwire that
// the paper/replay-only contract has been broken.
// ---------------------------------------------------------------------------

#[test]
fn run_mode_has_only_paper_and_replay() {
    fn assert_exhaustive(m: RunMode) -> &'static str {
        match m {
            RunMode::Paper => "paper",
            RunMode::Replay => "replay",
        }
    }
    assert_eq!(assert_exhaustive(RunMode::Paper), "paper");
    assert_eq!(assert_exhaustive(RunMode::Replay), "replay");
}

// ---------------------------------------------------------------------------
// promotion_readiness is never live-probe-eligible on paper/replay.
// ---------------------------------------------------------------------------

#[test]
fn promotion_readiness_never_live_eligible_on_paper_run() {
    // Fresh engine: nothing measured.
    let eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    assert!(
        !eng.promotion_readiness().live_probe_eligible,
        "a fresh paper/replay engine must never be live-probe-eligible"
    );

    // After driving the full golden tape (many realized trades), STILL not
    // eligible — the laptop cannot attest live capability, so it fails closed.
    let mut driven = Engine::new(Config::dev_portable(), RunMode::Replay);
    driven.run(&pq_regression_golden_events());
    assert!(
        !driven.promotion_readiness().live_probe_eligible,
        "a driven paper/replay engine must still fail closed (no live capability)"
    );
    // The blocker is a stable, honest label — never an accidental "eligible".
    assert_ne!(driven.promotion_readiness().blocked_on, "");
}

/// A tiny realized-trade tape (enough to exercise `promotion_readiness` past its
/// fresh state without paying the full golden tape's cost).
fn pq_regression_golden_events() -> Vec<pump_quant_app::event::AppEvent> {
    use pq_regression::golden_tape::mint;
    use pump_quant_app::event::AppEvent;
    let mut evs = Vec::new();
    let mt = mint(6_001);
    for i in 0..12u64 {
        evs.push(AppEvent::MarketTrade {
            mint: mt,
            price_fp: (100 + i as i128) * 10_000_000,
            quote_lamports: 800_000,
            liquidity_lamports: pq_regression::FIXTURE_VSOL_LAMPORTS,
            signed_base: 900_000 - i as i64,
            buyer_entity: 40 + i % 7,
            age_slots: 12,
        });
    }
    evs.push(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: pq_regression::FIXTURE_SELLABLE_LAMPORTS,
    });
    for _ in 0..20 {
        evs.push(AppEvent::Tick);
    }
    evs
}

// ---------------------------------------------------------------------------
// UNKNOWN stays UNKNOWN — sentiment / aggregator source classification path.
// ---------------------------------------------------------------------------

#[test]
fn empty_source_evidence_classifies_insufficient_sample() {
    use pump_quant_social::classification::{classify, ClassificationConfig, DeterminantBundle};
    use pump_quant_social::types::{DeterminantScore, SourceState};

    let empty = DeterminantScore::empty();
    let zero = DeterminantBundle {
        d1: empty,
        d2: empty,
        d3: empty,
        d4: empty,
        d5: empty,
        d6: empty,
        d7: empty,
        d8: empty,
        d9: empty,
        d10: empty,
        shill_suspect: false,
        post_peak_persistent: false,
        bot_farm: false,
        echo_heavy: false,
        total_sample: 0,
    };
    let cfg = ClassificationConfig::fade_first_default();

    // No evidence ⇒ the fade-first default, never a positive tier.
    assert_eq!(
        classify(&zero, &cfg).state,
        SourceState::InsufficientSample,
        "absent source evidence must resolve to INSUFFICIENT_SAMPLE, not a guess"
    );

    // Sample just below the min stays INSUFFICIENT (the boundary must fail closed).
    let mut thin = zero;
    thin.total_sample = cfg.min_sample.saturating_sub(1);
    assert_eq!(
        classify(&thin, &cfg).state,
        SourceState::InsufficientSample,
        "sub-threshold sample must stay INSUFFICIENT_SAMPLE"
    );
}

// ---------------------------------------------------------------------------
// UNKNOWN stays UNKNOWN — creator classifier path.
// ---------------------------------------------------------------------------

#[test]
fn thin_creator_evidence_classifies_unknown() {
    use pump_quant_wallet_graph::creator_classifier::{
        classify_creator, CreatorClass, CreatorInputs, CreatorThresholds,
    };

    let zero = CreatorInputs {
        prior_launch_count: 0,
        resolved_launch_count: 0,
        rugged_launch_count: 0,
        max_launches_in_window: 0,
        dump_intensity_bps: 0,
        median_survival_secs: 0,
        community_retention_bps: 0,
        streamer_launch_ratio_bps: 0,
        copycat_similarity_bps: 0,
    };
    let th = CreatorThresholds::test();
    assert_eq!(
        classify_creator(&zero, &th),
        CreatorClass::Unknown,
        "a creator with no evidence must classify UNKNOWN, not a guessed archetype"
    );

    // History below the evidence gate cannot earn a history-derived class.
    let mut thin = zero;
    thin.prior_launch_count = th.min_history_launches.saturating_sub(1);
    thin.median_survival_secs = 1; // would be ShortLivedRunner IF history sufficed
    assert_eq!(
        classify_creator(&thin, &th),
        CreatorClass::Unknown,
        "sub-gate history must stay UNKNOWN (§6.4 evidence gate)"
    );
}

// ---------------------------------------------------------------------------
// Bounded state (§99): feed far past cap, size stays ≤ cap.
// ---------------------------------------------------------------------------

#[test]
fn watchlist_stays_within_capacity_under_flood() {
    use pq_regression::baselines::WATCHLIST_CAPACITY;
    use pump_quant_watchlist::candidate::{Candidate, Features, Lane, Mint};
    use pump_quant_watchlist::rank::{LaneWeights, RankParams};
    use pump_quant_watchlist::state::WatchlistState;

    let cap = WATCHLIST_CAPACITY;
    let mut wl = WatchlistState::new(cap, RankParams::new(100), LaneWeights::default());
    assert_eq!(wl.capacity(), cap, "watchlist capacity default drifted");

    // Feed 8× capacity distinct candidates with varied scores/times.
    for tag in 0..(cap as u64 * 8) {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&tag.to_le_bytes());
        b[8] = 0xCD;
        let feat = Features {
            liquidity_lamports: 100_000_000 + tag,
            buy_pressure_bp: 5_000 + (tag as u32 % 3_000),
            unique_buyers: 3 + (tag as u32 % 20),
            age_slots: 10 + (tag as u32 % 50),
        };
        let cand = Candidate::new(
            Mint::new(b),
            Lane::ActiveMarketScalp,
            1_000 + (tag.wrapping_mul(2_654_435_761) % 9_000), // scattered scores
            tag,                                               // discovered_at
            feat,
        );
        wl.insert(cand, tag);
        assert!(
            wl.len() <= cap,
            "watchlist exceeded its §99 capacity ({} > {cap}) after {} inserts",
            wl.len(),
            tag + 1
        );
    }
    assert_eq!(
        wl.len(),
        cap,
        "a saturated watchlist must fill exactly to cap"
    );
}

#[test]
fn attention_field_tracks_at_most_track_cap_mints() {
    use pump_quant_app::attention::{AttentionField, AttentionParams};
    use pump_quant_narrative::attention_state::Mention;

    let cap = 32usize;
    let params = AttentionParams {
        track_cap: cap,
        ..AttentionParams::standard()
    };
    let mut f = AttentionField::new(params);
    // Feed 200 distinct mints — many more than the track cap.
    for k in 0..200u64 {
        let mut m = [0u8; 32];
        m[..8].copy_from_slice(&k.to_le_bytes());
        f.observe(
            m,
            Mention {
                ts_ns: 1_000 + k,
                source_id: k,
                community_id: k,
                weight: 100 + k,
                copycat: false,
            },
        );
        assert!(
            f.len() <= cap,
            "attention field exceeded its §99 track cap ({} > {cap})",
            f.len()
        );
    }
    assert_eq!(
        f.len(),
        cap,
        "a saturated attention field must hold exactly the cap"
    );
}

#[test]
fn live_chatter_breadth_is_bounded_and_deterministic() {
    use pump_quant_app::attention::{AttentionField, AttentionParams, MentionProvenance};
    use pump_quant_narrative::attention_state::Mention;

    // A flood of genuine distinct realtime chatters on ONE streamed mint.
    fn build(n_chatters: u64) -> u64 {
        let m = [9u8; 32];
        let mut f = AttentionField::new(AttentionParams::standard());
        for i in 0..n_chatters {
            f.observe_tagged(
                m,
                Mention {
                    ts_ns: 1_000_000_000 + i,
                    source_id: 300 + i,
                    community_id: 1,
                    weight: 50,
                    copycat: false,
                },
                &MentionProvenance {
                    realtime_chat: true,
                    broadcaster: i == 0,
                    author_id: 300 + i,
                    echo_or_coordinated: false,
                    aggregator: false,
                    bearish: false,
                    mainstream: false,
                    designated_caller: false,
                },
            );
        }
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 1_000, |_| true);
        buf.first().map(|c| c.discovery_score).unwrap_or(0)
    }

    // Determinism: identical floods score identically (§22).
    assert_eq!(
        build(64),
        build(64),
        "live-chat scoring must be deterministic"
    );

    // Boundedness (§99): the distinct-chatter breadth term saturates at
    // LIVE_CHATTER_CAP. Within the cap, breadth actively lifts the score; past
    // the cap, a 10× larger flood barely moves it. If the cap regressed, breadth
    // would keep climbing ~linearly with chatter count and the past-cap marginal
    // would dominate, not vanish.
    let few = build(4); // below the cap: breadth still accumulating
    let at_cap = build(16); // exactly at the cap
    let far_past = build(160); // 10× past the cap
    assert!(
        few > 0 && at_cap > 0,
        "a genuine live-chat field must score above zero"
    );
    assert!(
        at_cap > few,
        "breadth must lift the score while below the cap ({few} → {at_cap})"
    );
    // Adding 144 chatters PAST the cap moves the score by less than the breadth
    // gain seen over just 12 chatters WITHIN the cap — the term has saturated.
    let within_cap_gain = at_cap - few; // over 12 extra chatters (4→16)
    let past_cap_gain = far_past.saturating_sub(at_cap); // over 144 extra (16→160)
    assert!(
        past_cap_gain < within_cap_gain,
        "breadth beyond LIVE_CHATTER_CAP must saturate: past-cap gain {past_cap_gain} \
         (16→160) must be below the within-cap gain {within_cap_gain} (4→16); the §99 cap regressed"
    );
}
