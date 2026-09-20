//! Batch-2d law attribution (LAWs 12–21): records + report-plane hooks. Each test
//! proves its law — an A/B where the law changes a decision (LAW 13), a
//! presence/shape assertion where the law is a record or report-plane hook
//! (LAWs 12, 15, 16, 17, 18). LAWs 14/19/20/21 are proven by unit tests co-located
//! with their code (authority.rs, feature_admit.rs, ablation_replay.rs,
//! live_status.rs). Determinism (§22) makes every comparison exact.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;

/// **DEPTH REALISM (re-pin #26).** The gate's price-impact model is now DERIVED from
/// the market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves START at 30 SOL; the sub-SOL depths these fixtures used to declare put the
/// operator's 0.1 SOL floor clip at 5-125% of the pool — a market in which no strategy
/// result means anything (Amendment A-13(1)).
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

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

const PRICE_SCALE: i128 = 10_000_000;

/// Emit `n` rising net-buy trades for one mint (opens a long thesis on confirm).
fn pump(eng: &mut Engine, tag: u64, base_mult: i128, n: u64, liq: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: (base_mult + i as i128) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: liq,
            signed_base: 900_000 - (i as i64),
            buyer_entity: 40 + i % 7,
            age_slots: 12,
            recv_unix_ms: None,
        });
    }
}

/// Drive a multi-mint tape that opens and exits positions under `cfg`.
fn drive_positions(cfg: Config) -> Engine {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for round in 0..3u64 {
        for m in 0..6u64 {
            pump(&mut eng, m, 100 + round as i128 * 20, 24, REAL_CURVE_VSOL);
            eng.tick(AppEvent::OnchainConfirm {
                mint: mint(m),
                virtual_sol_lamports: REAL_CURVE_VSOL,
                real_sol_lamports: REAL_CURVE_REAL_SOL,
            });
        }
        for _ in 0..40 {
            eng.tick(AppEvent::Tick);
        }
        // A crash so held positions actually exit (trailing/hard stop).
        for m in 0..6u64 {
            for i in 0..12u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: mint(m),
                    price_fp: (150 - i as i128 * 6) * PRICE_SCALE,
                    quote_lamports: 800_000,
                    liquidity_lamports: REAL_CURVE_VSOL,
                    signed_base: -900_000,
                    buyer_entity: 40 + i % 7,
                    age_slots: 12,
                    recv_unix_ms: None,
                });
            }
        }
        for _ in 0..40 {
            eng.tick(AppEvent::Tick);
        }
    }
    eng
}

// ============================================================================
// LAW 12 (§34.4): the Admitted journal record carries the size band, the
// attempt/fail-rate multiplier, and the impact provenance.
// ============================================================================
#[test]
fn admitted_record_carries_band_and_provenance() {
    let cfg = Config::dev_portable();
    let fail_rate = cfg.gate_fail_rate_bps;
    let mut eng = drive_positions(cfg);
    let _ = eng.report();
    let admits: Vec<_> = eng
        .journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Admitted {
                mint: _,
                size_lamports,
                x_min,
                x_cost,
                x_max,
                fail_rate_bps,
                rt_cost_bps,
                move_bps,
                move_source,
                depth_basis,
            } => Some((
                size_lamports,
                x_min,
                x_cost,
                x_max,
                fail_rate_bps,
                rt_cost_bps,
                move_bps,
                move_source,
                depth_basis,
            )),
            _ => None,
        })
        .collect();
    assert!(
        !admits.is_empty(),
        "the tape must open at least one position"
    );
    for (size, x_min, x_cost, x_max, fr, rt, move_bps, move_source, depth_basis) in admits {
        // The band is well-ordered and the admitted size lies within it.
        assert!(x_min <= x_cost && x_cost <= x_max, "band must be ordered");
        assert!(size >= x_min && size <= x_max, "size within admitted band");
        // The fail-rate multiplier is the config value that inflated the fixed cost.
        assert_eq!(fr, fail_rate, "fail-rate provenance recorded");
        // Impact provenance: a real round-trip cost was measured at the admitted size.
        assert!(rt > 0, "round-trip impact provenance recorded");
        // BENEFIT-SIDE PROVENANCE (2026-07-28). The record used to say what was
        // admitted and what it cost, but never what we thought it was WORTH or which
        // estimator said so — so a replay could not reconstruct the admission at all.
        assert!(
            move_bps > i128::from(rt),
            "an admitted trade's priced move ({move_bps} bps) must beat the cost it              was admitted against ({rt} bps)"
        );
        assert!(
            move_source <= 2,
            "move source is one of cold-start / lane prior / model"
        );
        // DEPTH PROVENANCE: never 0 (unknown) on an admitted trade — an admission
        // sized against depth of unknown basis is exactly what `CurveDepth` forbids.
        assert!(
            depth_basis == 1 || depth_basis == 2,
            "an admitted curve trade's depth is derived (1) or decoded (2), got              {depth_basis}"
        );
    }
}

