//! §21.7/§70.1 the **concentration parallel stream**: coverage measurement and
//! stream behaviour.
//!
//! # The question this file answers
//!
//! The whole design rests on one empirical claim: **concentration LEVEL coverage is
//! thin and concentration TRAJECTORY coverage is not**, because the first needs
//! `HolderCountBasis::Exact` and the second only needs `DeltaOnly`. That claim is
//! the single biggest open risk in this family and it has never been quantified.
//! This file quantifies it, on the engine's own golden tape and on purpose-built
//! tapes that isolate each basis path.
//!
//! Coverage numbers here are **reported, not barred**. How often a real market is
//! seen from its creation event is a property of the ingestion plane and of the
//! world, not of this code; a threshold asserted against it would be measuring the
//! test tape. What IS asserted is the structural discipline: a refusal carries no
//! number, the trajectory reads where the level refuses, and neither one moves a
//! decision.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::holder_concentration::{internal_concentration_of, InternalUnknown};
use pump_quant_app::holder_flow::HolderCountBasis;
use pump_quant_brain::concentration::TrajectoryDirection;
use pump_quant_domain::ids::Mint;

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

/// One swap, at the engine's public event surface.
fn trade(e: &mut Engine, m: Mint, entity: u64, signed_base: i64) {
    e.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: 1_000_000_000,
        quote_lamports: 400_000,
        liquidity_lamports: 120_000_000,
        signed_base,
        buyer_entity: entity,
        age_slots: 12,
    });
}

/// A creation sighting — the ONLY path to an `Exact` basis.
fn creation(e: &mut Engine, m: Mint, slot: u64) {
    e.tick(AppEvent::TokenMetadata {
        mint: m,
        category_id: 0,
        taxonomy_version: 1,
        creator: 42,
        slot,
    });
}

/// Idle ticks, so information time advances past the estimator's minimum interval.
fn idle(e: &mut Engine, n: u64) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
    }
}

// ===========================================================================
// COVERAGE MEASUREMENT
// ===========================================================================

/// **THE COVERAGE MEASUREMENT.** How many mints reach each holder basis, and
/// therefore how many can carry a concentration LEVEL versus a TRAJECTORY.
///
/// Three cohorts on one engine, all fed the same flow:
/// * **mid-life** — swaps arrive with no prior creation sighting (`DeltaOnly`);
/// * **creation-first** — a `TokenMetadata` event precedes the first swap
///   (`Exact`);
/// * **falsified** — creation-first, then a pre-window seller proves an untracked
///   holder existed, demoting `Exact -> DeltaOnly` permanently.
///
/// The third cohort is the one people forget: an `Exact` claim is not durable. It
/// is a claim, and the ledger disproves it whenever the evidence says so.
#[test]
fn measure_basis_coverage_across_the_three_discovery_shapes() {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
    const PER_COHORT: u64 = 12;
    const ENTITIES: u64 = 30;

    let midlife: Vec<Mint> = (0..PER_COHORT).map(|i| mint(1_000 + i)).collect();
    let creation_first: Vec<Mint> = (0..PER_COHORT).map(|i| mint(2_000 + i)).collect();
    let falsified: Vec<Mint> = (0..PER_COHORT).map(|i| mint(3_000 + i)).collect();

    for m in creation_first.iter().chain(falsified.iter()) {
        creation(&mut e, *m, 1);
    }
    for m in midlife.iter().chain(&creation_first).chain(&falsified) {
        for entity in 0..ENTITIES {
            trade(&mut e, *m, entity, 500_000);
        }
    }
    // The falsifier: an entity we never saw buy, selling. Provably a pre-window
    // holder, so the `Exact` claim was wrong and the basis is demoted.
    for m in &falsified {
        trade(&mut e, *m, 9_999, -100_000);
    }

    let count = |mints: &[Mint]| {
        let mut exact = 0u32;
        let mut delta = 0u32;
        let mut level_known = 0u32;
        let mut internal_known = 0u32;
        for m in mints {
            match e.holder_reading(m.as_bytes()).map(|r| r.basis()) {
                Some(HolderCountBasis::Exact) => exact += 1,
                Some(HolderCountBasis::DeltaOnly) => delta += 1,
                _ => {}
            }
            if e.holder_concentration(m.as_bytes()).is_known() {
                level_known += 1;
            }
            if internal_concentration_of(e.holder_flow(), m.as_bytes())
                .reading()
                .is_some()
            {
                internal_known += 1;
            }
        }
        (exact, delta, level_known, internal_known)
    };

    let mid = count(&midlife);
    let cre = count(&creation_first);
    let fal = count(&falsified);
    println!("EXACT-BASIS COVERAGE (n={PER_COHORT} per cohort, {ENTITIES} entities each)");
    println!("  cohort           exact delta level_known internal_known");
    println!(
        "  mid-life         {:5} {:5} {:11} {:14}",
        mid.0, mid.1, mid.2, mid.3
    );
    println!(
        "  creation-first   {:5} {:5} {:11} {:14}",
        cre.0, cre.1, cre.2, cre.3
    );
    println!(
        "  falsified-exact  {:5} {:5} {:11} {:14}",
        fal.0, fal.1, fal.2, fal.3
    );

    // The structural claims, asserted (the counts above are reported).
    assert_eq!(mid.0, 0, "a mid-life mint can never be Exact");
    assert_eq!(mid.2, 0, "…so it can never carry a concentration LEVEL");
    assert_eq!(
        mid.3, PER_COHORT as u32,
        "…but its tracked cohort's internal shape IS observable"
    );
    assert_eq!(
        cre.0, PER_COHORT as u32,
        "a creation-first mint is Exact and can carry a level"
    );
    assert_eq!(cre.2, PER_COHORT as u32);
    assert_eq!(
        fal.0, 0,
        "an Exact claim is FALSIFIABLE — a pre-window seller demotes it"
    );
    assert_eq!(fal.2, 0, "…and the level goes with it");
    assert_eq!(
        fal.3, PER_COHORT as u32,
        "…while the internal statistic survives the demotion"
    );
}

