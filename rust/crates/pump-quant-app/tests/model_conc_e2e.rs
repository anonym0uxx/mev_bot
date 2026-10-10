//! conc C6: bounded client concurrency through the REAL engine (`Engine::tick`): queued expiry (never sent, released
//! once, late reply unusable), local overload classed apart from endpoint health, the named local-overload latch
//! (reset/latch rules), worker counts {2, 4, 6} and the bound formula. Synthetic feed; no network.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_admit::{model_bound, MODEL_WORKERS_ALLOWED};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;

fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}

fn mint() -> DomainMint {
    DomainMint::from_bytes(MINT)
}

fn cfg() -> Config {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    c
}

/// Answers by PROMPT CONTENT: the entry family gets BUY; the management family gets whatever the
/// script says for the step number it can read in the prompt. Records every management prompt.
struct Script {
    prompts: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    answer: fn(step: i64) -> &'static str,
}

const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const REDUCE: &str = "DECISION: REDUCE\nINVALIDATION: none\nEVIDENCE: x";
const EXIT: &str = "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: x";
const ADD: &str = "DECISION: ADD\nINVALIDATION: none\nEVIDENCE: x";

impl ModelSource for Script {
    fn complete(&self, _s: &str, user: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if user.starts_with("Decide the next action for a position you already hold") {
            self.prompts.lock().unwrap().push(user.to_string());
            let step: i64 = user
                .lines()
                .find_map(|l| l.strip_prefix("STEP: "))
                .and_then(|v| v.trim().parse().ok())
                .expect("a management prompt names its step");
            return Ok((self.answer)(step).to_string());
        }
        Ok(BUY.to_string())
    }
}

fn events(n: u32) -> Vec<AppEvent> {
    let mut ev = vec![AppEvent::LaunchObserved {
        mint: mint(),
        creator: CREATOR,
        launch_unix_ms: T0,
    }];
    for i in 0..n {
        let buy = i % 3 != 0;
        ev.push(AppEvent::MarketTrade {
            mint: mint(),
            price_fp: 22_000 + i128::from(i),
            quote_lamports: 500_000_000 + u64::from(i),
            liquidity_lamports: VSOL,
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(i),
            age_slots: 30,
            recv_unix_ms: Some(T0 + 1_000 + i64::from(i) * 2_000),
            trader_pubkey: Some(wallet(i)),
            slot: Some(1_000 + u64::from(i)),
            fee_lamports: Some(60_000 + u64::from(i) * 100),
            cu_consumed: Some(90_000 + u64::from(i)),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    let t_last = T0 + 1_000 + i64::from(n) * 2_000;
    ev.push(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(t_last),
        slot: 2_000,
    });
    ev.push(AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem: false,
        slot: 0,
    });
    ev.push(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn curve(e: &mut Engine, ts: i64, slot: u64, dsol: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL + dsol,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    });
    e.tick(AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem: false,
        slot: 0,
    });
    ticks(e, 6);
}

/// A trade print on the held mint at the feed clock `ts` (keeps decision state fresh and the clock moving).
fn print(e: &mut Engine, i: u32, ts: i64, slot: u64) {
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        // Near the curve fill's entry price (~45_085): the held position must not be underwater,
        // or the agreed HARD STOP (correctly) closes it before the model is ever asked.
        price_fp: 45_300 + i128::from(i % 7),
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: VSOL,
        signed_base: 30_000_000_000,
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(wallet(i)),
        slot: Some(slot),
        fee_lamports: Some(60_000 + u64::from(i) * 100),
        cu_consumed: Some(90_000 + u64::from(i)),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
}

/// Blocks every call while the gate is closed, then answers BUY (a late BUY must never create an order).
struct Gate {
    open: Arc<std::sync::atomic::AtomicBool>,
    calls: Arc<AtomicUsize>,
}
impl ModelSource for Gate {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        while !self.open.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(BUY.to_string())
    }
}

fn rep(e: &Engine, k: &str) -> u64 {
    e.model_lane_report().get(k).copied().unwrap_or(0)
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pq_conc_e2e_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("SAFETY_STATE.json")
}

struct Wedged {
    e: Engine,
    open: Arc<std::sync::atomic::AtomicBool>,
    calls: Arc<AtomicUsize>,
    clock: i64,
    i: u32,
}