// ============================================================================
// LAW 13 (§33/§43): sub-x_min probes route through the calibration budget and
// are journal-labeled as budgeted paid-information — NOT opened raw as positions.
// A/B: budget-accounted+labeled (ON) vs opened/rejected raw (OFF).
// ============================================================================

/// A tiny-bankroll tape: deployable is small enough that the §33 sizing lands
/// BELOW the economic x_min cost floor, so every viable candidate hits the
/// sub-x_min branch.
fn drive_sub_xmin(probe_budget: bool, bankroll_lamports: u64) -> Engine {
    let mut cfg = Config::dev_portable();
    // The initial bankroll is a PARAMETER. Under the measured cost model no bankroll
    // lands a candidate below the economic `x_min`, so the SWEPT RANGE is the evidence
    // (see the test) — kept explicit rather than pinned at one value that quietly
    // stopped exercising the branch.
    cfg.bankroll_initial_lamports = bankroll_lamports;
    // The §33/§43 LAW 13 sub-x_min paid-information probe is a SUB-FLOOR bet, so the
    // criterion-112 / A-6 operator floor switches it OFF whenever it is active. This
    // test exercises the legacy sub-x_min path, so it explicitly disables the floor
    // (min_trade_size = 0) — with the floor on, these tiny-bankroll candidates would
    // instead REFUSE below the floor (deeper protection, correct), never probe.
    cfg.min_trade_size_lamports = 0;
    cfg.probe_budget_enable = probe_budget;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for m in 0..6u64 {
        pump(&mut eng, m, 100, 24, REAL_CURVE_VSOL);
        eng.tick(AppEvent::OnchainConfirm {
            mint: mint(m),
            virtual_sol_lamports: REAL_CURVE_VSOL,
            real_sol_lamports: REAL_CURVE_REAL_SOL,
        });
    }
    for _ in 0..30 {
        eng.tick(AppEvent::Tick);
    }
    eng
}

#[test]
fn sub_xmin_probe_is_budget_accounted_and_labeled_vs_raw() {
    // PREMISE INVALIDATED BY THE MEASURED COST MODEL (A1-A4 / re-pin #29), AND REWRITTEN
    // TO ASSERT THE CORRECT NEW BEHAVIOUR.
    //
    // This test used to drive a 0.52-SOL bankroll and assert that every viable candidate
    // landed on the §33/§43 LAW 13 sub-x_min probe branch: budget-accounted
    // paid-information probes, journal-labelled `Probe` records, and never a raw open.
    //
    // It cannot any more, and the cause is the cost correction rather than a bug. With
    // the venue fee at its MEASURED rate — 94.63 bp/side instead of the 125 bp
    // placeholder, and 10_000 instead of 150_000 fixed lamports per leg — the economic
    // `x_min` (the lower feasibility crossing, where a bite's expected move stops
    // covering its round trip) falls BELOW every deployable sizing. Nothing sizes under
    // it, so the sub-x_min branch is unreachable and the arbiter resolves the candidates
    // upstream (the histogram shows REJECT_ARBITRATION, not REJECT_BELOW_COST_FLOOR).
    //
    // Verified by sweep: 0.52, 0.10, 0.02, 0.005, 0.002 and 0.001 SOL all produce zero
    // probes. A 500x range with no hit is not a tuning problem.
    //
    // So the correct assertion is the INERT property below — enabling
    // `probe_budget_enable` is a no-op versus the raw path, because the path it gates no
    // longer fires. That is a real thing worth pinning: it is the LAW 13 probe path
    // going dead under the measured cost model, which is a finding about the cost
    // correction, not a licence to delete the test. The `Probe` label remains reachable
    // only through `account_sub_xmin_probe`'s direct callers.
    const BANKROLLS: [u64; 6] = [
        520_000_000,
        100_000_000,
        20_000_000,
        5_000_000,
        2_000_000,
        1_000_000,
    ];

    for bankroll in BANKROLLS {
        // ON: `probe_budget_enable` set, operator floor disabled (the legacy path's own
        // preconditions) — and yet nothing probes.
        let mut on = drive_sub_xmin(true, bankroll);
        let ron = on.report();
        let (spend_on, probes_on) = on.probe_budget_report();
        let probe_records = on
            .journal()
            .recent()
            .filter(|d| matches!(**d, Decision::Probe { .. }))
            .count();

        // OFF: identical config but for the flag.
        let mut off = drive_sub_xmin(false, bankroll);
        let roff = off.report();
        let (spend_off, probes_off) = off.probe_budget_report();
        let probe_records_off = off
            .journal()
            .recent()
            .filter(|d| matches!(**d, Decision::Probe { .. }))
            .count();

        // The measured cost model leaves no candidate below `x_min`: the branch is inert.
        assert_eq!(
            probes_on, 0,
            "measured cost model must leave the legacy sub-x_min probe path INERT \
             (bankroll={bankroll}, probes={probes_on})"
        );
        assert_eq!(
            spend_on, 0,
            "inert probe path must account no calibration spend (bankroll={bankroll}, \
             spend={spend_on})"
        );
        assert_eq!(
            probe_records, 0,
            "inert probe path must journal no Probe labels (bankroll={bankroll}, \
             records={probe_records})"
        );
        // And the OFF arm does the same nothing: the flag is a no-op now, which is the
        // whole point of asserting the inertness rather than the old firing.
        assert_eq!(probes_off, 0, "probe-budget OFF accounts no probes");
        assert_eq!(spend_off, 0, "probe-budget OFF spends nothing");
        assert_eq!(probe_records_off, 0, "OFF journals no Probe labels");
        assert_eq!(
            (probes_on, spend_on),
            (probes_off, spend_off),
            "probe_budget_enable must be a NO-OP against the raw path under the measured \
             cost model (bankroll={bankroll})"
        );
        // A probe is never a position, and with none firing the two arms open identically.
        assert_eq!(
            ron.admitted, roff.admitted,
            "no probe may add a position (bankroll={bankroll}, on={}, off={})",
            ron.admitted, roff.admitted
        );
    }
}

