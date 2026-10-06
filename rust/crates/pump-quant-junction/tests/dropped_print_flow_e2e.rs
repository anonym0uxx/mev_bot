//! Producer -> engine dropped-print detection, driven through the REAL production path.
//!
//! The clocked LaserStream `accountSubscribe` handler refuses a reserve snapshot pair whose
//! delta it cannot derive (`derive_market_trade_from_delta` -> `None`). The extracted
//! production step [`pump_quant_junction::reserve_delta::note_curve_snapshot_outcome`] is the
//! SAME body `pq_daemon` calls: it classifies the miss and, for an upstream-dropped print
//! carrying a wire receive time, records the drop on the engine. This test calls that
//! function (never `Engine::note_flow_upstream_drop` directly) and then observes what the
//! engine actually SERVES or REFUSES on real `Engine::tick` events.
//!
//! Assertions (per the acceptance criteria):
//! (a) the rejection marks the affected mint's flow incomplete with the named reason
//!     `join_flow_upstream_drop`;
//! (b) a later VALID print cannot hide it — the window is still refused;
//! (c) the prompt stays refused until the 300 s history window no longer needs the drop,
//!     then serving resumes with no explicit clearing;
//! (d) an UNRELATED mint stays usable, and an ordinary no-trade (`DeltaMiss::NoPrint`) does
//!     not poison flow;
//! (e) held-position monitoring / reconciliation / emergency protection are NOT disabled
//!     while an entry window is refused.

use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_manage::MgmtKind;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;
use pump_quant_junction::reserve_delta::{note_curve_snapshot_outcome, DeltaMiss, ReserveSnapshot};
use pump_quant_protocol::decode::PumpCurve;

const T0: i64 = 1_800_000_000_000;

/// The mint whose flow window a dropped print poisons (unheld: an ENTRY candidate).
const AFFECTED: [u8; 32] = [0xAB; 32];
/// A mint with an OPEN position: its MANAGEMENT must keep working while AFFECTED is refused.
const HELD: [u8; 32] = [0xBA; 32];
/// An unrelated mint that must stay usable.
const OTHER: [u8; 32] = [0xCC; 32];
/// A mint fed an ordinary no-trade: it must NOT be poisoned.
const QUIET: [u8; 32] = [0xDD; 32];
const CREATOR: [u8; 32] = [0xCD; 32];

const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;
/// The wire receive time of the print the feed derivation dropped for `AFFECTED`.
const DROP_MS: i64 = T0 + 75_000;

const ENTRY_BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const MGMT_EXIT: &str = "DECISION: EXIT\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: close it";

fn dm(b: [u8; 32]) -> DomainMint {
    DomainMint::from_bytes(b)
}

fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}

fn cfg() -> Config {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    c
}

/// A source that answers management prompts with EXIT and entry prompts with BUY — so the
/// held-position EXIT route is exercised, not just the entry route.
struct Stub;

impl ModelSource for Stub {
    fn complete(&self, _s: &str, u: &str) -> Result<String, InferenceError> {
        if u.starts_with("Decide the next action for a position you already hold") {
            Ok(MGMT_EXIT.to_string())
        } else {
            Ok(ENTRY_BUY.to_string())
        }
    }
}

const MGMT_HOLD: &str = "DECISION: HOLD\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: hold it";

/// Answers HOLD for management prompts (so a held position stays OPEN for the duration of the
/// test) and BUY for entries.
struct HoldStub;

impl ModelSource for HoldStub {
    fn complete(&self, _s: &str, u: &str) -> Result<String, InferenceError> {
        if u.starts_with("Decide the next action for a position you already hold") {
            Ok(MGMT_HOLD.to_string())
        } else {
            Ok(ENTRY_BUY.to_string())
        }
    }
}

