//! (helpers shared with model_manage_e2e)
//! Model-managed position lane through the REAL engine entry point (`Engine::tick`):
//! entry BUY -> simulated fill -> held position -> (60 s) management snapshot rendered by the trained
//! renderer -> asynchronous stub verdict (keyed on PROMPT CONTENT) -> REDUCE / EXIT order -> paper fill
//! -> updated position. Synthetic events: this proves execution integration and failure handling. It is
//! NOT evidence that management is profitable.

#![allow(dead_code)] // test scaffolding: helper/fixture chains not every #[test] exercises (consolidation N2)

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pump_quant_app::config::Config;
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
/// A source that never answers inside the deadline (it sleeps), for the hung-endpoint trip.
struct Hang;
impl ModelSource for Hang {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        std::thread::sleep(Duration::from_millis(900));
        Err(InferenceError::Transport("hung".into()))
    }
}

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

struct Rig {
    e: Engine,
    prompts: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    clock: i64,
    slot: u64,
    n: u32,
}

fn rig(answer: fn(i64) -> &'static str) -> Rig {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        prompts: Arc::clone(&prompts),
        calls: Arc::clone(&calls),
        answer,
    });
    let evs = events(40);
    for ev in &evs {
        e.tick(*ev);
    }
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    assert!(
        e.model_position_open(&MINT),
        "setup: the entry must have filled: {:?}",
        e.model_lane_report()
    );
    Rig {
        e,
        prompts,
        calls,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
    }
}

impl Rig {
    /// Advance the feed clock by `ms` in 5 s prints, ticking so asks and fills can run.
    fn advance(&mut self, ms: i64) {
        let end = self.clock + ms;
        while self.clock < end {
            self.clock += 5_000;
            self.slot += 1;
            self.n += 1;
            print(&mut self.e, self.n, self.clock, self.slot);
            ticks(&mut self.e, 2);
        }
    }

    /// Advance in 1 s steps until a management ORDER is pending (or `max_ms` elapses), then stop, so
    /// the fill TTL has not run when the test supplies a landing state.
    fn advance_to_order(&mut self, max_ms: i64) {
        let end = self.clock + max_ms;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.clock += 1_000;
            self.slot += 1;
            self.n += 1;
            print(&mut self.e, self.n, self.clock, self.slot);
            ticks(&mut self.e, 2);
        }
    }

    fn landing(&mut self, dsol: u64) {
        self.clock += 1_000;
        self.slot += 5;
        curve(&mut self.e, self.clock, self.slot, dsol);
    }

    fn rep(&self, k: &str) -> u64 {
        self.e
            .model_lane_report()
            .iter()
            .filter(|(key, _)| key.starts_with(k))
            .map(|(_, v)| *v)
            .sum()
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pq_safety_e2e_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("SAFETY_STATE.json")
}

fn fresh_engine(answer: fn(i64) -> &'static str) -> (Engine, Arc<AtomicUsize>) {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        prompts,
        calls: Arc::clone(&calls),
        answer,
    });
    (e, calls)
}

#[test]
fn a_persisted_block_survives_restart_and_only_an_explicit_rearm_lifts_it() {
    let p = tmp("restart");
    let (mut e1, _c1) = fresh_engine(|_| HOLD);
    e1.model_safety_attach(&p);
    e1.model_safety_trip_operator();
    assert!(e1.model_safety_blocked());
    drop(e1);

    // RESTART: a new process with the same file. It must come back BLOCKED and must not ask.
    let (mut e2, calls2) = fresh_engine(|_| HOLD);
    let load = e2.model_safety_attach(&p);
    assert!(matches!(
        load,
        pump_quant_app::safety_off::SafetyLoad::Blocked { .. }
    ));
    assert!(e2.model_safety_blocked(), "restart must not re-arm");
    for ev in events(40) {
        e2.tick(ev);
    }
    ticks(&mut e2, 8);
    assert_eq!(
        calls2.load(Ordering::SeqCst),
        0,
        "no model asks while blocked"
    );
    assert_eq!(e2.model_pending_orders(), 0);

    // Re-arm needs a named operator and is durable.
    use pump_quant_app::safety_off::RearmRefusal;
    assert_eq!(e2.model_safety_rearm("  "), Err(RearmRefusal::NoOperator));
    assert_eq!(e2.model_safety_rearm("alon"), Ok(()));
    assert!(!e2.model_safety_blocked());
    assert_eq!(e2.model_safety_rearm("alon"), Err(RearmRefusal::NotBlocked));
    drop(e2);
    let (mut e3, _c3) = fresh_engine(|_| HOLD);
    let load3 = e3.model_safety_attach(&p);
    assert!(matches!(
        load3,
        pump_quant_app::safety_off::SafetyLoad::Armed { .. }
    ));
    assert!(!e3.model_safety_blocked());
}

