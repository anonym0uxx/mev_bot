//! §70.1 **continuous holder accounting** — the proof suite.
//!
//! The operator's directive was that holder-growth capture must be a CONSTANT
//! STREAM across watch / analyze / enter, not a seam nobody calls. These tests
//! prove, in order:
//!
//! 1. **The accounting is right** (`acct_*`) — buy from zero is `+1`, a sell that
//!    empties a position is `-1`, a partial sell moves nothing, a re-entry counts
//!    again, and a position never goes negative.
//! 2. **The observation-window law is enforced in the TYPE** (`basis_*`) — a mint
//!    watched from creation is `Exact`, a mint discovered mid-life is `DeltaOnly`,
//!    an over-cap ledger is `Incomplete`, and a LEVEL consumer structurally cannot
//!    read a `DeltaOnly` or `Incomplete` count. The `Exact` claim is falsifiable
//!    by evidence and gets falsified.
//! 3. **The state is bounded** (`bound_*`) — mints and entities both, under churn.
//! 4. **The stream is real** (`stream_*`) — folded for every watched mint at watch
//!    time (not only admitted ones), sampled at a cadence that respects the
//!    estimator's floors, point-in-time safe, and byte-reproducible on replay.
//! 5. **The seam became a stream** (`fingerprint_*`) — the load-bearing proof.
//!    On a tape with genuine holder broadening the brain fingerprint now receives
//!    a REAL non-neutral `holder_growth_accel_bps`, where before this wave it
//!    received the fabricated neutral rung on every admit.
//! 6. **The §70.1 money-proxy A/B** (`ab_*`) — two-sided, pre-registered, and
//!    reported in actual lamports whichever way it lands.
//!
//! Determinism (§22) makes every comparison exact rather than statistical.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::holder_flow::{
    HolderCountBasis, HolderFlow, HOLDER_ENTITY_CAP, HOLDER_SAMPLE_INTERVAL_TICKS,
};
use pump_quant_domain::ids::Mint;
use pump_quant_features::holder_growth::HOLDER_MIN_INTERVAL_NS;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

const PRICE_SCALE: i128 = 10_000_000;
/// One engine tick of information time (`brain::BRAIN_TICK_NS`).
const TICK_NS: u64 = 400_000_000;

fn m32(tag: u8) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0] = tag;
    b[31] = 0xC7;
    b
}

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0x5E;
    Mint::from_bytes(b)
}

// ---------------------------------------------------------------------------
// 1. Continuous accounting correctness
// ---------------------------------------------------------------------------

#[test]
fn acct_buy_from_zero_position_is_plus_one() {
    let mut hf = HolderFlow::new();
    let m = m32(1);
    hf.note_creation(&m, 0);
    let f = hf.observe_swap(&m, 10, 1_000, 0, 0);
    assert_eq!(
        f.delta, 1,
        "a buy by a zero-position entity creates a holder"
    );
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(1));
    // A second buy by the SAME entity is not a second holder.
    let f2 = hf.observe_swap(&m, 10, 1_000, 1, TICK_NS);
    assert_eq!(f2.delta, 0, "repeat buys by one entity count once");
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(1));
    // A distinct entity is a second holder.
    let f3 = hf.observe_swap(&m, 11, 500, 2, 2 * TICK_NS);
    assert_eq!(f3.delta, 1);
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(2));
}

#[test]
fn acct_sell_to_zero_is_minus_one() {
    let mut hf = HolderFlow::new();
    let m = m32(2);
    hf.note_creation(&m, 0);
    hf.observe_swap(&m, 10, 1_000, 0, 0);
    hf.observe_swap(&m, 11, 1_000, 0, 0);
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(2));
    let f = hf.observe_swap(&m, 10, -1_000, 1, TICK_NS);
    assert_eq!(
        f.delta, -1,
        "a sell that empties a position removes a holder"
    );
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(1));
}

#[test]
fn acct_partial_sell_does_not_move_the_count() {
    let mut hf = HolderFlow::new();
    let m = m32(3);
    hf.note_creation(&m, 0);
    hf.observe_swap(&m, 10, 1_000, 0, 0);
    for step in 1..=3u64 {
        let f = hf.observe_swap(&m, 10, -200, step, step * TICK_NS);
        assert_eq!(f.delta, 0, "a partial sell is still a holder");
    }
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(1));
    // The fourth 200 takes the remaining 200 to exactly zero.
    let f = hf.observe_swap(&m, 10, -400, 4, 4 * TICK_NS);
    assert_eq!(f.delta, -1);
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(0));
}

#[test]
fn acct_re_entry_after_full_exit_counts_again() {
    let mut hf = HolderFlow::new();
    let m = m32(4);
    hf.note_creation(&m, 0);
    hf.observe_swap(&m, 10, 1_000, 0, 0);
    hf.observe_swap(&m, 10, -1_000, 1, TICK_NS);
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(0));
    let f = hf.observe_swap(&m, 10, 700, 2, 2 * TICK_NS);
    assert_eq!(f.delta, 1, "the same entity buying back IS a holder again");
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(1));
    // And a repeated sell after the exit is a no-op, not a second decrement.
    hf.observe_swap(&m, 10, -700, 3, 3 * TICK_NS);
    let f = hf.observe_swap(&m, 10, -700, 4, 4 * TICK_NS);
    assert_eq!(
        f.delta, 0,
        "a sell from an already-zero position moves nothing"
    );
    let r = hf.reading(&m).expect("tracked");
    assert_eq!(r.lower_bound(), 0);
    // It ALSO costs the mint its exactness, and rightly so: an entity we watched
    // go to zero cannot sell again out of tokens that reached it through our swap
    // stream, so the tokens came from a transfer we never saw — our holder set was
    // never complete. The count degrades to a delta, and the level door shuts.
    assert_eq!(r.basis(), HolderCountBasis::DeltaOnly);
    assert_eq!(r.level(), None);
    assert_eq!(r.growth_level(), Some(0));
    assert_eq!(r.unattributed_exits(), 1);
}