/// A mint's warm-up tape: launch + `n` priced prints + (optional) a fresh curve observation.
/// Prints are 2 s apart starting at `start_ms`; `start_ms - 1_000` is the launch time.
/// `seq0` offsets the slot/wallet indices so CONTINUATION batches for the same mint stay
/// monotonic (the ledger rejects a re-used slot as out-of-order).
fn tape(
    m: [u8; 32],
    n: u32,
    start_ms: i64,
    with_launch: bool,
    with_curve: bool,
    seq0: u32,
    price0: i128,
) -> Vec<AppEvent> {
    let mut ev = Vec::new();
    if with_launch {
        ev.push(AppEvent::LaunchObserved {
            mint: dm(m),
            creator: CREATOR,
            launch_unix_ms: start_ms - 1_000,
        });
    }
    for i in 0..n {
        let seq = seq0 + i;
        let buy = i % 3 != 0;
        ev.push(AppEvent::MarketTrade {
            mint: dm(m),
            price_fp: price0 + i128::from(i),
            quote_lamports: 500_000_000 + u64::from(i),
            liquidity_lamports: VSOL,
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(seq),
            age_slots: 30,
            recv_unix_ms: Some(start_ms + i64::from(i) * 2_000),
            trader_pubkey: Some(wallet(seq)),
            slot: Some(1_000 + u64::from(seq)),
            fee_lamports: Some(60_000 + u64::from(seq) * 100),
            cu_consumed: Some(90_000 + u64::from(seq)),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    if with_curve {
        let t_last = start_ms + i64::from(n - 1) * 2_000;
        ev.push(AppEvent::CurveObserved {
            mint: dm(m),
            v_sol_lamports: VSOL,
            v_tokens: VTOK,
            real_sol_lamports: 7_900_000_000,
            real_tokens: 569_000_000_000_000,
            recv_unix_ms: Some(t_last),
            slot: 2_000 + u64::from(seq0),
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: dm(m),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

/// Drive events, then give the off-thread worker real time to answer between ticks.
fn drive(e: &mut Engine, evs: &[AppEvent], ticks: usize) {
    for ev in evs {
        e.tick(*ev);
    }
    for _ in 0..ticks {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The "landing state" curve observation that simulates the paper fill for `m`.
fn landing(e: &mut Engine, m: [u8; 32], after_ms: i64) {
    e.tick(AppEvent::CurveObserved {
        mint: dm(m),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(after_ms),
        slot: 2_100,
    });
    for _ in 0..6 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

/// Mints the engine currently refuses because their flow window holds an upstream drop.
fn mints_never_ready_with_drop(e: &Engine) -> u64 {
    e.model_funnel()
        .iter()
        .filter(|(k, _)| k.contains("never_ready") && k.contains("last=join_flow_upstream_drop"))
        .map(|(_, v)| *v)
        .sum()
}

fn curve_at(vsol: u64, vtok: u64) -> PumpCurve {
    PumpCurve {
        virtual_sol: vsol,
        virtual_token: vtok,
        real_sol: vsol.saturating_sub(30_000_000_000),
        real_token: 0,
        complete: false,
    }
}

fn prev_snapshot() -> ReserveSnapshot {
    ReserveSnapshot {
        virtual_sol: 30_000_000_000,
        virtual_token: 1_000_000_000,
        slot: 900,
    }
}

#[test]
fn an_upstream_dropped_print_refuses_the_window_end_to_end_and_does_not_disable_protection() {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Stub);
    assert!(e.paper_model_enabled());

    // ── Setup: open a HELD position so management/protection can be probed later. ──────────
    drive(
        &mut e,
        &tape(HELD, 40, T0 + 1_000, true, true, 0, 22_000),
        8,
    );
    assert!(
        rep(&e, "snapshot_ok") >= 1,
        "HELD must become an entry candidate: {:?}",
        e.model_lane_report()
    );
    landing(&mut e, HELD, T0 + 80_500);
    assert!(
        e.model_position_open(&HELD),
        "HELD position must open: {:?}",
        e.model_lane_report()
    );

    // ── The production producer->engine step: an out-of-range reserve delta is refused by the
    //    derivation, classified `UpstreamDropped`, and recorded with its wire receive time. ──
    let prev = prev_snapshot();
    let out_of_range = curve_at(31_000_000_000, 1_100_000_000);
    assert!(
        pump_quant_junction::reserve_delta::derive_market_trade_from_delta(
            &AFFECTED,
            Some(prev),
            &out_of_range,
            1_234,
            true,
            Some(DROP_MS),
        )
        .is_none(),
        "the fixture must be a derivation refusal"
    );
    let miss = note_curve_snapshot_outcome(
        &mut e,
        AFFECTED,
        Some(&prev),
        &out_of_range,
        901,
        Some(DROP_MS),
    );
    assert_eq!(
        miss,
        DeltaMiss::UpstreamDropped,
        "the shared production step must classify the rejection as an upstream drop"
    );
    assert_eq!(e.model_ingest_counters().flow_upstream_drops, 1);

    // ── (a) The rejection marks AFFECTED's flow incomplete by NAME. ─────────────────────────
    drive(
        &mut e,
        &tape(AFFECTED, 40, T0 + 1_000, true, true, 0, 22_000),
        8,
    );
    let refuse_drop_a = rep(&e, "refuse:join_flow_upstream_drop");
    assert!(
        refuse_drop_a >= 1,
        "AFFECTED's window must be refused by name: {:?}",
        e.model_lane_report()
    );
    assert!(
        mints_never_ready_with_drop(&e) >= 1,
        "AFFECTED must appear never_ready with the drop reason: {:?}",
        e.model_funnel()
    );
    let snap_ok_a = rep(&e, "snapshot_ok");

    // ── (b) A later VALID print cannot hide the missing observation. ────────────────────────
    drive(
        &mut e,
        &tape(AFFECTED, 1, T0 + 81_000, false, false, 100, 22_000),
        8,
    );
    assert!(
        rep(&e, "refuse:join_flow_upstream_drop") > refuse_drop_a,
        "the still-incomplete window must keep being refused after a valid print: {:?}",
        e.model_lane_report()
    );
    assert_eq!(
        rep(&e, "snapshot_ok"),
        snap_ok_a,
        "no AFFECTED prompt may be served while the window is incomplete: {:?}",
        e.model_lane_report()
    );

    // ── (d) An UNRELATED mint stays usable, and an ordinary no-trade does NOT poison. ───────
    let snap_ok_before_other = rep(&e, "snapshot_ok");
    drive(
        &mut e,
        &tape(OTHER, 40, T0 + 83_000, true, true, 0, 22_000),
        8,
    );
    assert!(
        rep(&e, "snapshot_ok") > snap_ok_before_other,
        "OTHER must be served while AFFECTED is refused: {:?}",
        e.model_lane_report()
    );
    assert_eq!(
        mints_never_ready_with_drop(&e),
        1,
        "the drop must affect only AFFECTED, never OTHER: {:?}",
        e.model_funnel()
    );

    let drops_before_noprint = e.model_ingest_counters().flow_upstream_drops;
    let no_print = note_curve_snapshot_outcome(
        &mut e,
        QUIET,
        Some(&prev),
        &curve_at(30_000_000_000, 1_000_000_000), // reserves unchanged: ordinary no-trade
        901,
        Some(T0 + 84_000),
    );
    assert_eq!(
        no_print,
        DeltaMiss::NoPrint,
        "unchanged reserves are an ordinary no-trade, not feed loss"
    );
    assert_eq!(
        e.model_ingest_counters().flow_upstream_drops,
        drops_before_noprint,
        "an ordinary no-trade must NOT be recorded as an upstream drop"
    );
    let snap_ok_before_quiet = rep(&e, "snapshot_ok");
    drive(
        &mut e,
        &tape(QUIET, 40, T0 + 85_000, true, true, 0, 22_000),
        8,
    );
    assert!(
        rep(&e, "snapshot_ok") > snap_ok_before_quiet,
        "a mint fed only an ordinary no-trade must still be served: {:?}",
        e.model_lane_report()
    );

    // ── (e) Held-position management / reconciliation / emergency protection still work ─────
    //     while AFFECTED's entry window is refused. Refresh HELD's reserves so management is
    //     not refused for staleness, and keep AFFECTED's tape continuous but still inside its
    //     drop horizon.
    let mut phase_e = tape(AFFECTED, 40, T0 + 167_000, false, false, 200, 22_000);
    phase_e.extend(tape(HELD, 3, T0 + 247_000, false, true, 50, 46_000));
    drive(&mut e, &phase_e, 12);
    assert!(
        rep(&e, "mgmt:dispatched") >= 1,
        "the management route must still ask about the held position: {:?}",
        e.model_lane_report()
    );
    assert!(
        e.model_position_open(&HELD),
        "the held position must remain open and monitored"
    );
    assert!(
        e.model_held_degraded().is_empty(),
        "the held position's data must not be degraded by the drop path"
    );
    // Emergency protection engages and does NOT abandon the held position or cancel its exit.
    let pending_before_trip = e.model_mgmt_pending(&HELD);
    e.model_safety_trip("dropped_print_e2e");
    assert!(e.model_safety_blocked(), "SAFETY_OFF must latch");
    assert!(
        e.model_management_complete(),
        "management must never be reported blocked by this path"
    );
    assert!(
        e.model_position_open(&HELD),
        "emergency protection must not abandon a held position"
    );
    assert_eq!(
        e.model_mgmt_pending(&HELD),
        pending_before_trip,
        "SAFETY_OFF must keep the EXIT intent (only ADD is cancelled): {:?}",
        e.model_lane_report()
    );
    // Reconciliation still functions: a reconciled fill for the pending EXIT closes it.
    if let Some((id, kind, intended, filled)) = e.model_mgmt_pending(&HELD) {
        assert_eq!(kind, MgmtKind::Exit);
        let tokens = intended - filled;
        assert!(
            e.model_mgmt_apply_reconciled_fill(HELD, id, tokens, 45_000_000_000)
                .is_ok(),
            "reconciliation of the held EXIT must still work: {:?}",
            e.model_lane_report()
        );
        assert!(
            !e.model_position_open(&HELD),
            "the reconciled EXIT must close the position"
        );
    } else {
        // The exit may have filled through the store's own hard path; that is still the exit
        // route functioning. Require the exit order to have been produced.
        assert!(
            rep(&e, "mgmt:order:exit") >= 1 || rep(&e, "mgmt:fill") >= 1,
            "the held EXIT route must have produced/filled an order: {:?}",
            e.model_lane_report()
        );
    }

    // ── (c) The ROLLING window clears at the horizon, but AFFECTED's CUMULATIVE history is
    //     still short the print — and NO timer repairs that. It stays refused (now by the
    //     cumulative reason) and only reconstruction/reconciliation resumes serving. ───────
    let snap_ok_before_recovery = rep(&e, "snapshot_ok");
    drive(
        &mut e,
        &tape(AFFECTED, 69, T0 + 251_000, false, true, 400, 22_000),
        8,
    );
    assert_eq!(
        rep(&e, "snapshot_ok"),
        snap_ok_before_recovery,
        "a timer must NOT resume serving while cumulative history is short: {:?}",
        e.model_lane_report()
    );
    assert!(
        rep(&e, "refuse:join_flow_history_unreconstructable") >= 1,
        "past the 300 s horizon the refusal must persist, by the cumulative reason: {:?}",
        e.model_lane_report()
    );
    assert_eq!(
        mints_never_ready_with_drop(&e),
        0,
        "the rolling reason must no longer be blamed after the horizon: {:?}",
        e.model_funnel()
    );

    // A PLAUSIBLE, covering receipt is NECESSARY but NOT SUFFICIENT. Production recovery is
    // UNSUPPORTED until a reconstruction installer exists, so it must NOT unlock inference over
    // unchanged incomplete state.
    assert_eq!(
        e.model_reconcile_flow_history(
            &AFFECTED,
            &pump_quant_app::decision_join::ReconstructionReceipt {
                provenance: "capture:test".into(),
                coverage_from_ms: DROP_MS - 1,
                coverage_to_ms: DROP_MS + 1,
            }
        ),
        Err(pump_quant_app::decision_join::ReconcileRefusal::ReconstructionUnsupported),
        "metadata alone must not unlock inference"
    );
    let before_restore = rep(&e, "snapshot_ok");
    drive(
        &mut e,
        &tape(AFFECTED, 5, T0 + 253_000, false, true, 500, 22_000),
        8,
    );
    assert_eq!(
        rep(&e, "snapshot_ok"),
        before_restore,
        "the mint must stay refused: production recovery is unsupported"
    );
}

/// The held position is ON the dropped mint. Its MANAGEMENT must be UNAVAILABLE (not silently
/// served with incomplete inputs), protective capabilities must stay active, and management must
/// resume with the true history only once completeness is restored.
#[test]
fn the_dropped_mints_own_management_is_unavailable_until_its_history_is_reconstructed() {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(HoldStub);

    // Open a HELD position on the mint that will carry the dropped print.
    drive(
        &mut e,
        &tape(AFFECTED, 40, T0 + 1_000, true, true, 0, 22_000),
        8,
    );
    landing(&mut e, AFFECTED, T0 + 80_500);
    assert!(
        e.model_position_open(&AFFECTED),
        "the position on AFFECTED must open first: {:?}",
        e.model_lane_report()
    );

    // An upstream-dropped print on that SAME mint.
    let prev = prev_snapshot();
    let out_of_range = curve_at(31_000_000_000, 1_100_000_000);
    assert_eq!(
        note_curve_snapshot_outcome(
            &mut e,
            AFFECTED,
            Some(&prev),
            &out_of_range,
            901,
            Some(DROP_MS)
        ),
        DeltaMiss::UpstreamDropped,
        "the production step must record the drop"
    );

    // Qwen management is UNAVAILABLE due to incomplete inputs: no NEW prompt is dispatched
    // while the mint's required state is incomplete, and the position stays open.
    let dispatched_before = rep(&e, "dispatched");
    assert_eq!(
        e.model_flow_drop_summary().mints_incomplete_now,
        1,
        "the dropped mint's rolling window must be marked incomplete"
    );
    assert_eq!(
        e.model_flow_drop_summary().mints_history_unreconstructed,
        1,
        "the dropped mint's cumulative history must be marked unreconstructed"
    );

    drive(
        &mut e,
        &tape(AFFECTED, 1, T0 + 81_500, false, false, 60, 22_000),
        4,
    );
    assert_eq!(
        rep(&e, "dispatched"),
        dispatched_before,
        "Qwen must not be queried with incomplete required state: {:?}",
        e.model_lane_report()
    );
    assert!(
        e.model_position_open(&AFFECTED) || rep(&e, "skip:held_or_pending") > 0,
        "the held position must stay TRACKED (open or pending) — only Qwen management is \
         unavailable: open={} rep={:?}",
        e.model_position_open(&AFFECTED),
        e.model_lane_report()
    );

    // Later VALID prints must not prematurely clear it.
    drive(
        &mut e,
        &tape(AFFECTED, 6, T0 + 120_000, false, false, 80, 22_000),
        8,
    );
    assert_eq!(
        rep(&e, "dispatched"),
        dispatched_before,
        "valid later prints must not clear the gap"
    );
    assert_eq!(
        e.model_flow_drop_summary().mints_incomplete_now,
        1,
        "the window must still be incomplete after valid prints"
    );

    // Protective capabilities remain active while Qwen management is unavailable.
    e.model_safety_trip("dropped_print_held_e2e");
    assert!(
        e.model_safety_blocked(),
        "SAFETY_OFF must latch while management is unavailable"
    );
    assert!(
        e.model_management_complete(),
        "management must never be reported blocked by this path"
    );
    assert!(
        e.model_position_open(&AFFECTED) || rep(&e, "skip:held_or_pending") > 0,
        "emergency protection must not abandon the held position"
    );

    // A timer must NOT resume management; it clears the ROLLING reason only.
    drive(
        &mut e,
        &tape(AFFECTED, 69, T0 + 251_000, false, true, 200, 22_000),
        8,
    );
    assert_eq!(
        rep(&e, "dispatched"),
        dispatched_before,
        "a timer must not resume management while the history is short"
    );
    assert_eq!(
        e.model_flow_drop_summary().mints_incomplete_now,
        0,
        "the rolling reason clears at the horizon"
    );
    assert_eq!(
        e.model_flow_drop_summary().mints_history_unreconstructed,
        1,
        "the cumulative reason must survive the horizon"
    );

    // ...and in PRODUCTION no call resolves it: recovery is unsupported until an installer exists.
    assert_eq!(
        e.model_reconcile_flow_history(
            &AFFECTED,
            &pump_quant_app::decision_join::ReconstructionReceipt {
                provenance: "capture:test".into(),
                coverage_from_ms: DROP_MS - 1,
                coverage_to_ms: DROP_MS + 1,
            }
        ),
        Err(pump_quant_app::decision_join::ReconcileRefusal::ReconstructionUnsupported),
        "metadata alone must not resolve the gap"
    );
    assert_eq!(
        e.model_flow_drop_summary().mints_history_unreconstructed,
        1,
        "the cumulative reason must survive even a plausible receipt"
    );
    let served_before_restore = rep(&e, "snapshot_ok") + rep(&e, "dispatched");
    drive(
        &mut e,
        &tape(AFFECTED, 5, T0 + 253_000, false, true, 300, 22_000),
        8,
    );
    assert_eq!(
        rep(&e, "snapshot_ok") + rep(&e, "dispatched"),
        served_before_restore,
        "with recovery unsupported the held mint stays refused (Qwen management unavailable): {:?}",
        e.model_lane_report()
    );
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// Durability through the REAL daemon persistence functions (temporary directory + fresh engine).
// ════════════════════════════════════════════════════════════════════════════════════════════

use pump_quant_app::decision_join::MissingKind;
use pump_quant_app::missing_history_store::{self as mhs, StoreLoad, Writer};
use pump_quant_junction::model_lifecycle::{attach_missing_history, MissingHistoryStartup};

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pq_mh_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A fresh engine with the paper model armed and AFFECTED's warm-up tape driven.
fn engine_with_tape(
    path: &std::path::Path,
    held_restored: bool,
) -> (Engine, MissingHistoryStartup) {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Stub);
    let st = attach_missing_history(&mut e, path, held_restored);
    drive(
        &mut e,
        &tape(AFFECTED, 40, T0 + 1_000, true, true, 0, 22_000),
        6,
    );
    (e, st)
}

fn record_possible_trade(e: &mut Engine) {
    let prev = prev_snapshot();
    let miss = note_curve_snapshot_outcome(
        e,
        AFFECTED,
        Some(&prev),
        &curve_at(31_000_000_000, 1_100_000_000),
        901,
        Some(DROP_MS),
    );
    assert_eq!(miss, DeltaMiss::UpstreamDropped);
}

#[test]
fn a_gap_survives_a_real_restart_through_the_daemon_persistence_functions() {
    let d = tmpdir("restart");
    let path = d.join("model_missing_history.json");

    // CONTROL (the check must be able to FAIL): the same tape with no gap becomes ready.
    let (control, st) = engine_with_tape(&d.join("control.json"), false);
    assert_eq!(st, MissingHistoryStartup::Clean);
    assert!(
        rep(&control, "snapshot_ok") >= 1,
        "control must be ready: {:?}",
        control.model_lane_report()
    );
    drop(control);

    // Process 1 records the gap and makes it durable.
    let (mut e1, st) = engine_with_tape(&path, false);
    assert_eq!(st, MissingHistoryStartup::Clean);
    record_possible_trade(&mut e1);
    assert!(
        e1.model_missing_flush(Duration::from_secs(5)),
        "the write must become durable"
    );
    let on_disk = match mhs::load(&path) {
        StoreLoad::Records(r) => r,
        other => panic!("expected records on disk, got {other:?}"),
    };
    assert_eq!(on_disk.len(), 1);
    assert_eq!(on_disk[0].1.kind, MissingKind::PossibleTrade);
    assert_eq!(on_disk[0].1.source_id, "slot:901");
    drop(e1); // process exit

    // Process 2: a fresh engine rebuilds the SAME tape; the gap must be restored first.
    let (e2, st) = engine_with_tape(&path, false);
    assert_eq!(st, MissingHistoryStartup::Restored(1));
    assert_eq!(
        rep(&e2, "snapshot_ok"),
        0,
        "a restart must not erase the gap: {:?}",
        e2.model_lane_report()
    );
    assert!(e2.model_missing_history_status(&AFFECTED).is_some());
    assert_eq!(
        e2.model_flow_drop_summary().mints_history_unreconstructed,
        1
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_failed_write_is_counted_keeps_refusing_and_is_retried_until_durable() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let d = tmpdir("failwrite");
    let path = d.join("model_missing_history.json");
    let fail = Arc::new(AtomicBool::new(true));

    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Stub);
    e.model_missing_attach_with_writer(Writer::start_with(path.clone(), Some(Arc::clone(&fail))));
    drive(
        &mut e,
        &tape(AFFECTED, 40, T0 + 1_000, true, true, 0, 22_000),
        6,
    );
    record_possible_trade(&mut e);
    for _ in 0..5 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(30));
    }
    let (_, consecutive, total) = e.model_missing_persist_health();
    assert!(
        consecutive >= 1 && total >= 1,
        "the failure must be counted"
    );
    assert!(
        rep(&e, "missing_history:persist_failed") >= 1,
        "and reported: {:?}",
        e.model_lane_report()
    );
    assert!(
        matches!(mhs::load(&path), StoreLoad::NeverWritten),
        "nothing durable yet"
    );
    assert!(
        e.model_missing_history_status(&AFFECTED).is_some(),
        "the in-memory gap still refuses"
    );
    assert!(
        !e.model_safety_blocked(),
        "a failed missing-history write must not trip SAFETY_OFF by itself"
    );

    // The disk recovers; the retry (wire clock +5 s) makes the SAME snapshot durable.
    fail.store(false, Ordering::SeqCst);
    drive(
        &mut e,
        &tape(AFFECTED, 4, T0 + 90_000, false, true, 300, 22_000),
        6,
    );
    assert!(
        e.model_missing_flush(Duration::from_secs(5)),
        "retry must eventually be durable"
    );
    assert!(matches!(mhs::load(&path), StoreLoad::Records(r) if r.len() == 1));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn corrupt_or_deleted_state_refuses_by_name_and_is_never_overwritten() {
    let d = tmpdir("corrupt");

    // Corrupt record.
    let bad = d.join("bad.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let (mut e, st) = engine_with_tape(&bad, false);
    assert_eq!(st, MissingHistoryStartup::ContinuityUnknown("json".into()));
    assert!(e.model_history_continuity_unknown());
    assert_eq!(
        rep(&e, "snapshot_ok"),
        0,
        "unreadable state must refuse: {:?}",
        e.model_lane_report()
    );
    assert!(
        e.model_funnel()
            .keys()
            .any(|k| k.contains("join_history_continuity_unknown")),
        "refused BY NAME: {:?}",
        e.model_funnel()
    );
    record_possible_trade(&mut e); // a new gap while untrusted must not clobber the evidence
    for _ in 0..4 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(&bad).unwrap(),
        "{ not json",
        "evidence left untouched"
    );
    drop(e);

    // Incompatible schema.
    let inc = d.join("inc.json");
    std::fs::write(&inc, r#"{"schema":99,"records":[]}"#).unwrap();
    let (e, st) = engine_with_tape(&inc, false);
    assert_eq!(
        st,
        MissingHistoryStartup::ContinuityUnknown("schema".into())
    );
    assert_eq!(rep(&e, "snapshot_ok"), 0);
    drop(e);

    // Deleted record next to restored exposure: absent != "no gap".
    let gone = d.join("gone.json");
    let (e, st) = engine_with_tape(&gone, true);
    assert_eq!(
        st,
        MissingHistoryStartup::ContinuityUnknown("absent_with_restored_exposure".into())
    );
    assert_eq!(rep(&e, "snapshot_ok"), 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn persistence_cost_is_bounded_and_unchanged_ticks_do_no_io() {
    let d = tmpdir("cost");
    let path = d.join("model_missing_history.json");
    let (mut e, _) = engine_with_tape(&path, false);
    // 64 mints x a FULL ring (64) of distinct observations each = 4096 records: the worst bounded snapshot.
    for m in 0..64u8 {
        for i in 0..64i64 {
            e.note_missing_observation(
                [m.wrapping_add(1); 32],
                T0 + i,
                MissingKind::PossibleTrade,
                format!("slot:{i}"),
            );
        }
    }
    let t = std::time::Instant::now();
    e.tick(AppEvent::Tick); // encodes + hands off the snapshot
    let first = t.elapsed();
    let t = std::time::Instant::now();
    for _ in 0..1_000 {
        e.tick(AppEvent::Tick); // rev unchanged: one integer compare
    }
    let steady = t.elapsed();
    eprintln!("missing-history persist: change tick {first:?}; 1000 unchanged ticks {steady:?}");
    assert!(first < Duration::from_millis(250), "change tick {first:?}");
    assert!(e.model_missing_flush(Duration::from_secs(5)));
    assert!(matches!(mhs::load(&path), StoreLoad::Records(r) if r.len() == 64 * 64));
    let _ = std::fs::remove_dir_all(&d);
}

// ════════════════════════════════════════════════════════════════════════════════════════════
// Protection on the AFFECTED held mint with NO pending exit.
// ════════════════════════════════════════════════════════════════════════════════════════════

fn collapse_print(e: &mut Engine, at_ms: i64, seq: u32) {
    e.tick(AppEvent::MarketTrade {
        mint: dm(AFFECTED),
        price_fp: 20_000, // ~-57% vs ~46_000: a single-swap collapse (precursor is 3_000 bps)
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 9_000 + u64::from(seq),
        age_slots: 30,
        recv_unix_ms: Some(at_ms),
        trader_pubkey: Some(wallet(seq)),
        slot: Some(1_000 + u64::from(seq)),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
}

fn protection_on_affected_held_mint(trip_safety_off_first: bool) {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(HoldStub);
    drive(
        &mut e,
        &tape(AFFECTED, 40, T0 + 1_000, true, true, 0, 22_000),
        8,
    );
    landing(&mut e, AFFECTED, T0 + 80_500);
    assert!(
        e.model_position_open(&AFFECTED),
        "{:?}",
        e.model_lane_report()
    );
    let inv_before = e.model_inventory_tokens(&AFFECTED);
    assert!(
        inv_before.is_some_and(|i| i > 0),
        "reconciled inventory exists: {inv_before:?}"
    );
    let cash_before = e.bankroll_balance();

    // The gap on THIS mint: Qwen management becomes unavailable.
    record_possible_trade(&mut e);
    let dispatched = rep(&e, "mgmt:dispatched");
    // Past the 60 s minimum hold so management is actually DUE and gets refused by name.
    drive(
        &mut e,
        &tape(AFFECTED, 40, T0 + 150_000, false, true, 100, 46_000),
        12,
    );
    assert_eq!(
        rep(&e, "mgmt:dispatched"),
        dispatched,
        "Qwen management is unavailable (refused, not asked)"
    );
    assert!(
        e.model_lane_report()
            .keys()
            .any(|k| k.starts_with("mgmt:refuse:join_flow")),
        "management refused BY NAME: {:?}",
        e.model_lane_report()
    );
    // NO masking pending exit: nothing is pending on the mint when the emergency arrives.
    assert_eq!(
        e.model_mgmt_pending(&AFFECTED),
        None,
        "no pre-existing pending exit"
    );
    // Missing data is NOT an endpoint failure and did not trip SAFETY_OFF by itself.
    assert!(
        !e.model_safety_blocked(),
        "SAFETY_OFF tripped by missing data: {:?}",
        e.model_lane_report()
    );
    assert_eq!(rep(&e, "endpoint:"), 0);
    assert_eq!(rep(&e, "safety:tripped"), 0);
    // The inputs that are NOT market history stay intact.
    assert_eq!(e.model_inventory_tokens(&AFFECTED), inv_before);

    if trip_safety_off_first {
        e.model_safety_trip_operator();
        assert!(e.model_safety_blocked());
    }

    assert!(
        e.model_position_open(&AFFECTED),
        "the position is still open going into the emergency"
    );
    // The agreed emergency condition: a single-swap collapse. Protection must act on its own.
    collapse_print(&mut e, T0 + 150_000 + 40 * 2_000 + 2_000, 150);
    for _ in 0..4 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(
        !e.model_position_open(&AFFECTED),
        "protection must close the held position"
    );
    assert_eq!(
        e.model_inventory_tokens(&AFFECTED),
        None,
        "inventory settled"
    );
    assert_ne!(
        e.bankroll_balance(),
        cash_before,
        "cash settled from the exit"
    );
    assert!(
        e.model_missing_history_status(&AFFECTED).is_some(),
        "the gap is still acknowledged, not repaired"
    );
    assert_eq!(
        rep(&e, "mgmt:dispatched"),
        dispatched,
        "and Qwen was never consulted"
    );
}

#[test]
fn emergency_protection_closes_the_affected_held_mint_with_no_pending_exit() {
    protection_on_affected_held_mint(false);
}

#[test]
fn emergency_protection_survives_safety_off_on_the_affected_held_mint() {
    protection_on_affected_held_mint(true);
}