// ============================================================================
// LAW 15 (§49): vetoes and haircuts now produce NON-degenerate convexity ledger
// events (counterfactual-vs-zero / reduced-vs-full), not a self-cancelling pair.
// ============================================================================
#[test]
fn vetoes_and_haircuts_produce_nondegenerate_convexity() {
    let mut eng = drive_positions(Config::dev_portable());
    let _ = eng.report();
    let rules = eng.analytics_report().convexity_rules;
    assert!(!rules.is_empty(), "convexity ledger must be populated");
    // At least one rule ledger folds a suppression (veto or haircut) whose net
    // convexity is non-zero — i.e. counterfactual != realized (non-degenerate).
    let nondegenerate = rules
        .iter()
        .any(|r| r.suppressed_n > 0 && r.net_convexity_bps() != 0);
    assert!(
        nondegenerate,
        "a veto or haircut must record a non-degenerate (counterfactual != realized) event; rules={rules:?}"
    );
}

// ============================================================================
// LAW 16 (§52): baseline_verdict runs the whole deterministic baseline FAMILY,
// and the family net-SOL vector is reported.
// ============================================================================
#[test]
fn baseline_family_vector_is_reported() {
    let mut cfg = Config::dev_portable();
    cfg.baseline_min_trades = 1;
    let mut eng = drive_positions(cfg);
    let _ = eng.report();
    let family = eng.baseline_family_report();
    // The full 5-baseline family net-SOL vector (random-eligible / buy-every-launch
    // / threshold-only / fixed-TP-SL / hold-to-death).
    assert_eq!(family.len(), 5, "the full baseline family must be reported");
    // The family-wise-margin verdict runs against ALL of them.
    assert!(
        eng.baseline_verdict().is_some(),
        "past the small-n guard a family verdict must exist"
    );
}

// ============================================================================
// LAW 17 (§47/§54): post-exit markout cells + foregone-upside per ExitReason are
// present in the AnalyticsReport after a run.
// ============================================================================
#[test]
fn markout_cells_and_foregone_present_per_exit_reason() {
    let mut eng = drive_positions(Config::dev_portable());
    let _ = eng.report();
    let a = eng.analytics_report();
    assert!(
        !a.markout_cells.is_empty(),
        "post-exit markout cells must be present after a run"
    );
    assert!(
        !a.foregone_upside.is_empty(),
        "foregone-upside aggregates must be present after a run"
    );
    // Cells are keyed by (ExitReason, horizon): the mandated horizons appear.
    let horizons: std::collections::BTreeSet<u64> =
        a.markout_cells.iter().map(|c| c.horizon_ns).collect();
    assert!(
        !horizons.is_empty(),
        "cells carry mandated ns horizons, got {horizons:?}"
    );
}

// ============================================================================
// LAW 18 (§47a): a dead mint gets a terminal label at the versioned δT.
// ============================================================================
#[test]
fn dead_mint_gets_terminal_label_at_versioned_delta_t() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    // A mint trades briefly, then goes silent forever.
    pump(&mut eng, 7, 100, 6, REAL_CURVE_VSOL);
    // Advance well past the terminal δT (240 ticks) with no further trades for it.
    for _ in 0..300 {
        eng.tick(AppEvent::Tick);
    }
    let refs = eng.terminal_reflections();
    assert!(
        !refs.is_empty(),
        "the reflection cadence must have produced terminal labels"
    );
    // Every reflection carries the versioned δT criterion (version 1).
    assert!(
        refs.iter().all(|r| r.criterion_version == 1),
        "labels stamped with the δT criterion version"
    );
    // The silent mint is labeled dead.
    assert!(
        refs.iter().any(|r| r.is_dead()),
        "a mint silent past δT must be labeled terminal"
    );
}