/// **THE COVERAGE MEASUREMENT ON THE ENGINE'S OWN GOLDEN TAPE.**
///
/// The golden tape feeds no `TokenMetadata`, so no mint on it can be `Exact` and
/// the LEVEL coverage there is exactly zero. That is not a defect of the tape — it
/// is the realistic case, because in production a mint is `Exact` only when the
/// creation event is decoded before the first swap AND no pre-window seller ever
/// appears. Reported so nobody has to guess.
#[test]
fn measure_level_coverage_on_a_golden_style_tape() {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
    const N: u64 = 24;
    let mints: Vec<Mint> = (0..N).map(|i| mint(4_000 + i)).collect();
    for m in &mints {
        for entity in 0..30u64 {
            trade(&mut e, *m, entity, 500_000);
        }
    }
    let level_known = mints
        .iter()
        .filter(|m| e.holder_concentration(m.as_bytes()).is_known())
        .count();
    let internal_known = mints
        .iter()
        .filter(|m| {
            internal_concentration_of(e.holder_flow(), m.as_bytes())
                .reading()
                .is_some()
        })
        .count();
    println!(
        "GOLDEN-STYLE (no creation sightings): mints={N} level_known={level_known} \
         internal_known={internal_known} level_coverage_bps={} internal_coverage_bps={}",
        level_known * 10_000 / N as usize,
        internal_known * 10_000 / N as usize
    );
    assert_eq!(
        level_known, 0,
        "a tape with no creation sightings has ZERO level coverage — this is the \
         number that makes concentration unfit as a fingerprint dimension"
    );
    assert_eq!(
        internal_known, N as usize,
        "and the derivative has FULL coverage on the same tape — which is why it \
         is the half that got streamed"
    );
}

/// The refusal REASON is preserved end to end, so an operator can tell "we did not
/// look" from "we looked and the basis forbade it".
#[test]
fn every_refusal_carries_its_reason_and_no_number() {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = mint(5_001);
    // Untracked: nothing folded at all.
    assert_eq!(
        format!(
            "{:?}",
            e.holder_concentration(m.as_bytes()).unknown_reason()
        ),
        "Some(Untracked)"
    );
    assert_eq!(
        internal_concentration_of(e.holder_flow(), m.as_bytes()).unknown_reason(),
        Some(InternalUnknown::Untracked)
    );
    // Thin: folded, but too few entities for a shape to have dynamic range.
    for entity in 0..5u64 {
        trade(&mut e, m, entity, 500_000);
    }
    assert_eq!(
        internal_concentration_of(e.holder_flow(), m.as_bytes()).unknown_reason(),
        Some(InternalUnknown::ThinLedger)
    );
    assert_eq!(
        internal_concentration_of(e.holder_flow(), m.as_bytes()).reading(),
        None,
        "a refusal yields no number, by any route"
    );
    // Delta-only: enough entities, wrong basis for a LEVEL.
    for entity in 5..30u64 {
        trade(&mut e, m, entity, 500_000);
    }
    assert_eq!(
        format!(
            "{:?}",
            e.holder_concentration(m.as_bytes()).unknown_reason()
        ),
        "Some(DeltaOnlyBasis)"
    );
    assert!(
        internal_concentration_of(e.holder_flow(), m.as_bytes())
            .reading()
            .is_some(),
        "…and the derivative reads on exactly that basis"
    );
}