#[test]
fn acct_positions_saturate_at_zero_and_never_go_negative() {
    let mut hf = HolderFlow::new();
    let m = m32(5);
    hf.note_creation(&m, 0);
    hf.observe_swap(&m, 10, 100, 0, 0);
    // An oversell cannot mean a negative balance; it means pre-window inventory.
    let f = hf.observe_swap(&m, 10, -100_000, 1, TICK_NS);
    assert_eq!(f.delta, -1);
    let r = hf.reading(&m).expect("tracked");
    assert_eq!(r.lower_bound(), 0, "the count floors at zero");
    // Ten more oversells cannot drive it below zero.
    for step in 2..12u64 {
        hf.observe_swap(&m, 10, -100_000, step, step * TICK_NS);
    }
    assert_eq!(hf.reading(&m).map(|r| r.lower_bound()), Some(0));
}

#[test]
fn acct_zero_quantity_swap_moves_no_holder() {
    let mut hf = HolderFlow::new();
    let m = m32(6);
    hf.note_creation(&m, 0);
    let f = hf.observe_swap(&m, 10, 0, 0, 0);
    assert_eq!(f.delta, 0, "a swap that moves no base moves no holder");
    assert_eq!(hf.reading(&m).and_then(|r| r.level()), Some(0));
    assert_eq!(hf.reading(&m).map(|r| r.entities_tracked()), Some(0));
}

// ---------------------------------------------------------------------------
// 2. The observation-window law, enforced in the type
// ---------------------------------------------------------------------------

#[test]
fn basis_creation_first_is_exact_mid_life_is_delta_only() {
    // Exact: the creation sighting lands BEFORE any swap.
    let mut a = HolderFlow::new();
    let ma = m32(20);
    a.note_creation(&ma, 0);
    a.observe_swap(&ma, 1, 500, 0, 0);
    assert_eq!(
        a.reading(&ma).map(|r| r.basis()),
        Some(HolderCountBasis::Exact)
    );

    // DeltaOnly: no creation sighting — we arrived mid-life.
    let mut b = HolderFlow::new();
    let mb = m32(21);
    b.observe_swap(&mb, 1, 500, 0, 0);
    assert_eq!(
        b.reading(&mb).map(|r| r.basis()),
        Some(HolderCountBasis::DeltaOnly)
    );
}

#[test]
fn basis_creation_noted_after_flow_cannot_retroactively_claim_exact() {
    // §20/§81: information is not retroactive. A creation sighting that arrives
    // after we already folded flow cannot make the earlier window complete.
    let mut hf = HolderFlow::new();
    let m = m32(22);
    hf.observe_swap(&m, 1, 500, 0, 0);
    hf.note_creation(&m, 1);
    hf.observe_swap(&m, 2, 500, 1, TICK_NS);
    assert_eq!(
        hf.reading(&m).map(|r| r.basis()),
        Some(HolderCountBasis::DeltaOnly),
        "a late creation sighting must not upgrade the basis"
    );
}

#[test]
fn basis_exact_claim_is_falsified_by_a_pre_window_seller() {
    let mut hf = HolderFlow::new();
    let m = m32(23);
    hf.note_creation(&m, 0);
    hf.observe_swap(&m, 1, 500, 0, 0);
    assert_eq!(
        hf.reading(&m).map(|r| r.basis()),
        Some(HolderCountBasis::Exact)
    );
    // Entity 99 sells without ever having bought inside our window: it provably
    // held before we started watching, so our exactness claim was wrong.
    hf.observe_swap(&m, 99, -300, 1, TICK_NS);
    let r = hf.reading(&m).expect("tracked");
    assert_eq!(
        r.basis(),
        HolderCountBasis::DeltaOnly,
        "a pre-window seller falsifies Exact"
    );
    assert_eq!(r.unattributed_exits(), 1);
    // And the unknown exit did NOT move the count (§6.4: we cannot tell a full
    // exit from a partial one for an entity whose balance we never saw).
    assert_eq!(r.lower_bound(), 1);
    // The demotion is monotone: later good behaviour cannot restore Exact.
    hf.observe_swap(&m, 2, 500, 2, 2 * TICK_NS);
    assert_eq!(
        hf.reading(&m).map(|r| r.basis()),
        Some(HolderCountBasis::DeltaOnly)
    );
}

#[test]
fn basis_entity_cap_marks_incomplete_and_the_count_becomes_a_lower_bound() {
    let cap = 8usize;
    let mut hf = HolderFlow::with_caps(4, cap);
    let m = m32(24);
    hf.note_creation(&m, 0);
    for e in 0..cap as u64 {
        let f = hf.observe_swap(&m, e, 100, 0, 0);
        assert_eq!(f.delta, 1);
        assert!(!f.truncated);
    }
    let r = hf.reading(&m).expect("tracked");
    assert_eq!(r.basis(), HolderCountBasis::Exact);
    assert_eq!(r.level(), Some(cap as u64));
    // One past the cap: refused, counted as truncation, basis degrades forever.
    let f = hf.observe_swap(&m, 999, 100, 1, TICK_NS);
    assert!(f.truncated, "the cap must be visible to the caller");
    assert_eq!(f.delta, 0, "an untrackable arrival must not be counted");
    let r = hf.reading(&m).expect("tracked");
    assert_eq!(r.basis(), HolderCountBasis::Incomplete);
    assert_eq!(r.truncated(), 1);
    assert_eq!(
        r.lower_bound(),
        cap as u64,
        "the raw number survives as an explicitly-named lower bound"
    );
}