#[test]
fn an_unreadable_state_file_blocks_rather_than_arms() {
    let p = tmp("corrupt");
    std::fs::write(&p, "\u{0}\u{0}garbage").unwrap();
    let (mut e, calls) = fresh_engine(|_| HOLD);
    e.model_safety_attach(&p);
    assert!(e.model_safety_blocked());
    assert_eq!(
        e.model_safety_reason(),
        pump_quant_app::safety_off::REASON_UNREADABLE
    );
    for ev in events(40) {
        e.tick(ev);
    }
    ticks(&mut e, 8);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_hung_endpoint_trips_safety_off_without_stalling_the_engine() {
    let p = tmp("hung");
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Hang);
    e.model_safety_attach(&p);
    let mut clock = T0 + 1_000;
    for ev in events(40) {
        e.tick(ev);
    }
    let t0 = std::time::Instant::now();
    // Feed time runs far faster than the hung workers: every request passes its feed-clock deadline.
    for i in 0..60u32 {
        clock += 5_000;
        print(&mut e, 100 + i, clock, 3_000 + u64::from(i));
        ticks(&mut e, 1);
        if e.model_safety_blocked() {
            break;
        }
    }
    assert!(
        t0.elapsed() < Duration::from_secs(20),
        "the engine kept ticking while the endpoint hung"
    );
    assert!(e.model_safety_blocked(), "{:?}", e.model_lane_report());
    assert_eq!(
        e.model_safety_reason(),
        pump_quant_app::safety_off::REASON_ENDPOINT_HUNG
    );
    // Durable: a fresh engine on the same file starts blocked.
    let (mut e2, _c) = fresh_engine(|_| HOLD);
    e2.model_safety_attach(&p);
    assert!(e2.model_safety_blocked());
}

#[test]
fn timeout_during_an_outstanding_order_keeps_an_uncertain_order_and_invalidates_a_certain_one() {
    let p = tmp("outstanding");
    let (_e, _c) = fresh_engine(|_| HOLD);
    // Entry verdict: use the BUY stub so an order is pending (no landing state yet).
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        prompts,
        calls: Arc::new(AtomicUsize::new(0)),
        answer: |_| HOLD,
    });
    e.model_safety_attach(&p);
    for ev in events(40) {
        e.tick(ev);
    }
    ticks(&mut e, 8);
    let (oid, _attempt, _clip) = e
        .model_pending_order(&MINT)
        .expect("an entry order is pending");
    assert!(e.model_mark_ack_uncertain(oid));
    // Timeout / trip while the order's acknowledgement is unknown.
    let invalidated = e.model_safety_trip("model_endpoint_hung");
    assert_eq!(
        invalidated, 0,
        "an uncertain order may already have landed: it is NOT invalidated"
    );
    assert_eq!(
        e.model_pending_orders(),
        1,
        "pending reconciliation is preserved"
    );
    // Re-arm is refused while that order is unreconciled.
    assert_eq!(
        e.model_safety_rearm("alon"),
        Err(pump_quant_app::safety_off::RearmRefusal::UncertainOrderPending)
    );
    assert!(e.model_safety_blocked());
    // The durable file lists the pending order.
    let raw = std::fs::read_to_string(&p).unwrap();
    assert!(raw.contains("\"uncertain\": true"), "{raw}");
}

#[test]
fn a_queued_certain_entry_order_is_invalidated_by_the_block() {
    let p = tmp("queued");
    let (mut e, _c) = fresh_engine(|_| HOLD);
    e.model_safety_attach(&p);
    for ev in events(40) {
        e.tick(ev);
    }
    ticks(&mut e, 8);
    assert_eq!(e.model_pending_orders(), 1, "{:?}", e.model_lane_report());
    assert_eq!(e.model_safety_trip_operator(), 1);
    assert_eq!(
        e.model_pending_orders(),
        0,
        "a queued risk-increasing intent must not fill later"
    );
    curve(&mut e, T0 + 1_000 + 40 * 2_000 + 1_500, 2_100, 200_000_000);
    assert!(
        !e.model_position_open(&MINT),
        "no position opens after the block"
    );
}

#[test]
fn controlled_shutdown_holds_positions_keeps_orders_pending_and_management_keeps_working() {
    let p = tmp("shutdown");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r.e.model_safety_attach(&p);
    r.advance_to_order(120_000);
    let (_id, _, intended, _) =
        r.e.model_mgmt_pending(&MINT)
            .expect("a REDUCE order is pending");
    let rep = r.e.model_controlled_shutdown();
    assert_eq!(rep.held, 1, "shutdown never flattens a held position");
    assert_eq!(rep.mgmt_orders_pending, 1);
    assert!(rep.persisted);
    assert!(r.e.model_position_open(&MINT));
    // The independent protective / management path is NOT blocked by SAFETY_OFF: the pending REDUCE
    // still fills through a landing state.
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.landing(250_000_000);
    r.landing(260_000_000);
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - intended));
    // The durable record lists the held position for the restart.
    let held = pump_quant_app::safety_off::SafetyOff::read_held(&p);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].mint, MINT);
    assert!(held[0].model_managed);
}

