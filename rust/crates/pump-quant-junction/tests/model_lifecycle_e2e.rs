//! Daemon-lifecycle integration of the paper model lane, through the PRODUCTION `InferenceClient`
//! talking HTTP to a controlled local llama-server stand-in (no direct verdict injection), armed and
//! stopped by the same `model_lifecycle` code `pq_daemon` runs.
//!
//! Synthetic events: this proves execution plumbing, request/position binding, fill accounting and
//! failure/shutdown behaviour. It is NOT evidence that management is profitable, and paper sell
//! economics on the AMM remain unassessed.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_junction::model_lifecycle::{
    arm_paper_model, decide_stop, handle_stop_request, StopGate,
};

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


/// A curve observation at the feed clock, without extra ticks: the account subscription keeps the
/// reserves fresh in production, and management now refuses reserves older than PRICING_BUDGET_MS.
fn curve_quiet(e: &mut Engine, ts: i64, slot: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    });
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
    });
}


const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const REDUCE: &str = "DECISION: REDUCE\nINVALIDATION: none\nEVIDENCE: x";
const EXIT: &str = "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: x";
const ADD: &str = "DECISION: ADD\nINVALIDATION: none\nEVIDENCE: x";

/// A controlled llama-server stand-in speaking the OpenAI chat-completions wire form over real TCP.
struct Endpoint {
    url: String,
    answer: Arc<Mutex<fn(i64) -> &'static str>>,
    hang: Arc<AtomicBool>,
    mgmt_requests: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<String>>>,
}

fn read_request(s: &mut TcpStream) -> Option<String> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(h) = text.find("\r\n\r\n") {
            let len = text[..h]
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
            if buf.len() >= h + 4 + len {
                return Some(text[h + 4..h + 4 + len].to_string());
            }
        }
    }
}

impl Endpoint {
    fn start(answer: fn(i64) -> &'static str) -> Endpoint {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let ep = Endpoint {
            url,
            answer: Arc::new(Mutex::new(answer)),
            hang: Arc::new(AtomicBool::new(false)),
            mgmt_requests: Arc::new(AtomicUsize::new(0)),
            bodies: Arc::new(Mutex::new(Vec::new())),
        };
        let (ans, hang, cnt, bodies) = (
            Arc::clone(&ep.answer),
            Arc::clone(&ep.hang),
            Arc::clone(&ep.mgmt_requests),
            Arc::clone(&ep.bodies),
        );
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut s) = conn else { continue };
                let (ans, hang, cnt, bodies) = (Arc::clone(&ans), Arc::clone(&hang), Arc::clone(&cnt), Arc::clone(&bodies));
                std::thread::spawn(move || {
                    let Some(body) = read_request(&mut s) else { return };
                    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let user = v["messages"][1]["content"].as_str().unwrap_or("").to_string();
                    bodies.lock().unwrap().push(body.clone());
                    if hang.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_secs(12));
                        return;
                    }
                    let text = if user.starts_with("Decide the next action for a position you already hold") {
                        cnt.fetch_add(1, Ordering::SeqCst);
                        let step: i64 = user
                            .lines()
                            .find_map(|l| l.strip_prefix("STEP: "))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(-1);
                        (ans.lock().unwrap())(step).to_string()
                    } else {
                        BUY.to_string()
                    };
                    let out = serde_json::json!({"choices":[{"message":{"content":text},"finish_reason":"stop"}]}).to_string();
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        out.len(),
                        out
                    );
                });
            }
        });
        ep
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pq_lifecycle_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Rig {
    e: Engine,
    clock: i64,
    slot: u64,
    n: u32,
    dir: std::path::PathBuf,
}

fn rig(ep: &Endpoint, name: &str) -> Rig {
    let dir = tmp(name);
    let mut e = Engine::new(cfg(), RunMode::Paper);
    let armed = arm_paper_model(&mut e, &ep.url, &dir.join("safety.json"));
    assert!(!armed.blocked_at_start, "{armed:?}");
    for ev in events(40) {
        e.tick(ev);
    }
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    // The entry decision travels over HTTP; give the worker a moment to land.
    for _ in 0..50 {
        if e.model_position_open(&MINT) {
            break;
        }
        ticks(&mut e, 2);
        curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    }
    assert!(e.model_position_open(&MINT), "entry must fill: {:?}", e.model_lane_report());
    Rig { e, clock: t_last + 1_500, slot: 2_100, n: 41, dir }
}