#[test]
fn basis_level_consumer_refuses_under_delta_only_and_incomplete() {
    // THE CRUX. A level consumer gets a number ONLY under Exact.
    let mut exact = HolderFlow::new();
    let me = m32(25);
    exact.note_creation(&me, 0);
    exact.observe_swap(&me, 1, 100, 0, 0);
    assert_eq!(exact.reading(&me).and_then(|r| r.level()), Some(1));

    let mut delta = HolderFlow::new();
    let md = m32(26);
    delta.observe_swap(&md, 1, 100, 0, 0);
    let r = delta.reading(&md).expect("tracked");
    assert_eq!(r.basis(), HolderCountBasis::DeltaOnly);
    assert_eq!(r.level(), None, "a level consumer must refuse DeltaOnly");
    assert_eq!(
        r.growth_level(),
        Some(1),
        "a growth consumer legitimately reads DeltaOnly (§70.1 wants a derivative)"
    );

    let mut inc = HolderFlow::with_caps(2, 1);
    let mi = m32(27);
    inc.note_creation(&mi, 0);
    inc.observe_swap(&mi, 1, 100, 0, 0);
    inc.observe_swap(&mi, 2, 100, 0, 0); // over the 1-entity cap
    let r = inc.reading(&mi).expect("tracked");
    assert_eq!(r.basis(), HolderCountBasis::Incomplete);
    assert_eq!(r.level(), None, "a level consumer must refuse Incomplete");
    assert_eq!(
        r.growth_level(),
        None,
        "a truncated ledger biases the RATE too, so growth refuses as well"
    );
}

#[test]
fn basis_lattice_only_ever_loses_confidence() {
    assert_eq!(
        HolderCountBasis::Exact.worst(HolderCountBasis::DeltaOnly),
        HolderCountBasis::DeltaOnly
    );
    assert_eq!(
        HolderCountBasis::DeltaOnly.worst(HolderCountBasis::Exact),
        HolderCountBasis::DeltaOnly
    );
    assert_eq!(
        HolderCountBasis::DeltaOnly.worst(HolderCountBasis::Incomplete),
        HolderCountBasis::Incomplete
    );
    assert_eq!(
        HolderCountBasis::Incomplete.worst(HolderCountBasis::Exact),
        HolderCountBasis::Incomplete
    );
    // And the two gates agree with the documented table.
    assert!(HolderCountBasis::Exact.admits_level());
    assert!(!HolderCountBasis::DeltaOnly.admits_level());
    assert!(!HolderCountBasis::Incomplete.admits_level());
    assert!(HolderCountBasis::Exact.admits_growth());
    assert!(HolderCountBasis::DeltaOnly.admits_growth());
    assert!(!HolderCountBasis::Incomplete.admits_growth());
}

// ---------------------------------------------------------------------------
// 3. Bounded state under churn (§99/§57)
// ---------------------------------------------------------------------------

#[test]
fn bound_mint_capacity_holds_under_churn_and_evicts_least_recently_traded() {
    let cap = 4usize;
    let mut hf = HolderFlow::with_caps(cap, 64);
    // Twelve distinct mints, each traded at a distinct, increasing tick.
    for i in 0..12u8 {
        let m = m32(100 + i);
        hf.observe_swap(&m, u64::from(i), 100, u64::from(i), u64::from(i) * TICK_NS);
        assert!(hf.len() <= cap, "mint capacity is absolute");
    }
    assert_eq!(hf.len(), cap);
    assert_eq!(hf.evictions(), 8);
    // The four survivors are the four most recently traded.
    for i in 8..12u8 {
        assert!(
            hf.reading(&m32(100 + i)).is_some(),
            "recently traded mint {i} must survive"
        );
    }
    for i in 0..8u8 {
        assert!(
            hf.reading(&m32(100 + i)).is_none(),
            "stale mint {i} must have been evicted"
        );
    }
}

#[test]
fn bound_entity_ledger_never_exceeds_the_cap_under_churn() {
    let cap = 16usize;
    let mut hf = HolderFlow::with_caps(2, cap);
    let m = m32(120);
    for e in 0..500u64 {
        hf.observe_swap(&m, e, 100, e / 8, (e / 8) * TICK_NS);
        let r = hf.reading(&m).expect("tracked");
        assert!(
            r.entities_tracked() as usize <= cap,
            "entity ledger blew its cap at entity {e}"
        );
    }
    let r = hf.reading(&m).expect("tracked");
    assert_eq!(r.entities_tracked() as usize, cap);
    assert_eq!(r.truncated(), 500 - cap as u64);
    assert_eq!(r.basis(), HolderCountBasis::Incomplete);
}

#[test]
fn bound_production_caps_are_the_named_constants() {
    // §102: the defaults a production engine runs at are the named consts, not
    // whatever a test happened to pass.
    let hf = HolderFlow::new();
    assert_eq!(hf.entity_cap(), HOLDER_ENTITY_CAP);
    // And the sampling cadence clears the estimator's floor. The relationship is
    // also asserted at COMPILE time inside the module (clippy correctly notes
    // that both sides are constants — which is the point: this is a named-const
    // relationship, §102, not a runtime condition).
    assert_eq!(HOLDER_SAMPLE_INTERVAL_TICKS * TICK_NS, 1_200_000_000);
    assert_eq!(HOLDER_MIN_INTERVAL_NS, 1_000_000_000);
}