#[test]
fn a_new_management_question_is_still_asked_while_entries_are_blocked() {
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r.e.model_safety_trip_operator();
    r.advance_to_order(120_000);
    assert!(
        r.e.model_mgmt_pending(&MINT).is_some(),
        "risk-reducing management must survive SAFETY_OFF: {:?}",
        r.e.model_lane_report()
    );
}

/// A source whose calls block while the gate is closed, then fail at the transport (the 8 s socket timeout's Err).
struct Gate(Arc<std::sync::atomic::AtomicBool>);
impl ModelSource for Gate {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        while !self.0.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(InferenceError::Transport("socket timeout".into()))
    }
}

/// HEALTH IS CHARGED ONCE PER REQUEST ID. Reasoning from the initial counters: consecutive failures start at 0 and
/// the trip threshold is `CONSECUTIVE_ABANDONED_TRIP` = 3. Two asks (entry ids 1 and 2) are each abandoned at the
/// engine deadline (+1 each -> 2) and then each answer LATE with a transport error (the socket timeout). The late
/// error keeps its own label (`endpoint:transport_error`) but is the SAME request, so it adds 0: the count stays 2
/// and the latch does NOT trip (the serving run counted 4 increments for 2 asks and tripped). A THIRD distinct ask
/// (id 3) abandoned at its deadline makes 3 -> trip.
#[test]
fn two_abandoned_asks_whose_late_errors_arrive_do_not_trip_three_distinct_asks_do() {
    use pump_quant_app::safety_off::CONSECUTIVE_ABANDONED_TRIP;
    assert_eq!(
        CONSECUTIVE_ABANDONED_TRIP, 3,
        "the threshold this test reasons from"
    );
    let p = tmp("once_per_id");
    let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Gate(Arc::clone(&open)));
    e.model_safety_attach(&p);
    let rep = |e: &Engine, k: &str| e.model_lane_report().get(k).copied().unwrap_or(0);
    for ev in events(40) {
        e.tick(ev);
    }
    assert_eq!(e.model_safety_consecutive_failures(), 0, "initial counter");
    assert!(!e.model_safety_blocked());
    let mut clock = T0 + 1_000 + 40 * 2_000;
    let mut i = 0u32;
    let mut step = |e: &mut Engine| {
        clock += 5_000;
        i += 1;
        print(e, 200 + i, clock, 4_000 + u64::from(i));
        ticks(e, 1);
    };
    // Phase A: two asks, both abandoned at the deadline while their workers are still blocked.
    for _ in 0..40 {
        if rep(&e, "request_abandoned_deadline") >= 2 {
            break;
        }
        step(&mut e);
    }
    assert_eq!(
        rep(&e, "request_abandoned_deadline"),
        2,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(
        e.model_table_last_issued_for_test(),
        2,
        "request ids 1 and 2"
    );
    assert_eq!(e.model_safety_consecutive_failures(), 2);
    assert!(!e.model_safety_blocked());
    // Their late transport errors arrive (no clock movement, so no new ask): labelled, NOT charged again.
    open.store(true, Ordering::SeqCst);
    for _ in 0..200 {
        if rep(&e, "health:late_arrival_already_charged") >= 2 {
            break;
        }
        ticks(&mut e, 1);
    }
    open.store(false, Ordering::SeqCst);
    assert_eq!(
        rep(&e, "health:late_arrival_already_charged"),
        2,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(
        rep(&e, "endpoint:transport_error"),
        2,
        "the socket-timeout label is kept separately"
    );
    assert_eq!(
        rep(&e, "discard:abandoned"),
        2,
        "late answers are discarded, never executed"
    );
    assert_eq!(
        e.model_safety_consecutive_failures(),
        2,
        "2 asks = 2 failures, not 4: {:?}",
        e.model_lane_report()
    );
    assert!(
        !e.model_safety_blocked(),
        "two asks must not trip a threshold of three"
    );
    // Phase B: a third distinct ask is abandoned -> 3 -> trip.
    for _ in 0..40 {
        if e.model_safety_blocked() {
            break;
        }
        step(&mut e);
    }
    assert_eq!(
        e.model_table_last_issued_for_test(),
        3,
        "a third request id"
    );
    assert_eq!(rep(&e, "request_abandoned_deadline"), 3);
    assert!(e.model_safety_blocked(), "{:?}", e.model_lane_report());
    assert_eq!(
        e.model_safety_reason(),
        pump_quant_app::safety_off::REASON_ENDPOINT_HUNG
    );
    open.store(true, Ordering::SeqCst);
}