impl Rig {
    fn advance_to_order(&mut self, max_ms: i64) {
        let end = self.clock + max_ms;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.clock += 1_000;
            self.slot += 1;
            self.n += 1;
            curve_quiet(&mut self.e, self.clock, self.slot);
            print(&mut self.e, self.n, self.clock, self.slot);
            ticks(&mut self.e, 3);
        }
    }
    fn advance(&mut self, ms: i64) {
        let end = self.clock + ms;
        while self.clock < end {
            self.clock += 5_000;
            self.slot += 1;
            self.n += 1;
            curve_quiet(&mut self.e, self.clock, self.slot);
            print(&mut self.e, self.n, self.clock, self.slot);
            ticks(&mut self.e, 3);
        }
    }
    fn landing(&mut self, dsol: u64) {
        self.clock += 1_000;
        self.slot += 5;
        curve(&mut self.e, self.clock, self.slot, dsol);
    }
    fn rep(&self, k: &str) -> u64 {
        self.e.model_lane_report().iter().filter(|(key, _)| key.starts_with(k)).map(|(_, v)| *v).sum()
    }
}

#[test]
fn hold_reduce_exit_travel_over_the_production_client_and_only_fills_change_state() {
    // HOLD first (step 0), REDUCE at step 1, EXIT at step 2 — each answer decided by a REAL HTTP round trip.
    let ep = Endpoint::start(|step| match step {
        0 => HOLD,
        1 => REDUCE,
        _ => EXIT,
    });
    let mut r = rig(&ep, "hre");
    let inv0 = r.e.model_inventory_tokens(&MINT).expect("fill set inventory");
    let cash0 = r.e.model_free_cash_lamports();
    // Past the 60 s hold: step 0 -> HOLD. HOLD is a valid decision: no order, no endpoint failure.
    r.advance(75_000);
    assert!(r.rep("mgmt:verdict:hold") >= 1, "{:?}", r.e.model_lane_report());
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    assert_eq!(r.rep("endpoint:"), 0, "a valid HOLD is not endpoint failure");
    assert!(!r.e.model_safety_blocked());
    // Step 1 -> REDUCE: an order INTENT first.
    r.advance_to_order(60_000);
    let (id, _, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("REDUCE order pending");
    assert_eq!(filled, 0);
    assert_eq!(intended, inv0 / 2, "half of current inventory, floored");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0), "intent is not a fill");
    // PARTIAL fill: only the filled quantity changes.
    let part = intended / 3;
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000).unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - part));
    let (_, _, i2, f2) = r.e.model_mgmt_pending(&MINT).expect("remainder stays pending");
    assert_eq!((i2, f2), (intended, part));
    // Duplicate/over-sized report: refused, nothing moves.
    assert!(r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000).is_err());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - part));
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended - part, 22_000).unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - inv0 / 2));
    assert!(r.e.model_position_open(&MINT));
    // Cash rose by the sale proceeds and by nothing else; the book and the position agree.
    let cash1 = r.e.model_free_cash_lamports();
    assert!(cash1 > cash0, "REDUCE released cash: {cash0} -> {cash1}");
    // Step >=2 -> EXIT closes the remainder through a fill.
    r.advance_to_order(90_000);
    let (id2, k, intended2, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT order pending");
    assert_eq!(format!("{k:?}"), "Exit");
    assert_eq!(Some(intended2), r.e.model_inventory_tokens(&MINT));
    r.e.model_mgmt_apply_reconciled_fill(MINT, id2, intended2, 22_000).unwrap();
    assert!(!r.e.model_position_open(&MINT), "closed only by the reconciled fill");
    assert_eq!(r.e.model_inventory_tokens(&MINT), None);
    // Late duplicate of the closing fill: refused.
    assert!(r.e.model_mgmt_apply_reconciled_fill(MINT, id2, 1, 22_000).is_err());
    assert_eq!(r.e.model_mgmt_fills().len(), 3, "partial + remainder + exit, once each");
    assert!(ep.mgmt_requests.load(Ordering::SeqCst) >= 3);
}