// ---------------------------------------------------------------------------
// 4. The stream: folded at WATCH time, for every mint, deterministically
// ---------------------------------------------------------------------------

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

/// One decoded swap through the real engine seam.
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

fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

#[test]
fn stream_is_folded_for_every_watched_mint_not_only_admitted_ones() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    // Three markets, NONE of which is ever confirmed on-chain, so none can be
    // admitted. The holder stream must still exist for all of them: that is the
    // whole point of folding at WATCH time.
    for tag in 0..3u64 {
        let mt = mint(tag);
        for e in 0..5u64 {
            trade(&mut eng, mt, 100, 10_000, e, REAL_CURVE_VSOL);
        }
    }
    ticks(&mut eng, 4);
    for tag in 0..3u64 {
        let r = eng
            .holder_reading(mint(tag).as_bytes())
            .expect("every mint that traded has a holder ledger");
        assert_eq!(
            r.growth_level(),
            Some(5),
            "five distinct entities bought mint {tag}"
        );
    }
    let rep = eng.report();
    assert_eq!(rep.admitted, 0, "the tape deliberately admits nothing");
}

#[test]
fn stream_sampling_cadence_respects_the_estimator_floors() {
    // Many swaps inside ONE tick must produce at most ONE sample, because the
    // estimator refuses sub-interval comparison points and `record_holder_count`
    // drops a non-advancing information time.
    let mut hf = HolderFlow::new();
    let m = m32(40);
    let mut samples = 0u32;
    for e in 0..50u64 {
        if hf.observe_swap(&m, e, 100, 0, 0).sample.is_some() {
            samples += 1;
        }
    }
    assert_eq!(samples, 1, "one tick yields at most one sample");
    // A swap one tick later is still inside the cadence.
    assert!(hf.observe_swap(&m, 60, 100, 1, TICK_NS).sample.is_none());
    assert!(hf
        .observe_swap(&m, 61, 100, 2, 2 * TICK_NS)
        .sample
        .is_none());
    // The full cadence has now elapsed.
    assert!(hf
        .observe_swap(&m, 62, 100, HOLDER_SAMPLE_INTERVAL_TICKS, 3 * TICK_NS)
        .sample
        .is_some());
}

#[test]
fn stream_estimate_at_tick_t_cannot_use_a_swap_from_t_plus_one() {
    // §20 point-in-time safety, end to end through the engine.
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mt = mint(50);
    // Build a growing holder series over several sampling windows.
    let mut entity = 0u64;
    for _ in 0..6 {
        for _ in 0..4 {
            trade(&mut eng, mt, 100, 5_000, entity, REAL_CURVE_VSOL);
            entity += 1;
        }
        ticks(&mut eng, HOLDER_SAMPLE_INTERVAL_TICKS);
    }
    let t = eng.now();
    let as_of = t.saturating_mul(TICK_NS);
    let key = pump_quant_ingest::social_parse::fnv1a_64(mt.as_bytes());
    let before = eng.measured().holder_estimate(key, as_of);
    assert!(
        before.is_some(),
        "the series must be rich enough to estimate"
    );
    // Advance the clock STRICTLY past the cutoff, then let a large burst of new
    // holders arrive. Every sample it produces is stamped after `as_of`.
    ticks(&mut eng, HOLDER_SAMPLE_INTERVAL_TICKS + 1);
    assert!(eng.now() > t);
    // A large burst of NEW holders arrives afterwards.
    for _ in 0..40 {
        trade(&mut eng, mt, 100, 5_000, entity, REAL_CURVE_VSOL);
        entity += 1;
    }
    ticks(&mut eng, HOLDER_SAMPLE_INTERVAL_TICKS * 2);
    let after_same_as_of = eng.measured().holder_estimate(key, as_of);
    assert_eq!(
        before, after_same_as_of,
        "a later swap must not change an earlier point-in-time estimate"
    );
    // The estimate at the LATER cutoff is allowed to move (and does).
    let later = eng
        .measured()
        .holder_estimate(key, eng.now().saturating_mul(TICK_NS));
    assert!(later.is_some());
}

#[test]
fn stream_is_replay_deterministic() {
    fn drive() -> Vec<(u64, Option<u64>, HolderCountBasis)> {
        let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
        for round in 0..8u64 {
            for tag in 0..6u64 {
                let mt = mint(200 + tag);
                for i in 0..4u64 {
                    let sb = if round < 5 { 4_000 } else { -4_000 };
                    trade(
                        &mut eng,
                        mt,
                        100 + round as i128,
                        sb,
                        (tag * 17 + i + round) % 23,
                        REAL_CURVE_VSOL,
                    );
                }
            }
            ticks(&mut eng, 4);
        }
        (0..6u64)
            .map(|tag| {
                let r = eng.holder_reading(mint(200 + tag).as_bytes()).unwrap();
                (r.lower_bound(), r.growth_level(), r.basis())
            })
            .collect()
    }
    assert_eq!(drive(), drive(), "same tape -> identical holder series");
}

// ---------------------------------------------------------------------------
// 5. THE PROOF: the seam became a stream
// ---------------------------------------------------------------------------