// ===========================================================================
// THE STREAM
// ===========================================================================

/// A concentration TRAJECTORY exists on the engine, on a delta-only ledger,
/// without any operator action — i.e. concentration really is a maintained stream
/// now and not a point reading taken at admit.
#[test]
fn the_engine_maintains_a_concentration_trajectory_on_a_delta_only_ledger() {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = mint(6_001);
    // A broad cohort accumulates evenly: 30 distinct entities, so the ledger
    // clears the shape floor and the series starts.
    for entity in 0..30u64 {
        trade(&mut e, m, entity, 500_000);
        idle(&mut e, 10);
    }
    // Then one entity takes over, in steps spaced well past the estimator's 1 s
    // minimum interval so every step lands its own sample.
    //
    // NOTE the trajectory is the change between the two most recent
    // adequately-spaced samples — it is a CURRENT rate, not a summary of the whole
    // history — so the fixture ends ON the move rather than after it. A long flat
    // tail would correctly read `Flat`, which is the estimator working, not a bug.
    for _ in 0..8 {
        trade(&mut e, m, 0, 5_000_000);
        idle(&mut e, 10);
    }

    assert_eq!(
        e.holder_reading(m.as_bytes()).map(|r| r.basis()),
        Some(HolderCountBasis::DeltaOnly),
        "the premise: this is the basis an absolute share refuses"
    );
    let rows = e.report().holder_trajectory;
    println!(
        "TRAJECTORY rows_on_open_book={} (report rows exist only for OPEN positions)",
        rows.len()
    );
    // The plane itself, read through the engine's own admit-time path: build a
    // trajectory verdict the same way `brain_entry_at_admit` does.
    let traj = e.concentration_trajectory(m.as_bytes());
    println!(
        "TRAJECTORY mint=6001 verdict={:?} rate_bps={:?}",
        traj,
        e.concentration_rate_bps(m.as_bytes())
    );
    assert!(
        traj.is_known(),
        "a delta-only ledger must still yield a trajectory"
    );
    assert_eq!(
        traj.shape().map(|s| s.direction()),
        Some(TrajectoryDirection::Concentrating),
        "one entity absorbing the float is CONCENTRATING"
    );
    assert!(
        e.concentration_rate_bps(m.as_bytes()).unwrap_or(0) > 0,
        "and the signed rate agrees with the direction"
    );
}

/// A single reading is not a trajectory: the stream refuses with
/// `InsufficientHistory` rather than reporting `Flat`, which would be a fabricated
/// measurement of "no change" (§6.4).
#[test]
fn a_fresh_mint_has_no_trajectory_rather_than_a_flat_one() {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = mint(6_002);
    for entity in 0..30u64 {
        trade(&mut e, m, entity, 500_000);
    }
    let traj = e.concentration_trajectory(m.as_bytes());
    assert!(!traj.is_known(), "one sample is not a change");
    assert_eq!(traj.shape(), None);
    assert_eq!(e.concentration_rate_bps(m.as_bytes()), None);
}

/// The stream is REPLAY-DETERMINISTIC: the same events reproduce the same
/// trajectory, bit for bit (§22/§54).
#[test]
fn the_trajectory_stream_is_replay_deterministic() {
    let run = || {
        let mut e = Engine::new(Config::dev_portable(), RunMode::Replay);
        let m = mint(6_003);
        for entity in 0..40u64 {
            trade(&mut e, m, entity, 500_000);
            idle(&mut e, 6);
        }
        for _ in 0..4 {
            trade(&mut e, m, 0, 9_000_000);
            idle(&mut e, 6);
        }
        (
            e.concentration_trajectory(m.as_bytes()),
            e.concentration_rate_bps(m.as_bytes()),
        )
    };
    assert_eq!(run(), run(), "same events -> identical trajectory");
}