#[test]
fn add_over_the_wire_targets_half_inventory_and_is_not_endpoint_failure() {
    let ep = Endpoint::start(|_| ADD);
    let mut r = rig(&ep, "add");
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.advance_to_order(120_000);
    let (_, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("ADD order pending");
    assert_eq!(intended, inv0 / 2, "inventory-based target, never account capital");
    assert_eq!(r.rep("mgmt:add_unsupported"), 0);
    assert_eq!(r.rep("endpoint:"), 0, "a valid ADD is a valid decision, not endpoint failure");
}

#[test]
fn a_malformed_or_hung_endpoint_trips_safety_off_but_keeps_management_and_protection() {
    let ep = Endpoint::start(|_| "this is not the trained grammar");
    let mut r = rig(&ep, "bad");
    r.advance(200_000);
    assert!(r.rep("endpoint:malformed") >= 3, "{:?}", r.e.model_lane_report());
    assert!(r.e.model_safety_blocked(), "three consecutive malformed answers trip the latch");
    assert!(r.e.model_position_open(&MINT), "tripping never closes or abandons a position");
    // The latch is on disk.
    assert!(std::fs::read_to_string(r.dir.join("safety.json")).unwrap().contains("\"blocked\": true")
        || std::fs::read_to_string(r.dir.join("safety.json")).unwrap().contains("\"blocked\":true"));
    // Hung endpoint: every ask has a bounded deadline (engine abandons at 3 s).
    let ep2 = Endpoint::start(|_| HOLD);
    let mut r2 = rig(&ep2, "hang");
    ep2.hang.store(true, Ordering::SeqCst);
    r2.advance(200_000);
    assert!(r2.rep("mgmt:request_abandoned_deadline") >= 1, "{:?}", r2.e.model_lane_report());
    assert!(r2.e.model_safety_blocked());
    assert!(r2.e.model_position_open(&MINT));
}

#[test]
fn a_stop_request_does_not_terminate_the_sole_protector_of_open_exposure() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "stop");
    let ack = r.dir.join("ACK");
    // Held position, no handoff: INCOMPLETE — the daemon must stay up.
    match handle_stop_request(&mut r.e, &ack) {
        StopGate::Incomplete(a) => assert!(a.held >= 1, "{a:?}"),
        other => panic!("must not complete with open exposure: {other:?}"),
    }
    assert!(r.e.model_safety_blocked(), "stop request blocks entries");
    assert!(r.e.model_position_open(&MINT), "stop never liquidates");
    // Explicit acknowledged protective handoff: now it may complete.
    std::fs::write(&ack, b"operator handoff").unwrap();
    assert_eq!(
        format!("{:?}", handle_stop_request(&mut r.e, &ack)),
        "CompleteHandedOff"
    );
    let _ = decide_stop; // pure rule is also covered by the unit tests
}

#[test]
fn the_latch_survives_process_recreation_and_rearm_is_refused_with_unresolved_exposure() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "restart");
    r.e.model_safety_trip_operator();
    assert!(r.e.model_safety_blocked());
    let path = r.dir.join("safety.json");
    drop(r.e);
    // "Process recreation": a brand-new engine adopts the file. Held state is NOT restored (that is
    // the stated remaining dependency); the latch is what is proven here.
    let mut e2 = Engine::new(cfg(), RunMode::Paper);
    let armed = arm_paper_model(&mut e2, &ep.url, &path);
    assert!(armed.blocked_at_start, "a restart never re-arms: {armed:?}");
    assert!(e2.model_safety_blocked());
    // The file still records the held position that the new process cannot yet restore.
    let held = pump_quant_app::safety_off::SafetyOff::read_held(&path);
    assert!(!held.is_empty(), "unfinished positions stay on record, never erased");
    // Explicit re-arm with an unresolved record is refused or succeeds only by named operator.
    let res = e2.model_safety_rearm("");
    assert!(res.is_err(), "an unnamed re-arm is refused");
}