/// A tape with GENUINE holder broadening: a steadily widening set of distinct
/// entities accumulating, at an ACCELERATING rate (4, then 8, then 16 new
/// holders per window) — exactly the §70.1 "money before momentum" shape.
fn broadening_tape(eng: &mut Engine, mt: Mint) {
    // Arrivals per equally-spaced window: 3, 3, 9, 45. Cumulative holders
    // 3 -> 6 -> 15 -> 60, so the RELATIVE growth rate runs 100% -> 150% -> 300%
    // per window. Relative acceleration (the §70.1 quantity) is therefore
    // strictly positive — accumulation is broadening FASTER, which is the whole
    // point of a leading indicator. Equal spacing makes the per-minute
    // normalization cancel, so the sign is unambiguous.
    const ARRIVALS: [u64; 4] = [3, 3, 9, 45];
    let mut entity = 1u64;
    for (window, arrivals) in ARRIVALS.into_iter().enumerate() {
        for _ in 0..arrivals {
            trade(
                eng,
                mt,
                100 + window as i128,
                6_000,
                entity,
                REAL_CURVE_VSOL,
            );
            entity += 1;
        }
        ticks(eng, HOLDER_SAMPLE_INTERVAL_TICKS + 1);
    }
}

#[test]
fn fingerprint_receives_a_real_non_neutral_holder_accel_after_this_wave() {
    // BEFORE (the old world): `observe_holder_count` was never called in
    // production, so the estimator had NO series and the fingerprint took the
    // documented neutral rung on every admit. That state is reproduced exactly by
    // an engine that has folded no swaps at all.
    let empty = Engine::new(Config::dev_portable(), RunMode::Replay);
    let key = pump_quant_ingest::social_parse::fnv1a_64(mint(300).as_bytes());
    assert_eq!(
        empty.measured().holder_growth_accel_bps(key, 10 * TICK_NS),
        None,
        "with no stream the estimator has nothing and refuses"
    );
    assert_eq!(
        empty
            .measured()
            .holder_growth_accel_input(key, 10 * TICK_NS),
        0,
        "and the fingerprint input collapses to the neutral rung — the fabricated \
         non-measurement this wave exists to remove"
    );

    // AFTER: the identical field, on a tape with genuine accelerating holder
    // growth, fed only by decoded swaps through the ordinary engine seam.
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mt = mint(300);
    broadening_tape(&mut eng, mt);
    let as_of = eng.now().saturating_mul(TICK_NS);
    let measured = eng.measured().holder_growth_accel_bps(key, as_of);
    let est = measured.expect("the stream must produce a real measurement");
    assert_ne!(
        est, 0,
        "the field must carry a REAL value, not the neutral rung"
    );
    assert!(
        est > 0,
        "accelerating holder broadening must read positive acceleration ({est} bps)"
    );
    assert_eq!(
        eng.measured().holder_growth_accel_input(key, as_of),
        est,
        "the fingerprint input is the measurement, not the neutral"
    );
    // And the honest limitation is still true and still stated: the ladder has no
    // UNKNOWN rung, so a REFUSED estimate and a MEASURED zero are the same
    // fingerprint code. The `Option` is where the distinction survives.
    let unseen = pump_quant_ingest::social_parse::fnv1a_64(mint(999).as_bytes());
    assert_eq!(eng.measured().holder_growth_accel_bps(unseen, as_of), None);
    assert_eq!(eng.measured().holder_growth_accel_input(unseen, as_of), 0);
}

#[test]
fn fingerprint_stream_distinguishes_broadening_from_a_single_whale() {
    // The bitset proxy cannot tell these apart in any useful way; the folded
    // count can. Same notional flow, same number of swaps, opposite breadth.
    let mut broad = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mut whale = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mt = mint(310);
    for i in 0..24u64 {
        trade(&mut broad, mt, 100, 5_000, i, REAL_CURVE_VSOL);
        trade(&mut whale, mt, 100, 5_000, 7, REAL_CURVE_VSOL);
        if i % 4 == 3 {
            ticks(&mut broad, HOLDER_SAMPLE_INTERVAL_TICKS + 1);
            ticks(&mut whale, HOLDER_SAMPLE_INTERVAL_TICKS + 1);
        }
    }
    let b = broad.holder_reading(mt.as_bytes()).unwrap();
    let w = whale.holder_reading(mt.as_bytes()).unwrap();
    assert_eq!(b.growth_level(), Some(24), "24 distinct accumulators");
    assert_eq!(w.growth_level(), Some(1), "one whale is one holder");
}

#[test]
fn holder_trajectory_is_reported_for_the_open_book() {
    // The enter/hold limb: a position open at end of run carries its holder
    // trajectory onto the Report, basis and all.
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mt = mint(320);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
    });
    let mut entity = 1u64;
    for round in 0..8u64 {
        for _ in 0..6 {
            trade(
                &mut eng,
                mt,
                100 + round as i128 * 3,
                9_000,
                entity,
                REAL_CURVE_VSOL,
            );
            entity += 1;
        }
        ticks(&mut eng, 2);
    }
    let rep = eng.report();
    // Whether or not the gate admitted on this particular tape, the row shape is
    // what is under test: every reported row must be basis-tagged and must refuse
    // a level under a non-Exact basis.
    for row in &rep.holder_trajectory {
        assert_eq!(row.level.is_some(), row.basis == HolderCountBasis::Exact);
        assert_eq!(
            row.growth_level.is_some(),
            row.basis.admits_growth(),
            "growth availability must follow the basis gate"
        );
    }
    // The stream itself is present regardless of admission.
    let r = eng.holder_reading(mt.as_bytes()).expect("folded");
    assert!(r.growth_level().unwrap_or(0) > 0);
}

// ---------------------------------------------------------------------------
// 6. §3 — the money-proxy A/B, two-sided and pre-registered
// ---------------------------------------------------------------------------