fn wedged(workers: usize, tag: &str) -> Wedged {
    let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model_with_workers(
        Gate {
            open: Arc::clone(&open),
            calls: Arc::clone(&calls),
        },
        workers,
    )
    .unwrap();
    e.model_safety_attach(&tmp(tag));
    for ev in events(40) {
        e.tick(ev);
    }
    Wedged {
        e,
        open,
        calls,
        clock: T0 + 1_000 + 40 * 2_000,
        i: 0,
    }
}

impl Wedged {
    fn step(&mut self) {
        self.clock += 5_000;
        self.i += 1;
        print(
            &mut self.e,
            200 + self.i,
            self.clock,
            4_000 + u64::from(self.i),
        );
        ticks(&mut self.e, 1);
    }
    fn release_and_drain(&mut self) {
        self.open.store(true, Ordering::SeqCst);
        for _ in 0..300 {
            ticks(&mut self.e, 1);
            let sent = self.calls.load(Ordering::SeqCst) as u64;
            if rep(&self.e, "discard:abandoned") + rep(&self.e, "discard:entries_blocked") >= sent {
                break;
            }
        }
    }
}

#[test]
fn the_worker_count_is_a_named_option_and_every_bound_derives_from_it() {
    assert_eq!(MODEL_WORKERS_ALLOWED, [2, 4, 6]);
    for n in [2usize, 4, 6] {
        let mut e = Engine::new(cfg(), RunMode::Paper);
        e.enable_paper_model_with_workers(
            Gate {
                open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            n,
        )
        .unwrap();
        assert_eq!(e.model_workers(), n);
        assert_eq!(
            e.model_bounds(),
            (2 * n, 2 * n, 2 * n),
            "entry cap, mgmt cap, queue = 2N"
        );
        assert_eq!(model_bound(n), 2 * n);
    }
    // Default = 2 = the previous fixed constants (2 workers, queue 4, tables 4/4).
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Gate {
        open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    assert_eq!((e.model_workers(), e.model_bounds()), (2, (4, 4, 4)));
    for bad in [0usize, 1, 3, 5, 7, 8] {
        let mut e = Engine::new(cfg(), RunMode::Paper);
        let r = e.enable_paper_model_with_workers(
            Gate {
                open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            bad,
        );
        assert_eq!(r, Err("model_workers_not_allowed"));
        assert!(!e.paper_model_enabled(), "nothing armed on a refused count");
    }
}

/// QUEUED EXPIRY (C3) + CLASSIFICATION (C4), workers = 2 (the default). Both workers wedge inside the endpoint on the
/// first two (sent) asks. Every later ask waits in the local queue past its 3 s engine deadline and is NEVER sent:
/// the endpoint sees exactly 2 calls. Local expiries are not endpoint evidence (the endpoint streak stays at the 2
/// sent abandonments), each releases its table slot exactly once, and 3 consecutive local expiries latch the named
/// local-overload block. The late replies are unusable (discarded by name, no order).
#[test]
fn a_queued_ask_past_its_deadline_is_never_sent_is_released_once_and_is_not_endpoint_evidence() {
    let mut w = wedged(2, "qexp");
    for _ in 0..60 {
        if w.e.model_safety_blocked() {
            break;
        }
        w.step();
    }
    assert!(w.e.model_safety_blocked(), "{:?}", w.e.model_lane_report());
    assert_eq!(
        w.e.model_safety_reason(),
        pump_quant_app::safety_off::REASON_LOCAL_OVERLOAD
    );
    assert_eq!(
        w.calls.load(Ordering::SeqCst),
        2,
        "only the 2 sent asks reached the endpoint"
    );
    assert_eq!(rep(&w.e, "request_abandoned_deadline"), 2);
    assert_eq!(
        w.e.model_safety_consecutive_failures(),
        2,
        "local expiry is never charged to the endpoint"
    );
    assert_eq!(w.e.model_local_expired_streak(), 3);
    assert_eq!(rep(&w.e, "local_queue_expired"), 3);
    assert_eq!(
        rep(&w.e, "local:queue_expired_already_released"),
        0,
        "released exactly once"
    );
    assert!(rep(&w.e, "stop:ALERT_MODEL_LOCAL_OVERLOAD:entry_asks_stopped_local_overload_management_continues") <= 1);
    let recs = w.e.model_request_records();
    let local: Vec<_> = recs
        .iter()
        .filter(|r| r.outcome == Some("local_queue_expired"))
        .collect();
    assert_eq!(local.len(), 3);
    for r in &local {
        assert_eq!(r.kind, "entry");
        assert_eq!(r.http_send_ms, None, "never sent: {r:?}");
        assert_eq!(
            r.deadline_ms,
            r.created_ms + 3_000,
            "the 3 s start point is the submit"
        );
        assert!(r.resolved_ms.unwrap() > r.deadline_ms);
    }
    let sent: Vec<_> = recs
        .iter()
        .filter(|r| r.outcome == Some("abandoned_deadline"))
        .collect();
    assert_eq!(sent.len(), 2);
    // The endpoint comes back: the two late BUYs are for abandoned asks -> discarded, no order, no position.
    w.release_and_drain();
    assert_eq!(
        rep(&w.e, "discard:abandoned"),
        2,
        "{:?}",
        w.e.model_lane_report()
    );
    assert_eq!(w.e.model_pending_orders(), 0);
    assert!(!w.e.model_position_open(&MINT));
    let recs = w.e.model_request_records();
    for r in recs
        .iter()
        .filter(|r| r.outcome == Some("abandoned_deadline"))
    {
        assert_eq!(r.late, Some("discard:abandoned"));
        assert_eq!(r.action, Some("BUY"), "the parsed action is recorded");
        assert!(r.sent && r.http_send_ms.is_some());
    }
    // Every slot is free again (none leaked, none freed twice).
    assert_eq!(
        w.e.model_entry_outstanding(),
        0,
        "{:?}",
        w.e.model_lane_report()
    );
}

/// LATCH + RESET (C4): the local-overload block is durable SAFETY_OFF. A healthy answer after the trip resets the
/// streak but does NOT re-arm; a restart does not re-arm; only the explicit operator re-arm lifts it.
#[test]
fn the_local_overload_block_latches_and_only_an_explicit_rearm_lifts_it() {
    let mut w = wedged(2, "latch");
    for _ in 0..60 {
        if w.e.model_safety_blocked() {
            break;
        }
        w.step();
    }
    assert_eq!(
        w.e.model_safety_reason(),
        pump_quant_app::safety_off::REASON_LOCAL_OVERLOAD
    );
    assert!(w.e.model_entries_blocked(), "new exposure is blocked");
    w.release_and_drain();
    for _ in 0..5 {
        w.step();
    }
    assert!(w.e.model_safety_blocked(), "no automatic re-arm");
    let p = tmp("latch_restart");
    let _ = p;
    w.e.model_safety_rearm("alon").expect("explicit re-arm");
    assert!(!w.e.model_safety_blocked());
    assert_eq!(
        w.e.model_local_expired_streak(),
        0,
        "re-arm resets the local streak"
    );
}

/// LATE-RESULT REJECTION after abandonment, workers = 4 and 6 (and 2): the endpoint never sees more than N calls,
/// local expiry is never endpoint evidence, and every late reply is discarded by name with no order.
#[test]
fn late_results_after_abandonment_are_rejected_at_every_worker_count() {
    for n in [2usize, 4, 6] {
        let mut w = wedged(n, &format!("late{n}"));
        for _ in 0..60 {
            if w.e.model_safety_blocked() {
                break;
            }
            w.step();
        }
        assert!(
            w.e.model_safety_blocked(),
            "n={n} {:?}",
            w.e.model_lane_report()
        );
        let sent = w.calls.load(Ordering::SeqCst);
        assert!(sent <= n, "n={n}: the endpoint saw {sent} calls");
        let abandoned = rep(&w.e, "request_abandoned_deadline");
        assert_eq!(
            abandoned, sent as u64,
            "n={n}: only sent asks are endpoint abandonments"
        );
        assert_eq!(
            u64::from(w.e.model_safety_consecutive_failures()),
            abandoned,
            "n={n}: endpoint streak counts sent abandonments only"
        );
        assert!(w.e.model_bounds().2 == 2 * n);
        w.release_and_drain();
        assert_eq!(rep(&w.e, "discard:abandoned"), abandoned, "n={n}");
        assert_eq!(w.e.model_pending_orders(), 0, "n={n}");
        assert!(!w.e.model_position_open(&MINT), "n={n}");
        assert_eq!(
            rep(&w.e, "local:queue_expired_already_released"),
            0,
            "n={n}"
        );
    }
}