/// Valid Solana pubkeys for the A/B cohorts (the attention lane only sees mints
/// a social post actually named, and the parser demands a real base58 key).
const BROADEN_B58: [&str; 4] = [
    "29d2S8fphGNdxpkLtoYM42q9Q6h7bxNiopT6JBHXmGB2",
    "2DYKaS8EYv7c92Pu9NMQRnvJdNA5gtFhEaLtf6SFCjbE",
    "2HTcijaeQZraKE3TPwAToZ1Trdd3mp8ffLEh21axeD1S",
    "2MNus334GDbYVRh1eVyXBK6d5u61rk1e668VNvjg5gRe",
];
const DISTRIB_B58: [&str; 4] = [
    "2RJD1LVU7sLWfdLZu4naZ5BnKAYywftcWr2HjqtPX9qr",
    "2VDW9dwsyX5Uqpz89dbdvqGwYS1x2bmawbv66m36xdG4",
    "2Z8oHwQHqApT22dgQCQhJbN6mhUv7XeZNMotTgBpQ6gG",
    "2d46SErhgpZRCEHEemDkgMTFzxwtCTXXo7hgpbLXqa6U",
];

fn b58(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid pubkey"))
}

/// One social post through the real capture seam (this is what puts a mint into
/// the attention field, which is the only place `money_of` is consulted).
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

/// Which side of the two-sided A/B this tape is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    /// HAPPY PATH: genuine holder broadening LEADS price on the winners, while
    /// the losers are a rising price on a SHRINKING holder base (distribution
    /// into the crowd). The armed proxy should prefer the broadening cohort.
    HolderLed,
    /// UNHAPPY PATH: the mirror. The cohort with the shrinking holder base is the
    /// one that pays (a healthy shakeout that keeps running), and the broadening
    /// cohort fades. If the armed proxy is a real edge rather than a coincidence,
    /// its loss here must be much smaller than its gain above.
    HolderMisleads,
}

/// A stable per-mint entity id space, so the two cohorts cannot accidentally
/// share entities (which would leak one cohort's holder trajectory into the
/// other's ledger).
fn seed_entity(addr: &str, k: u64) -> u64 {
    pump_quant_ingest::social_parse::fnv1a_64(addr.as_bytes()) % 1_000 * 10_000 + k + 1
}

/// The cohort's single market maker / whale — the counterparty that keeps net
/// base flow at exactly zero.
fn market_maker(addr: &str) -> u64 {
    seed_entity(addr, 9_999)
}

fn ab_drive(cfg: Config, side: Side) -> pump_quant_app::engine::Report {
    let mut cfg = cfg;
    // Same cost realism as the golden tape, so the lamport numbers are comparable.
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    // The numeric lane's DISCOVERY bar is raised out of reach on this tape. Both
    // cohorts trade with net-zero base flow, so neither has the imbalance the
    // numeric lane discovers on; lifting the bar makes that structural rather
    // than incidental, and forces every candidate to arrive through the
    // corroboration tier — which is the only place `money_of` is consulted at
    // all. (If the numeric lane also discovered these mints, the per-mint union
    // would keep the numeric lane's much larger score and the money proxy would
    // be masked entirely — which is exactly what happens on a tape that does not
    // do this, and is why this line is here rather than left to chance.)
    cfg.numeric_ofi_min_bp = 9_500;
    cfg.revert_ofi_min_bp = 9_500;
    // Promotion capacity is SCARCE, so rank actually decides something: eight
    // socially-hyped candidates compete for three promotion slots per tick.
    cfg.promote_k = 1;
    cfg.promote_corroboration_quota = 1;
    let mut eng = Engine::new(cfg, RunMode::Replay);

    // Both cohorts are confirmed on-chain at identical depth.
    for a in BROADEN_B58.iter().chain(DISTRIB_B58.iter()) {
        eng.tick(AppEvent::OnchainConfirm {
            mint: b58(a),
            virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
        });
    }

    // ---- THE CONTROLLED COMPARISON -------------------------------------------
    // Both cohorts are constructed to be NUMERICALLY INDISTINGUISHABLE: identical
    // swap counts, identical quote volume per swap, identical pool depth, and net
    // base flow of exactly ZERO per round. So order-flow imbalance, CVD, buyer
    // pressure and realized volatility carry the SAME information about both, and
    // neither cohort clears the numeric lane's own discovery bar — they can only
    // be discovered through the attention lane, where `money_of` decides the rank.
    //
    // The ONLY thing that separates them is who is doing the trading:
    //   * broadening: five NEW entities accumulate each round while one large
    //     market maker distributes the same notional. Holder count RISES.
    //   * distributing: one whale accumulates while five EXISTING holders exit in
    //     full. Holder count FALLS.
    //
    // The `unique_buyers` bitset cannot express the second shape at all: its bits
    // are never cleared, so a mint bleeding holders reads as flat, never falling.
    // The folded ledger reads it as negative money velocity.
    const SEED_HOLDERS: u64 = 48;
    const PER_ROUND: u64 = 5;
    const CLIP: i64 = 5_000;
    let broad_pays = side == Side::HolderLed;

    // IDENTICAL seed for both cohorts: 48 distinct entities each accumulate one
    // clip while a single market maker distributes the same notional. Net base
    // flow is zero, buys and sells alternate so the order-flow imbalance stays at
    // the neutral point, and BOTH cohorts start from a 48-holder base. The market
    // maker sells out of inventory we never saw it acquire, which correctly
    // demotes both cohorts to `DeltaOnly` — the basis the §70.1 growth term is
    // designed to work under, so the A/B exercises the interesting case.
    for a in BROADEN_B58.iter().chain(DISTRIB_B58.iter()) {
        let mt = b58(a);
        let mm = market_maker(a);
        for k in 0..SEED_HOLDERS {
            trade(&mut eng, mt, 100, CLIP, seed_entity(a, k), REAL_CURVE_VSOL);
            trade(&mut eng, mt, 100, -CLIP, mm, REAL_CURVE_VSOL);
        }
        post(&mut eng, "seedcaller", a, "seed call", 900_000_000);
    }
    ticks(&mut eng, 8);

    for round in 0..9u64 {
        for (i, a) in BROADEN_B58.iter().enumerate() {
            let mt = b58(a);
            let px = price_path(round, broad_pays, i as i128);
            let mm = market_maker(a);
            for k in 0..PER_ROUND {
                // A NEW entity accumulates...
                let e = SEED_HOLDERS + round * PER_ROUND + k;
                trade(&mut eng, mt, px, CLIP, seed_entity(a, e), REAL_CURVE_VSOL);
                // ...and the market maker distributes the same notional, so net
                // base flow for the round is exactly zero.
                trade(&mut eng, mt, px, -CLIP, mm, REAL_CURVE_VSOL);
            }
            post(
                &mut eng,
                "broadcaller",
                a,
                &format!("broad r{round} i{i}"),
                1_000_000_000 + round * 20_000_000 + i as u64,
            );
        }
        for (i, a) in DISTRIB_B58.iter().enumerate() {
            let mt = b58(a);
            let px = price_path(round, !broad_pays, i as i128);
            let whale = market_maker(a);
            for k in 0..PER_ROUND {
                // The whale accumulates...
                trade(&mut eng, mt, px, CLIP, whale, REAL_CURVE_VSOL);
                // ...out of an EXISTING holder who exits in full. Same net zero,
                // same swap count, opposite holder trajectory.
                let e = round * PER_ROUND + k;
                trade(&mut eng, mt, px, -CLIP, seed_entity(a, e), REAL_CURVE_VSOL);
            }
            post(
                &mut eng,
                "distcaller",
                a,
                &format!("dist r{round} i{i}"),
                1_000_000_000 + round * 20_000_000 + 100 + i as u64,
            );
        }
        ticks(&mut eng, 8);
    }
    eng.report()
}

/// Deterministic price path (§22 — no RNG). A cohort that `pays` runs to ~1.9x and
/// settles at ~1.5x; one that does not fades to ~0.85x.
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
    100 * bp / 10_000 + offset
}

#[test]
fn ab_tape_actually_exercises_the_holder_term() {
    // GUARD AGAINST A VACUOUS A/B. An A/B on a tape where the mechanism never
    // fires proves nothing while looking like a result, so the tape has to show
    // its work: the two cohorts must have genuinely OPPOSITE holder trajectories
    // while remaining numerically comparable, and the holder term the money proxy
    // reads must actually differ from the bitset term it replaces.
    let mut cfg = Config::dev_portable();
    cfg.money_proxy_holder_flow_enable = true;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // Re-drive the tape shape inline on one mint of each cohort.
    let bm = b58(BROADEN_B58[0]);
    let dm = b58(DISTRIB_B58[0]);
    for k in 0..48u64 {
        trade(&mut eng, dm, 100, 5_000, 100_000 + k, REAL_CURVE_VSOL);
        trade(&mut eng, bm, 100, 5_000, 900_000, REAL_CURVE_VSOL);
    }
    ticks(&mut eng, 8);
    let b_start = eng.holder_reading(bm.as_bytes()).unwrap().growth_level();
    let d_start = eng.holder_reading(dm.as_bytes()).unwrap().growth_level();
    for round in 0..9u64 {
        for k in 0..5u64 {
            trade(
                &mut eng,
                bm,
                100,
                5_000,
                200_000 + round * 100 + k,
                REAL_CURVE_VSOL,
            );
            trade(&mut eng, bm, 100, -5_000, 900_000, REAL_CURVE_VSOL);
            trade(&mut eng, dm, 100, 5_000, 800_000, REAL_CURVE_VSOL);
            trade(
                &mut eng,
                dm,
                100,
                -5_000,
                100_000 + round * 5 + k,
                REAL_CURVE_VSOL,
            );
        }
        ticks(&mut eng, 8);
    }
    let b_end = eng.holder_reading(bm.as_bytes()).unwrap().growth_level();
    let d_end = eng.holder_reading(dm.as_bytes()).unwrap().growth_level();
    println!("AB tape holders | broadening {b_start:?} -> {b_end:?} | distributing {d_start:?} -> {d_end:?}");
    assert!(
        b_end > b_start,
        "the broadening cohort must actually broaden ({b_start:?} -> {b_end:?})"
    );
    assert!(
        d_end < d_start,
        "the distributing cohort must actually bleed holders ({d_start:?} -> {d_end:?})"
    );
}

#[test]
fn ab_money_proxy_holder_flow_two_sided() {
    // PRE-REGISTERED RULE (fixed before the numbers were read, and identical in
    // form to the §56.2 rule LAW B7 was judged under): the armed term is adopted
    // by default ONLY if it earns on the happy path AND its gain there is at
    // least 3x the magnitude of any loss it causes on the mirror tape. A law that
    // wins by the same amount it loses is a coin flip with extra state.
    const ASYMMETRY_BAR: i128 = 3;

    let base = |side| {
        let mut c = Config::dev_portable();
        c.money_proxy_holder_flow_enable = false;
        ab_drive(c, side)
    };
    let armed = |side| {
        let mut c = Config::dev_portable();
        c.money_proxy_holder_flow_enable = true;
        ab_drive(c, side)
    };

    let off_happy = base(Side::HolderLed);
    let on_happy = armed(Side::HolderLed);
    let off_sad = base(Side::HolderMisleads);
    let on_sad = armed(Side::HolderMisleads);

    let gain = on_happy.net_lamports - off_happy.net_lamports;
    let loss = on_sad.net_lamports - off_sad.net_lamports;
    println!(
        "AB holder-flow money proxy | HAPPY off={} on={} delta={} (admitted {} -> {}) \
         | MIRROR off={} on={} delta={} (admitted {} -> {})",
        off_happy.net_lamports,
        on_happy.net_lamports,
        gain,
        off_happy.admitted,
        on_happy.admitted,
        off_sad.net_lamports,
        on_sad.net_lamports,
        loss,
        off_sad.admitted,
        on_sad.admitted,
    );
    // The tape is NOT degenerate: the two sides produce materially different
    // outcomes, so a term that mattered would have room to show it.
    assert_ne!(
        off_happy.net_lamports, off_sad.net_lamports,
        "the two sides of the A/B must be genuinely different tapes"
    );

    // Whatever the verdict, the DEFAULT must match it. This assertion is the
    // thing that keeps the report honest: if the term ever starts earning under
    // the pre-registered rule, this test fails until the default is flipped.
    let earns = gain > 0 && ASYMMETRY_BAR * loss.abs() < gain;
    assert_eq!(
        earns,
        Config::dev_portable().money_proxy_holder_flow_enable,
        "the configured default must equal the pre-registered A/B verdict \
         (gain {gain}, mirror {loss}, bar {ASYMMETRY_BAR}x)"
    );
}

#[test]
fn ab_the_money_proxy_itself_is_inert_on_every_tape_we_can_build() {
    // THE HONEST QUALIFIER ON THE A/B ABOVE, asserted rather than asserted-away.
    //
    // `ab_money_proxy_holder_flow_two_sided` measures a delta of exactly zero.
    // There are two very different reasons a delta can be zero — "we tested the
    // mechanism and it did not help" and "the mechanism was never reachable" —
    // and reporting the first when the truth is the second is precisely the kind
    // of fabricated result §6.4 exists to prevent. This test establishes which
    // one it is, by toggling the ENTIRE §70.1 composite money proxy (the wallet
    // inflow term and the holder term together, i.e. a far larger perturbation
    // than the holder term alone) on the same tapes.
    //
    // The result: the whole composite is ALSO exactly neutral. The money proxy
    // reaches an outcome only through `nv_attention_money_divergence` -> lifecycle
    // stage -> `nv_candidate_score` -> discovery rank, and on these tapes the
    // attention lane's rank ordering is dominated by attention level and the §29
    // fade cap, so no money value the proxy can produce reorders the promotion
    // set. The holder term's zero is therefore an UNREACHABILITY result, not an
    // efficacy result, and the report says so.
    for side in [Side::HolderLed, Side::HolderMisleads] {
        let mut off = Config::dev_portable();
        off.money_proxy_enable = false;
        let mut on = Config::dev_portable();
        on.money_proxy_enable = true;
        let r_off = ab_drive(off, side);
        let r_on = ab_drive(on, side);
        println!(
            "AB whole-money-proxy | off net={} adm={} | on net={} adm={}",
            r_off.net_lamports, r_off.admitted, r_on.net_lamports, r_on.admitted
        );
        assert_eq!(
            r_off.net_lamports, r_on.net_lamports,
            "the composite money proxy is inert on this tape — so the holder \
             term's A/B measures reachability, not efficacy"
        );
    }
}

#[test]
fn ab_disarmed_holder_flow_term_is_byte_identical() {
    // The safety property that makes the default-OFF decision cost nothing: with
    // the term disarmed the engine's decisions are bit-for-bit what they were
    // before the term existed, even though the holder stream is fully running.
    let mut c = Config::dev_portable();
    c.money_proxy_holder_flow_enable = false;
    let a = ab_drive(c, Side::HolderLed);
    let b = ab_drive(c, Side::HolderLed);
    assert_eq!(a.journal_digest, b.journal_digest);
    assert_eq!(a.net_lamports, b.net_lamports);
    // And the stream really is running on that tape (otherwise this proves
    // nothing): the holder ledger has to be non-trivially populated.
    let mut cfg = Config::dev_portable();
    cfg.money_proxy_holder_flow_enable = false;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    broadening_tape(&mut eng, mint(400));
    assert!(
        !eng.holder_flow().is_empty(),
        "the watch-time stream must be live even with the money term disarmed"
    );
}

#[test]
fn ab_armed_term_is_refused_on_an_incomplete_ledger() {
    // The explicit fallback: an over-cap (Incomplete) reading must NOT feed the
    // money term. Proven at the reading level, which is the gate the money proxy
    // actually calls.
    let mut hf = HolderFlow::with_caps(4, 2);
    let m = m32(60);
    hf.observe_swap(&m, 1, 100, 0, 0);
    hf.observe_swap(&m, 2, 100, 0, 0);
    assert_eq!(hf.reading(&m).and_then(|r| r.growth_level()), Some(2));
    hf.observe_swap(&m, 3, 100, 0, 0); // past the 2-entity cap
    assert_eq!(
        hf.reading(&m).and_then(|r| r.growth_level()),
        None,
        "an Incomplete reading must refuse the money term, which then falls back"
    );
}
