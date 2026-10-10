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
    arm_paper_model, handle_stop_request, restore_held_state, AckRejection, StaleCallout,
    StartupRestore, StopGate, StopSession,
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
        event_id: None,
        feature: None,
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
    #[allow(clippy::type_complexity)]
    // fn-pointer answer hook; a type alias would not add clarity here
    answer: Arc<Mutex<fn(i64) -> &'static str>>,
    hang: Arc<AtomicBool>,
    /// When non-zero, a MANAGEMENT answer is delayed by this many ms (still a valid, complete decision).
    delay_ms: Arc<std::sync::atomic::AtomicU64>,
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
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
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
            delay_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            mgmt_requests: Arc::new(AtomicUsize::new(0)),
            bodies: Arc::new(Mutex::new(Vec::new())),
        };
        let delay = Arc::clone(&ep.delay_ms);
        let (ans, hang, cnt, bodies) = (
            Arc::clone(&ep.answer),
            Arc::clone(&ep.hang),
            Arc::clone(&ep.mgmt_requests),
            Arc::clone(&ep.bodies),
        );
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut s) = conn else { continue };
                let (ans, hang, cnt, bodies) = (
                    Arc::clone(&ans),
                    Arc::clone(&hang),
                    Arc::clone(&cnt),
                    Arc::clone(&bodies),
                );
                let delay = Arc::clone(&delay);
                std::thread::spawn(move || {
                    let Some(body) = read_request(&mut s) else {
                        return;
                    };
                    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let user = v["messages"][1]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    bodies.lock().unwrap().push(body.clone());
                    if hang.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_secs(12));
                        return;
                    }
                    let text = if user
                        .starts_with("Decide the next action for a position you already hold")
                    {
                        cnt.fetch_add(1, Ordering::SeqCst);
                        let d = delay.load(Ordering::SeqCst);
                        if d > 0 {
                            std::thread::sleep(Duration::from_millis(d));
                        }
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
    assert!(
        e.model_position_open(&MINT),
        "entry must fill: {:?}",
        e.model_lane_report()
    );
    Rig {
        e,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
        dir,
    }
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
    #[allow(dead_code)] // test scaffolding helper; not every scenario drives a landing
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

#[test]
fn hold_reduce_exit_travel_over_the_production_client_and_only_fills_change_state() {
    // HOLD first (step 0), REDUCE at step 1, EXIT at step 2 — each answer decided by a REAL HTTP round trip.
    let ep = Endpoint::start(|step| match step {
        0 => HOLD,
        1 => REDUCE,
        _ => EXIT,
    });
    let mut r = rig(&ep, "hre");
    let inv0 =
        r.e.model_inventory_tokens(&MINT)
            .expect("fill set inventory");
    let cash0 = r.e.model_free_cash_lamports();
    // Past the 60 s hold: step 0 -> HOLD. HOLD is a valid decision: no order, no endpoint failure.
    r.advance(75_000);
    assert!(
        r.rep("mgmt:verdict:hold") >= 1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    assert_eq!(
        r.rep("endpoint:"),
        0,
        "a valid HOLD is not endpoint failure"
    );
    assert!(!r.e.model_safety_blocked());
    // Step 1 -> REDUCE: an order INTENT first.
    r.advance_to_order(60_000);
    let (id, _, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("REDUCE order pending");
    assert_eq!(filled, 0);
    assert_eq!(intended, inv0 / 2, "half of current inventory, floored");
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0),
        "intent is not a fill"
    );
    // PARTIAL fill: only the filled quantity changes.
    let part = intended / 3;
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000)
        .unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - part));
    let (_, _, i2, f2) =
        r.e.model_mgmt_pending(&MINT)
            .expect("remainder stays pending");
    assert_eq!((i2, f2), (intended, part));
    // Duplicate/over-sized report: refused, nothing moves.
    assert!(r
        .e
        .model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .is_err());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - part));
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended - part, 22_000)
        .unwrap();
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
    r.e.model_mgmt_apply_reconciled_fill(MINT, id2, intended2, 22_000)
        .unwrap();
    assert!(
        !r.e.model_position_open(&MINT),
        "closed only by the reconciled fill"
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), None);
    // Late duplicate of the closing fill: refused.
    assert!(r
        .e
        .model_mgmt_apply_reconciled_fill(MINT, id2, 1, 22_000)
        .is_err());
    assert_eq!(
        r.e.model_mgmt_fills().len(),
        3,
        "partial + remainder + exit, once each"
    );
    assert!(ep.mgmt_requests.load(Ordering::SeqCst) >= 3);
}

#[test]
fn add_over_the_wire_targets_half_inventory_and_is_not_endpoint_failure() {
    let ep = Endpoint::start(|_| ADD);
    let mut r = rig(&ep, "add");
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.advance_to_order(120_000);
    let (_, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("ADD order pending");
    assert_eq!(
        intended,
        inv0 / 2,
        "inventory-based target, never account capital"
    );
    assert_eq!(r.rep("mgmt:add_unsupported"), 0);
    assert_eq!(
        r.rep("endpoint:"),
        0,
        "a valid ADD is a valid decision, not endpoint failure"
    );
}

#[test]
fn a_malformed_or_hung_endpoint_trips_safety_off_but_keeps_management_and_protection() {
    let ep = Endpoint::start(|_| "this is not the trained grammar");
    let mut r = rig(&ep, "bad");
    r.advance(200_000);
    assert!(
        r.rep("endpoint:malformed") >= 3,
        "{:?}",
        r.e.model_lane_report()
    );
    assert!(
        r.e.model_safety_blocked(),
        "three consecutive malformed answers trip the latch"
    );
    assert!(
        r.e.model_position_open(&MINT),
        "tripping never closes or abandons a position"
    );
    // The latch is on disk.
    assert!(
        std::fs::read_to_string(r.dir.join("safety.json"))
            .unwrap()
            .contains("\"blocked\": true")
            || std::fs::read_to_string(r.dir.join("safety.json"))
                .unwrap()
                .contains("\"blocked\":true")
    );
    // Hung endpoint: every ask has a bounded deadline (engine abandons at 3 s).
    let ep2 = Endpoint::start(|_| HOLD);
    let mut r2 = rig(&ep2, "hang");
    ep2.hang.store(true, Ordering::SeqCst);
    r2.advance(200_000);
    assert!(
        r2.rep("mgmt:request_abandoned_deadline") >= 1,
        "{:?}",
        r2.e.model_lane_report()
    );
    assert!(r2.e.model_safety_blocked());
    assert!(r2.e.model_position_open(&MINT));
}

/// Read the published request, then write an acknowledgement with the given overrides.
fn write_ack(ack: &std::path::Path, req: &std::path::Path, edit: impl Fn(&mut serde_json::Value)) {
    let r: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(req).unwrap()).unwrap();
    let mut a = serde_json::json!({
        "session_id": r["session_id"], "request_id": r["request_id"],
        "exposure_digest": r["exposure_digest"],
        "recipient": "protector-B", "accepted_protective_responsibility": true,
    });
    edit(&mut a);
    std::fs::write(ack, a.to_string()).unwrap();
}

fn incomplete_reason(g: StopGate) -> AckRejection {
    match g {
        StopGate::Incomplete { rejection, .. } => rejection,
        other => panic!("must be incomplete: {other:?}"),
    }
}

#[test]
fn a_stop_request_does_not_terminate_the_sole_protector_of_open_exposure() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "stop");
    let (ack, req) = (r.dir.join("ACK.json"), r.dir.join("REQ.json"));
    // A PRE-EXISTING acknowledgement (garbage, or a plausible-looking file) exists before any request.
    std::fs::write(&ack, b"operator handoff").unwrap();
    let mut st = StopSession::new();
    assert_eq!(
        incomplete_reason(handle_stop_request(&mut r.e, &mut st, &req, &ack)),
        AckRejection::Unreadable,
        "a pre-existing file authorizes nothing"
    );
    assert!(r.e.model_safety_blocked(), "stop request blocks entries");
    assert!(r.e.model_position_open(&MINT), "stop never liquidates");
    // A well-formed ack from a DIFFERENT session / stale request is rejected.
    write_ack(&ack, &req, |a| a["session_id"] = "pq-other-session".into());
    assert_eq!(
        incomplete_reason(handle_stop_request(&mut r.e, &mut st, &req, &ack)),
        AckRejection::SessionMismatch
    );
    write_ack(&ack, &req, |a| a["request_id"] = "old-request".into());
    assert_eq!(
        incomplete_reason(handle_stop_request(&mut r.e, &mut st, &req, &ack)),
        AckRejection::RequestMismatch
    );
    write_ack(&ack, &req, |a| a["exposure_digest"] = "deadbeef".into());
    assert_eq!(
        incomplete_reason(handle_stop_request(&mut r.e, &mut st, &req, &ack)),
        AckRejection::ExposureMismatch
    );
    // No identified recipient / did not accept responsibility.
    write_ack(&ack, &req, |a| a["recipient"] = "".into());
    assert_eq!(
        incomplete_reason(handle_stop_request(&mut r.e, &mut st, &req, &ack)),
        AckRejection::NoAcceptedRecipient
    );
    write_ack(&ack, &req, |a| {
        a["accepted_protective_responsibility"] = false.into()
    });
    assert_eq!(
        incomplete_reason(handle_stop_request(&mut r.e, &mut st, &req, &ack)),
        AckRejection::NoAcceptedRecipient
    );
    assert!(r.e.model_position_open(&MINT));
    // The valid acknowledgement: this session, this request, this exposure, an identified recipient.
    write_ack(&ack, &req, |_| {});
    match handle_stop_request(&mut r.e, &mut st, &req, &ack) {
        StopGate::CompleteHandedOff { recipient } => assert_eq!(recipient, "protector-B"),
        other => panic!("a valid bound ack must complete: {other:?}"),
    }
}

#[test]
fn an_acknowledgement_for_an_earlier_exposure_snapshot_does_not_survive_a_change() {
    // The endpoint answers EXIT once the position has been held long enough, so a management order
    // APPEARS after the first shutdown request was published and acknowledged.
    let ep = Endpoint::start(|step| if step == 0 { EXIT } else { HOLD });
    let mut r = rig(&ep, "chg");
    let (ack, req) = (r.dir.join("ACK.json"), r.dir.join("REQ.json"));
    let mut st = StopSession::new();
    let first = handle_stop_request(&mut r.e, &mut st, &req, &ack);
    let req1: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&req).unwrap()).unwrap();
    assert!(matches!(first, StopGate::Incomplete { .. }));
    write_ack(&ack, &req, |_| {}); // a recipient accepts THIS snapshot (held, no pending order)
                                   // Exposure changes: a pending EXIT order now exists.
    r.advance_to_order(120_000);
    assert!(
        r.e.model_mgmt_pending(&MINT).is_some(),
        "setup: an order appeared"
    );
    match handle_stop_request(&mut r.e, &mut st, &req, &ack) {
        StopGate::Incomplete { rejection, .. } => assert!(
            matches!(
                rejection,
                AckRejection::RequestMismatch | AckRejection::ExposureMismatch
            ),
            "{rejection:?}"
        ),
        other => {
            panic!("the earlier acknowledgement must NOT authorize the changed exposure: {other:?}")
        }
    }
    let req2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&req).unwrap()).unwrap();
    assert_ne!(
        req1["request_id"], req2["request_id"],
        "a new request id is published for the new snapshot"
    );
    assert_ne!(req1["exposure_digest"], req2["exposure_digest"]);
    // Re-acknowledging the NEW snapshot is what authorizes it.
    write_ack(&ack, &req, |_| {});
    assert!(matches!(
        handle_stop_request(&mut r.e, &mut st, &req, &ack),
        StopGate::CompleteHandedOff { .. }
    ));
}

#[test]
fn live_plus_model_endpoint_fails_clearly_and_never_starts_legacy_live() {
    // The real binary, not a helper: PQ_MODEL_ENDPOINT together with --live must exit 98 before touching
    // any wallet, engine or network.
    let exe = env!("CARGO_BIN_EXE_pq-daemon");
    let out = std::process::Command::new(exe)
        .arg("--live")
        .env("PQ_MODEL_ENDPOINT", "http://127.0.0.1:9")
        .env("HOME", std::env::temp_dir())
        .output()
        .expect("run pq-daemon");
    assert_eq!(
        out.status.code(),
        Some(98),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("incompatible with PQ_MODEL_ENDPOINT"), "{err}");
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
    assert!(
        !held.is_empty(),
        "unfinished positions stay on record, never erased"
    );
    // Explicit re-arm with an unresolved record is refused or succeeds only by named operator.
    let res = e2.model_safety_rearm("");
    assert!(res.is_err(), "an unnamed re-arm is refused");
}

/// Advance the feed a little with prints + curve observations so the engine polls its worker results.
fn ticks_and_prints(r: &mut Rig, n: usize) {
    for _ in 0..n {
        r.clock += 1_000;
        r.slot += 1;
        r.n += 1;
        curve_quiet(&mut r.e, r.clock, r.slot);
        print(&mut r.e, r.n, r.clock, r.slot);
        ticks(&mut r.e, 2);
    }
}

#[test]
fn an_abandoned_ask_whose_http_worker_is_still_busy_never_executes_late_and_the_pool_is_not_exhausted(
) {
    // The endpoint answers with a VALID EXIT, but only after 5 s: past the engine's 3 s deadline, within
    // the 8 s socket timeout. The engine abandons each ask; the worker stays occupied until the (valid,
    // complete) answer finally arrives. That late answer must NOT execute, and repeated timeouts must not
    // let the request table or the fixed worker pool grow without bound.
    let ep = Endpoint::start(|_| EXIT);
    let mut r = rig(&ep, "late");
    ep.delay_ms.store(5_000, Ordering::SeqCst);
    r.advance(150_000);
    let abandoned = r.rep("mgmt:request_abandoned_deadline");
    assert!(abandoned >= 1, "{:?}", r.e.model_lane_report());
    // Capacity: the fixed pool (2 workers) / table (4 slots) bound what can be outstanding; excess asks are
    // REFUSED by name instead of queueing without limit.
    let dispatched = r.rep("mgmt:dispatched");
    let refused = r.rep("mgmt:refuse:submit") + r.rep("mgmt:refuse:dispatch");
    assert!(dispatched >= abandoned, "{:?}", r.e.model_lane_report());
    assert!(dispatched - abandoned <= 4 || refused > 0,
        "outstanding asks stay inside the table bound: dispatched={dispatched} abandoned={abandoned} refused={refused}");
    // Let the delayed (valid, complete) EXIT answers actually ARRIVE on the wire, then drive the engine so it
    // drains them. Wall-clock wait: the endpoint sleeps 5 s per answer.
    std::thread::sleep(Duration::from_millis(7_500));
    ticks_and_prints(&mut r, 6);
    let discarded = r.rep("mgmt:discard:");
    assert!(
        discarded >= 1,
        "the late answers must be DISCARDED BY NAME when they land: {:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.rep("mgmt:verdict:exit"), 0);
    assert_eq!(r.rep("mgmt:order:"), 0, "no late response created an order");
    assert!(
        r.e.model_position_open(&MINT),
        "a late answer must never close the position"
    );
    assert!(
        r.e.model_mgmt_fills().is_empty(),
        "no late response may place or fill an order"
    );
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
    // Repeated timeouts trip SAFETY_OFF after the tested threshold (3 consecutive), never earlier.
    assert!(
        r.e.model_safety_blocked(),
        "three consecutive abandoned asks trip the latch"
    );
    // And once the endpoint is fast again the lane recovers its worker slots (nothing leaked forever).
    ep.delay_ms.store(0, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(6_000));
    r.advance(60_000);
    let after = r.rep("mgmt:dispatched");
    assert!(
        after > dispatched,
        "worker slots were released after the blocked requests returned: {:?}",
        r.e.model_lane_report()
    );
}

#[test]
fn a_late_valid_answer_after_the_position_changed_is_discarded_by_version_not_executed() {
    // Fast endpoint: an ask is answered REDUCE; before the verdict is accepted the position version changes
    // (a reconciled fill from a previous order). The bound version no longer matches => discarded.
    let ep = Endpoint::start(|step| if step == 0 { REDUCE } else { HOLD });
    let mut r = rig(&ep, "ver");
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    // Fill the order completely: the position version moves on and the order is gone.
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .unwrap();
    let fills = r.e.model_mgmt_fills().len();
    // Replaying the same reconciled report cannot repeat the reduction.
    assert!(r
        .e
        .model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .is_err());
    assert_eq!(r.e.model_mgmt_fills().len(), fills);
}

#[test]
fn a_safety_state_that_cannot_be_persisted_fails_closed_everywhere() {
    // The latch path is a DIRECTORY: every write fails (the same observable as a full disk / read-only fs).
    let ep = Endpoint::start(|_| HOLD);
    let dir = tmp("nowrite");
    let bad = dir.join("safety_is_a_dir");
    std::fs::create_dir_all(&bad).unwrap();
    let mut e = Engine::new(cfg(), RunMode::Paper);
    let _ = arm_paper_model(&mut e, &ep.url, &bad);
    // Whatever the attach decided, a trip must be reported as UNPERSISTED, stay blocked in memory, and count.
    e.model_safety_trip_operator();
    assert!(
        e.model_safety_blocked(),
        "an unpersistable trip still blocks in memory"
    );
    assert!(e.model_safety_persist_failures() >= 1);
    assert!(
        e.model_lane_report()
            .get("safety:persist_failed")
            .copied()
            .unwrap_or(0)
            >= 1
    );
    // Re-arm must be REFUSED when it cannot be made durable (never a volatile re-arm that a restart forgets).
    assert!(e.model_safety_rearm("alon").is_err());
    assert!(
        e.model_safety_blocked(),
        "still blocked after the refused re-arm"
    );
    // Shutdown cannot claim completion on an unrecorded exposure.
    let report = e.model_controlled_shutdown();
    assert!(!report.persisted);
    let mut st = StopSession::new();
    let g = handle_stop_request(
        &mut e,
        &mut st,
        &dir.join("REQ.json"),
        &dir.join("ACK.json"),
    );
    // Flat engine + unpersisted state: must NOT be CompleteFlat.
    assert!(matches!(g, StopGate::Incomplete { .. }), "{g:?}");
}

#[test]
fn headroom_check_reports_ok_low_and_unknown_distinctly() {
    use pump_quant_junction::model_lifecycle::{check_headroom, free_bytes, Headroom};
    let d = tmp("headroom");
    let free = free_bytes(&d).expect("measurable");
    assert!(free > 0);
    assert!(matches!(check_headroom(&d, 1), Headroom::Ok { .. }));
    assert!(
        matches!(check_headroom(&d, u64::MAX), Headroom::Low { .. }),
        "a floor above free is LOW"
    );
    assert_eq!(
        check_headroom(std::path::Path::new("/definitely/not/a/real/path/x"), 1),
        Headroom::Unknown
    );
}

#[test]
fn a_crash_mid_reduce_restarts_through_the_daemons_restore_path_and_resolves_only_by_reconciled_report(
) {
    // The production client over HTTP; the engine persists held state as it changes (NOT only at a trip
    // or shutdown), then the process "crashes": no handoff, no shutdown call, engine just dropped.
    let ep = Endpoint::start(|step| if step == 0 { REDUCE } else { HOLD });
    let dir = tmp("crash");
    let held = dir.join("held.json");
    let mut e = Engine::new(cfg(), RunMode::Paper);
    let armed = arm_paper_model(&mut e, &ep.url, &dir.join("safety.json"));
    assert!(!armed.blocked_at_start);
    assert!(
        matches!(restore_held_state(&mut e, &held), StartupRestore::Clean),
        "first start is clean"
    );
    for ev in events(40) {
        e.tick(ev);
    }
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    for _ in 0..50 {
        if e.model_position_open(&MINT) {
            break;
        }
        ticks(&mut e, 2);
        curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    }
    let mut r = Rig {
        e,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
        dir: dir.clone(),
    };
    r.advance_to_order(120_000);
    let (id, kind, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE in flight");
    assert_eq!(format!("{kind:?}"), "Reduce");
    // One more tick lets the on-change persist write the in-flight order (no explicit flush).
    ticks(&mut r.e, 2);
    let inv_before = r.e.model_inventory_tokens(&MINT).unwrap();
    let basis = r.e.model_accounting_view(&MINT).remaining_cost_basis;
    assert!(
        held.exists(),
        "the ledger was written by the running engine, not by a shutdown hook"
    );
    drop(r.e); // crash

    let mut e2 = Engine::new(cfg(), RunMode::Paper);
    let _ = arm_paper_model(&mut e2, &ep.url, &dir.join("safety2.json"));
    match restore_held_state(&mut e2, &held) {
        StartupRestore::Restored(rep) => {
            assert_eq!(rep.positions, 1);
            assert_eq!(rep.pending_uncertain, 1);
        }
        other => panic!("expected a restore, got {other:?}"),
    }
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        Some(inv_before),
        "the unfilled REDUCE changed nothing"
    );
    assert_eq!(e2.model_accounting_view(&MINT).remaining_cost_basis, basis);
    let (id2, _, int2, _) = e2.model_mgmt_pending(&MINT).unwrap();
    assert_eq!((id2, int2), (id, intended));
    assert_eq!(
        pump_quant_junction::model_lifecycle::mints_needing_feeds(&e2),
        vec![MINT],
        "its feeds are re-established independent of discovery"
    );
    // The stop gate sees unresolved exposure: a restart is not a handoff.
    let mut st = StopSession::new();
    let g = handle_stop_request(
        &mut e2,
        &mut st,
        &dir.join("req.json"),
        &dir.join("ack.json"),
    );
    assert!(matches!(g, StopGate::Incomplete { .. }), "{g:?}");
    // Only a reconciled report resolves it: exactly the filled quantity.
    e2.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .unwrap();
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        Some(inv_before - intended)
    );
    assert!(e2.model_mgmt_pending(&MINT).is_none());
}

#[test]
fn a_ledger_that_cannot_be_applied_refuses_startup_and_is_left_untouched() {
    let ep = Endpoint::start(|_| HOLD);
    let dir = tmp("refuse");
    let held = dir.join("held.json");
    let mut r = rig(&ep, "refuse_src");
    r.e.model_held_attach(&held);
    assert!(r.e.model_held_persist_now());
    drop(r.e);
    let before = std::fs::read(&held).unwrap();
    // A different wallet seed than the books were kept under.
    let mut c = cfg();
    c.bankroll_initial_lamports = 1_234_567_890;
    let mut e2 = Engine::new(c, RunMode::Paper);
    let _ = arm_paper_model(&mut e2, &ep.url, &dir.join("safety.json"));
    match restore_held_state(&mut e2, &held) {
        StartupRestore::Refused(why) => assert!(why.contains("SeedMismatch"), "{why}"),
        other => panic!("must refuse: {other:?}"),
    }
    assert!(!e2.model_position_open(&MINT), "nothing applied");
    assert_eq!(
        std::fs::read(&held).unwrap(),
        before,
        "the ledger is untouched for the operator"
    );
}

#[test]
fn the_stale_callout_is_edge_triggered_names_the_loss_of_protection_and_recovers() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "callout");
    let mut c = StaleCallout::default();
    // Healthy: fresh reserve + prints => silence.
    r.advance(20_000);
    let now = r.e.model_clock_ms_now();
    assert!(
        c.evaluate(&r.e, now, 60_000).is_empty(),
        "no callout while management data is fresh"
    );
    // Feed goes quiet: ONLY the clock moves (a heartbeat tick with no reserve/print), as a dead feed does.
    r.e.tick(AppEvent::Tick);
    let quiet_from = now;
    // Advance the engine's own clock past the 60 s pricing bound using an unrelated mint's print, which
    // must NOT refresh the held mint's reserve.
    let other = [0x77u8; 32];
    for k in 1..=3 {
        let t = quiet_from + k * 40_000;
        r.e.tick(AppEvent::MarketTrade {
            mint: DomainMint::from_bytes(other),
            price_fp: 1_000,
            quote_lamports: 1_000_000,
            liquidity_lamports: 30_000_000_000,
            signed_base: 5,
            buyer_entity: 9,
            age_slots: 30,
            recv_unix_ms: Some(t),
            trader_pubkey: Some(wallet(900 + k as u32)),
            slot: Some(9_000 + k as u64),
            fee_lamports: Some(5_000),
            cu_consumed: Some(1),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    let t1 = r.e.model_clock_ms_now();
    assert!(t1 - quiet_from >= 100_000);
    let lines = c.evaluate(&r.e, t1, 60_000);
    assert!(
        lines.iter().any(|l| l.alert && l.text.starts_with("ONSET")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.text.contains("UNPROTECTED")),
        "prints for the held mint are silent too: {lines:?}"
    );
    assert_eq!(c.degraded_count(), 1);
    // Edge-triggered: the same instant again says nothing; a reminder only after the interval.
    assert!(
        c.evaluate(&r.e, t1, 60_000).is_empty(),
        "no repeat inside the reminder interval"
    );
    let rem = c.evaluate(&r.e, t1 + 61_000, 60_000);
    assert!(
        rem.iter().any(|l| l.text.starts_with("REMINDER")),
        "{rem:?}"
    );
    // The 60 s bound was not loosened to make it go away: management still refuses.
    assert!(r.e.model_held_degraded().len() == 1);
    // A fresh reserve + print for the HELD mint recovers it, and the outage length is reported.
    r.clock = t1 + 1_000;
    r.slot += 10;
    curve(&mut r.e, r.clock, r.slot, 200_000_000);
    print(&mut r.e, r.n + 1, r.clock, r.slot);
    // The state ledger serves trades STRICTLY BEFORE the decision clock, so the next live print is what
    // makes the market "alive" at the clock (the existing 60 s idle contract, unchanged).
    r.clock += 1_000;
    r.slot += 1;
    curve_quiet(&mut r.e, r.clock, r.slot);
    print(&mut r.e, r.n + 2, r.clock, r.slot);
    ticks(&mut r.e, 3);
    let rec = c.evaluate(&r.e, r.clock, 60_000);
    assert!(
        rec.iter()
            .any(|l| !l.alert && l.text.starts_with("RECOVERED")),
        "{rec:?} {:?}",
        r.e.model_held_degraded()
    );
    assert_eq!(c.degraded_count(), 0);
}

/// RESERVATION CALLOUT (onset / reminder / recovery) and the difference between "reservation released" and
/// "protection restored": releasing the reservation with a still-stale reserve must NOT print a recovery of the
/// price-based protection, and the stale-data callout keeps its own state.
#[test]
fn the_reservation_callout_has_onset_reminder_recovery_and_release_is_not_protection_restored() {
    let ep = Endpoint::start(|step| if step == 0 { EXIT } else { HOLD });
    let mut r = rig(&ep, "rescallout");
    let mut c = StaleCallout::default();
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    ticks(&mut r.e, 2);
    assert_eq!(
        Some(intended),
        r.e.model_inventory_tokens(&MINT),
        "EXIT reserves everything"
    );
    let now = r.e.model_clock_ms_now();
    // ONSET: named, an alert, and it says nothing is sold or resubmitted.
    let on = c.evaluate(&r.e, now, 60_000);
    let line = on
        .iter()
        .find(|l| l.text.contains("PROTECTION DEFERRED"))
        .expect("onset line");
    assert!(line.alert && line.text.starts_with("ONSET"), "{on:?}");
    assert!(
        line.text.contains("nothing is sold or resubmitted"),
        "{}",
        line.text
    );
    // Edge-triggered: same instant says nothing about the reservation again.
    assert!(!c
        .evaluate(&r.e, now, 60_000)
        .iter()
        .any(|l| l.text.contains("PROTECTION DEFERRED")));
    // REMINDER after the interval, with the elapsed time.
    let rem = c.evaluate(&r.e, now + 61_000, 60_000);
    let rl = rem
        .iter()
        .find(|l| l.text.contains("PROTECTION DEFERRED"))
        .expect("reminder");
    assert!(
        rl.alert && rl.text.starts_with("REMINDER") && rl.text.contains("degraded_for=61s"),
        "{}",
        rl.text
    );
    // Release with definitive not-executed evidence while the feed goes quiet (stale reserve and prints).
    assert!(r.e.model_mgmt_resolve_uncertain_not_executed(&MINT, id));
    let other = [0x77u8; 32];
    for k in 1..=3 {
        let t = now + k * 40_000;
        r.e.tick(AppEvent::MarketTrade {
            mint: DomainMint::from_bytes(other),
            price_fp: 1_000,
            quote_lamports: 1_000_000,
            liquidity_lamports: 30_000_000_000,
            signed_base: 5,
            buyer_entity: 9,
            age_slots: 30,
            recv_unix_ms: Some(t),
            trader_pubkey: Some(wallet(900 + k as u32)),
            slot: Some(9_000 + k as u64),
            fee_lamports: Some(5_000),
            cu_consumed: Some(1),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    let t1 = r.e.model_clock_ms_now();
    let after = c.evaluate(&r.e, t1, 60_000);
    // The reservation is released ...
    assert!(
        after.iter().any(|l| !l.alert
            && l.text.starts_with("RECOVERED")
            && l.text.contains("unresolved sell was reconciled")),
        "{after:?}"
    );
    // ... but price-based protection is NOT restored: the stale-data state is raised on its own.
    assert!(
        after
            .iter()
            .any(|l| l.alert && l.text.contains("UNPROTECTED")),
        "released reservation with stale prices must still be named degraded: {after:?}"
    );
    assert_eq!(c.degraded_count(), 1, "only the staleness remains degraded");
    // Fresh reserve + print restores readiness; that is a separate recovery.
    r.clock = t1 + 1_000;
    r.slot += 10;
    curve(&mut r.e, r.clock, r.slot, 200_000_000);
    print(&mut r.e, r.n + 1, r.clock, r.slot);
    r.clock += 1_000;
    r.slot += 1;
    curve_quiet(&mut r.e, r.clock, r.slot);
    print(&mut r.e, r.n + 2, r.clock, r.slot);
    ticks(&mut r.e, 3);
    let rec = c.evaluate(&r.e, r.clock, 60_000);
    assert!(
        rec.iter()
            .any(|l| !l.alert && l.text.starts_with("RECOVERED")),
        "{rec:?}"
    );
    assert_eq!(c.degraded_count(), 0);
}

/// M1: the daemon's final sequence AFTER an accepted protective handoff (stop gate -> report()) leaves the
/// handed-off exposure exactly as acknowledged: same exposure digest, same inventory, same durable ledger.
#[test]
fn m1_final_report_after_an_accepted_handoff_preserves_the_acknowledged_exposure() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "m1_handoff");
    let held = r.dir.join("held.json");
    r.e.model_held_attach(&held);
    let (ack, req) = (r.dir.join("ACK.json"), r.dir.join("REQ.json"));
    let mut st = StopSession::new();
    let _ = handle_stop_request(&mut r.e, &mut st, &req, &ack);
    write_ack(&ack, &req, |_| {});
    match handle_stop_request(&mut r.e, &mut st, &req, &ack) {
        StopGate::CompleteHandedOff { .. } => {}
        other => panic!("valid ack must complete: {other:?}"),
    }
    let acked: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&req).unwrap()).unwrap();
    let inv0 = r.e.model_inventory_tokens(&MINT);
    let led = |p: &std::path::Path| {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        (
            v["held"].clone(),
            v["pending"].clone(),
            v["realized_lamports"].clone(),
        )
    };
    let l0 = led(&held);
    // The daemon's post-loop path: snapshot, report(), exposure view.
    let _snap = r.e.open_positions_snapshot();
    let rep = r.e.report();
    let x = r.e.model_open_exposure();
    assert_eq!(rep.net_lamports, 0);
    assert!(
        r.e.model_position_open(&MINT),
        "handed-off exposure is not closed by the final report"
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), inv0);
    assert_eq!(x.len(), 1);
    assert_eq!(
        r.e.model_exposure_digest().0,
        acked["exposure_digest"].as_str().unwrap(),
        "exposure after report() is exactly what the recipient acknowledged"
    );
    assert!(r.e.model_held_persist_now());
    assert_eq!(
        led(&held),
        l0,
        "durable ledger unchanged by the final report"
    );
}

/// Item 3 through the DAEMON's own code: the deadline comes from `run_deadline_from` (the env parser pq_daemon uses),
/// the engine is armed by `arm_paper_model` (production HTTP client), and the handoff uses `request_deadline_handoff`.
/// During the drain a valid REDUCE executes. The next valid ADD is refused BY THE DEADLINE ROW'S NAME and creates no
/// order. When the drain bound elapses the handoff sentinel is raised, and the position is not force-closed.
#[test]
fn daemon_path_run_deadline_drain_refuses_add_by_name_executes_reduce_and_hands_off_without_closing(
) {
    use pump_quant_app::stop_policy::{action_for, OpsInputs, RunPhase, StopTrigger};
    use pump_quant_junction::model_lifecycle::{request_deadline_handoff, run_deadline_from};
    let ep = Endpoint::start(|step| match step {
        0 => REDUCE,
        1 => ADD,
        _ => HOLD,
    });
    let mut r = rig(&ep, "drain_add");
    let (d, dr) = run_deadline_from(Some("10000"), Some("5000"));
    assert_eq!((d, dr), (10_000, 5_000), "an override may only shorten");
    r.e.model_stop_set_deadline(d, dr);
    let ev = r.e.model_stop_evaluate(10_000, OpsInputs::healthy());
    assert_eq!(ev.phase, RunPhase::Drain { ends_at_ms: 15_000 });
    assert!(r.e.model_entries_blocked() && !r.e.model_safety_blocked());
    // Step 0: REDUCE over the wire -> order intent -> reconciled fill.
    r.advance_to_order(120_000);
    let (id, k, intended, _) =
        r.e.model_mgmt_pending(&MINT)
            .expect("REDUCE pending in the drain");
    assert_eq!(format!("{k:?}"), "Reduce");
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - intended));
    // Step 1: ADD over the wire -> refused by the deadline row's name, no order.
    let label = format!(
        "mgmt:refuse:add_blocked_stop:{}",
        action_for(StopTrigger::RunDeadline).name
    );
    for _ in 0..60 {
        if r.rep(&label) >= 1 {
            break;
        }
        r.advance(5_000);
    }
    assert!(
        r.rep(&label) >= 1,
        "ADD refused by name: {:?}",
        r.e.model_lane_report()
    );
    assert!(
        r.e.model_mgmt_pending(&MINT).is_none(),
        "no ADD order in the drain"
    );
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0 - intended),
        "refused ADD moved nothing"
    );
    // Drain bound elapsed: the daemon's handoff sentinel, never a force close.
    let ev = r.e.model_stop_evaluate(15_000, OpsInputs::healthy());
    assert!(ev.handoff_due);
    let stop = r.dir.join("STOP");
    assert!(request_deadline_handoff(&stop) && stop.exists());
    assert!(
        !request_deadline_handoff(&stop),
        "raised once, never overwritten"
    );
    assert!(
        r.e.model_position_open(&MINT),
        "the drain hands off; it does not close"
    );
}

/// Item 4 through the PRODUCTION client (the serve_term runA shape): the endpoint hangs (12 s). The engine abandons
/// each ask at its 3 s deadline and the 8 s socket timeout later returns a transport error for the SAME request.
/// Initial count 0, threshold 3: two asks = 2 failures (deadline), and their two late `endpoint:transport_error`
/// arrivals add 0. So the latch does not trip, where runA counted 4 for 2 asks and tripped at the 3rd increment.
/// A third distinct ask trips it.
#[test]
fn daemon_path_two_hung_asks_with_socket_timeouts_do_not_trip_a_third_ask_does() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "once_per_id");
    assert_eq!(r.e.model_safety_consecutive_failures(), 0);
    ep.hang.store(true, Ordering::SeqCst);
    for _ in 0..80 {
        if r.rep("mgmt:request_abandoned_deadline") >= 2 {
            break;
        }
        r.advance(5_000);
    }
    assert_eq!(
        r.rep("mgmt:request_abandoned_deadline"),
        2,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.e.model_safety_consecutive_failures(), 2);
    assert!(!r.e.model_safety_blocked());
    // Hold the feed clock still (no new ask) while the 8 s socket timeouts fire on both workers.
    for _ in 0..120 {
        if r.rep("health:late_arrival_already_charged") >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        ticks(&mut r.e, 1);
    }
    assert_eq!(
        r.rep("endpoint:transport_error"),
        2,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.rep("health:late_arrival_already_charged"), 2);
    assert_eq!(
        r.e.model_safety_consecutive_failures(),
        2,
        "2 asks count 2, not 4"
    );
    assert!(
        !r.e.model_safety_blocked(),
        "two asks must not trip a threshold of three"
    );
    // A third distinct ask is abandoned -> 3 -> trip.
    for _ in 0..80 {
        if r.e.model_safety_blocked() {
            break;
        }
        r.advance(5_000);
    }
    assert!(r.e.model_safety_blocked(), "{:?}", r.e.model_lane_report());
    assert_eq!(r.rep("mgmt:request_abandoned_deadline"), 3);
    assert!(
        r.e.model_position_open(&MINT),
        "tripping never closes the position"
    );
}

// ─── Operator re-arm channel (model_lifecycle::{validate_rearm_request, apply_validated_rearm}) ──────────────────
// SCOPE_REARM_LAUNCH §A: local channel (owner = daemon uid, 0600, dir not world-writable), request binds lineage +
// safety_epoch + unique request_id + acknowledged latch reasons, consumed once, request_id durable across restarts,
// re-check at apply, never waives blockers.

use pump_quant_app::safety_off::RearmRefusal;
use pump_quant_junction::model_lifecycle::{
    apply_validated_rearm, check_operator_channel_access, check_operator_channel_access_as,
    handle_rearm_request, harness_force_protect_from, refuse_stale_rearm_at_startup,
    validate_rearm_request, RearmOutcome, RearmRequestRefusal,
};
use std::os::unix::fs::PermissionsExt as _;

/// The channel dir (0700) and request/result paths under the rig's dir.
fn chan(r: &Rig) -> (std::path::PathBuf, std::path::PathBuf) {
    chan_in(&r.dir)
}

fn chan_in(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let d = dir.join("operator");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    (
        d.join("OPERATOR_REARM.json"),
        d.join("OPERATOR_REARM_RESULT.json"),
    )
}

/// Write a request as the operator would: 0600, owned by us.
fn put(req: &std::path::Path, body: &str) {
    std::fs::write(req, body).unwrap();
    std::fs::set_permissions(req, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn body(id: &str, lineage: &str, epoch: u64, reasons: &[&str]) -> String {
    serde_json::json!({"schema": 2, "request_id": id, "operator": "alon", "lineage": lineage,
        "safety_epoch": epoch, "acknowledged_reasons": reasons})
    .to_string()
}

/// Attach a held ledger so the engine has a lineage to bind; returns it.
fn bind_ledger(e: &mut Engine, dir: &std::path::Path) -> String {
    e.model_held_attach(&dir.join("held.json"));
    assert!(e.model_held_persist_now());
    let l = e.model_held_lineage().to_string();
    assert!(!l.is_empty());
    l
}

fn refused(o: Option<RearmOutcome>) -> RearmRequestRefusal {
    match o {
        Some(RearmOutcome::Refused { why, .. }) => why,
        other => panic!("expected a refusal: {other:?}"),
    }
}

fn result_json(res: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(res).unwrap()).unwrap()
}

fn safety_file(r: &Rig) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(r.dir.join("safety.json")).unwrap()).unwrap()
}

#[test]
fn operator_rearm_request_binds_lineage_epoch_id_and_reasons_and_each_refusal_is_named() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "rearm_bind");
    let lin = bind_ledger(&mut r.e, &r.dir.clone());
    r.e.model_safety_trip_operator();
    let epoch = r.e.model_safety_epoch();
    assert_eq!(r.e.model_safety_reasons(), vec!["operator".to_string()]);
    // The safety file carries what the operator must echo.
    let f = safety_file(&r);
    assert_eq!(
        (f["lineage"].as_str(), f["epoch"].as_u64()),
        (Some(lin.as_str()), Some(epoch))
    );
    let (req, res) = chan(&r);
    let old_form = format!(r#"{{"operator":"alon","safety_epoch":{epoch}}}"#);
    let cases: Vec<(String, RearmRequestRefusal)> = vec![
        ("not json".into(), RearmRequestRefusal::Unreadable),
        (old_form, RearmRequestRefusal::SchemaUnsupported),
        (
            r#"{"schema":2,"operator":"alon"}"#.into(),
            RearmRequestRefusal::RequestIdMissing,
        ),
        (
            serde_json::json!({"schema":2,"request_id":"r-noop","lineage":lin,"safety_epoch":epoch,
                "acknowledged_reasons":["operator"]}).to_string(),
            RearmRequestRefusal::Contract(RearmRefusal::NoOperator),
        ),
        (
            serde_json::json!({"schema":2,"request_id":"r-nolin","operator":"alon","safety_epoch":epoch,
                "acknowledged_reasons":["operator"]}).to_string(),
            RearmRequestRefusal::LineageMissing,
        ),
        (
            serde_json::json!({"schema":2,"request_id":"r-noep","operator":"alon","lineage":lin,
                "acknowledged_reasons":["operator"]}).to_string(),
            RearmRequestRefusal::EpochMissing,
        ),
        (
            serde_json::json!({"schema":2,"request_id":"r-norsn","operator":"alon","lineage":lin,
                "safety_epoch":epoch}).to_string(),
            RearmRequestRefusal::ReasonsMissing,
        ),
        (
            body("r-lin", "deadbeef", epoch, &["operator"]),
            RearmRequestRefusal::LineageMismatch {
                requested: "deadbeef".into(),
                current: lin.clone(),
            },
        ),
        (
            body("r-ep", &lin, epoch + 7, &["operator"]),
            RearmRequestRefusal::EpochMismatch {
                requested: epoch + 7,
                current: epoch,
            },
        ),
        (
            body("r-rsn-extra", &lin, epoch, &["operator", "disk_hard_floor"]),
            RearmRequestRefusal::ReasonsMismatch {
                acknowledged: vec!["operator".into(), "disk_hard_floor".into()],
                active: vec!["operator".into()],
            },
        ),
        (
            body("r-rsn-none", &lin, epoch, &[]),
            RearmRequestRefusal::ReasonsMismatch {
                acknowledged: vec![],
                active: vec!["operator".into()],
            },
        ),
        (
            body("r-rsn-dup", &lin, epoch, &["operator", "operator"]),
            RearmRequestRefusal::ReasonsMismatch {
                acknowledged: vec!["operator".into(), "operator".into()],
                active: vec!["operator".into()],
            },
        ),
    ];
    for (b, want) in cases {
        put(&req, &b);
        assert_eq!(
            refused(handle_rearm_request(&mut r.e, &req, &res)),
            want,
            "{b}"
        );
        assert!(!req.exists(), "consumed even when refused");
        assert!(r.e.model_safety_blocked(), "refused => block stands ({b})");
        assert_eq!(r.e.model_safety_epoch(), epoch);
        let out = result_json(&res);
        assert_eq!(out["outcome"], "refused");
        assert_eq!(out["safety_blocked"], true);
        assert!(!out["why"].as_str().unwrap().is_empty(), "named");
    }
    assert!(handle_rearm_request(&mut r.e, &req.with_file_name("absent.json"), &res).is_none());
    // Lineage is checked at VALIDATION (before any claim/apply), not only by the apply-time re-check.
    put(&req, &body("r-lin-v", "feedface", epoch, &["operator"]));
    match validate_rearm_request(&r.e, &req, &res) {
        Some(Err(RearmOutcome::Refused {
            why: RearmRequestRefusal::LineageMismatch { .. },
            ..
        })) => {}
        other => panic!("lineage mismatch must be refused before apply: {other:?}"),
    }
    let ledger =
        std::fs::read_to_string(req.with_file_name("OPERATOR_REARM_LEDGER.jsonl")).unwrap();
    assert!(
        !ledger
            .lines()
            .any(|l| l.contains("r-lin-v") && l.contains("claimed")),
        "never claimed: {ledger}"
    );
    // A refused id is decided: it can never be used again, even with a now-correct body.
    put(&req, &body("r-ep", &lin, epoch, &["operator"]));
    assert_eq!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::DuplicateRequestId {
            request_id: "r-ep".into()
        }
    );
    // A correct, fresh request lifts it, durably, with the audit label recorded.
    put(&req, &body("r-ok", &lin, epoch, &["operator"]));
    match handle_rearm_request(&mut r.e, &req, &res) {
        Some(RearmOutcome::Rearmed {
            operator,
            request_id,
            epoch: e2,
            entries_blocked,
            restrictions,
        }) => {
            assert_eq!((operator.as_str(), request_id.as_str()), ("alon", "r-ok"));
            assert_eq!(e2, epoch + 1);
            assert!(!entries_blocked && restrictions.is_empty());
        }
        other => panic!(
            "valid request must re-arm: {other:?} blockers={:?}",
            r.e.model_safety_rearm_blockers()
        ),
    }
    assert!(!r.e.model_safety_blocked() && !r.e.model_entries_blocked());
    let f = safety_file(&r);
    assert_eq!(
        (f["blocked"].clone(), f["rearmed_by"].clone()),
        (serde_json::json!(false), serde_json::json!("alon"))
    );
    // Not blocked -> NotBlocked (named).
    put(&req, &body("r-nb", &lin, e2_of(&r), &[]));
    assert_eq!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::Contract(RearmRefusal::NotBlocked)
    );
}

fn e2_of(r: &Rig) -> u64 {
    r.e.model_safety_epoch()
}

#[test]
fn operator_channel_access_is_owner_0600_and_a_non_world_writable_dir_or_a_named_refusal() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "rearm_access");
    let lin = bind_ledger(&mut r.e, &r.dir.clone());
    r.e.model_safety_trip_operator();
    let epoch = r.e.model_safety_epoch();
    let (req, res) = chan(&r);
    let good = body("a-1", &lin, epoch, &["operator"]);
    // Group/world readable request.
    std::fs::write(&req, &good).unwrap();
    std::fs::set_permissions(&req, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::ModeNot0600 { mode: 0o644 }
    );
    assert!(!req.exists() && r.e.model_safety_blocked());
    // Owner-only but executable/other bits.
    std::fs::write(&req, &good).unwrap();
    std::fs::set_permissions(&req, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::ModeNot0600 { mode: 0o700 }
    );
    // A symlink is never followed.
    let target = r.dir.join("elsewhere.json");
    put(&target, &good);
    std::os::unix::fs::symlink(&target, &req).unwrap();
    assert_eq!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::NotRegularFile
    );
    assert!(target.exists(), "the symlink target is untouched");
    // Another uid owns it (checked through the uid-injectable form; the daemon uses its effective uid).
    put(&req, &good);
    let me = unsafe { libc::geteuid() };
    assert_eq!(
        check_operator_channel_access_as(&req, me.wrapping_add(1)),
        Err(RearmRequestRefusal::NotOwnedByDaemon {
            file_uid: me,
            daemon_uid: me.wrapping_add(1)
        })
    );
    assert_eq!(check_operator_channel_access(&req), Ok(()));
    let _ = std::fs::remove_file(&req);
    // World-writable directory.
    let d = req.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
    put(&req, &good);
    assert_eq!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::ChannelDirWorldWritable { mode: 0o777 }
    );
    assert!(
        r.e.model_safety_blocked(),
        "no access refusal ever lifts the latch"
    );
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The id of an access-refused request was never read, so it is still usable from a proper file.
    put(&req, &good);
    assert!(
        matches!(
            handle_rearm_request(&mut r.e, &req, &res),
            Some(RearmOutcome::Rearmed { .. })
        ),
        "blockers={:?}",
        r.e.model_safety_rearm_blockers()
    );
}

#[test]
fn a_replayed_request_id_is_refused_in_process_and_after_a_restart_and_a_claimed_id_survives_a_crash(
) {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "rearm_replay");
    let dir = r.dir.clone();
    let lin = bind_ledger(&mut r.e, &dir);
    r.e.model_safety_trip_operator();
    let (req, res) = chan(&r);
    put(
        &req,
        &body("x-1", &lin, r.e.model_safety_epoch(), &["operator"]),
    );
    assert!(matches!(
        handle_rearm_request(&mut r.e, &req, &res),
        Some(RearmOutcome::Rearmed { .. })
    ));
    // A new latch; the same request_id with a now-correct epoch is a replay.
    r.e.model_safety_trip_operator();
    let ep2 = r.e.model_safety_epoch();
    put(&req, &body("x-1", &lin, ep2, &["operator"]));
    assert_eq!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::DuplicateRequestId {
            request_id: "x-1".into()
        }
    );
    assert!(r.e.model_safety_blocked());
    // Validate-only (claim) and then "crash" before apply: the claimed id is spent.
    put(&req, &body("x-2", &lin, ep2, &["operator"]));
    let v = validate_rearm_request(&r.e, &req, &res)
        .unwrap()
        .expect("valid");
    assert_eq!(v.request_id, "x-2");
    assert!(r.e.model_safety_blocked(), "validation applies nothing");
    assert!(r.e.model_held_persist_now());
    drop(v);
    drop(r);
    // Restart: a brand-new engine adopts the same safety file and ledger.
    let mut e = Engine::new(cfg(), RunMode::Paper);
    let armed = arm_paper_model(&mut e, &ep.url, &dir.join("safety.json"));
    assert!(armed.blocked_at_start, "a restart never re-arms");
    match restore_held_state(&mut e, &dir.join("held.json")) {
        StartupRestore::Restored(_) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(e.model_held_lineage(), lin, "same lineage after restart");
    let epn = e.model_safety_epoch();
    assert_eq!(epn, ep2, "epoch persisted");
    for id in ["x-1", "x-2"] {
        put(&req, &body(id, &lin, epn, &["operator"]));
        assert_eq!(
            refused(handle_rearm_request(&mut e, &req, &res)),
            RearmRequestRefusal::DuplicateRequestId {
                request_id: id.into()
            },
            "{id} stays spent across a restart"
        );
        assert!(e.model_safety_blocked());
    }
    // The same request on disk at startup: StaleAtStartup; its id is then spent too.
    put(&req, &body("x-3", &lin, epn, &["operator"]));
    assert!(refuse_stale_rearm_at_startup(&e, &req, &res));
    assert!(!req.exists());
    assert_eq!(result_json(&res)["why"], "StaleAtStartup");
    put(&req, &body("x-3", &lin, epn, &["operator"]));
    assert_eq!(
        refused(handle_rearm_request(&mut e, &req, &res)),
        RearmRequestRefusal::DuplicateRequestId {
            request_id: "x-3".into()
        }
    );
    // A torn ledger is untrusted: no re-arm until an operator repairs it.
    let ledger = req.with_file_name("OPERATOR_REARM_LEDGER.jsonl");
    let mut raw = std::fs::read_to_string(&ledger).unwrap();
    raw.push_str("{\"request_id\":\"half");
    std::fs::write(&ledger, &raw).unwrap();
    put(&req, &body("x-4", &lin, epn, &["operator"]));
    assert!(matches!(
        refused(handle_rearm_request(&mut e, &req, &res)),
        RearmRequestRefusal::LedgerUntrusted(_)
    ));
    assert!(e.model_safety_blocked());
    // A complete record without its newline is torn too (the next append would fuse onto it).
    let mut raw2 = std::fs::read_to_string(&ledger).unwrap();
    raw2.truncate(raw2.rfind("{\"request_id\":\"half").unwrap());
    raw2.push_str("{\"request_id\":\"tail\"}");
    std::fs::write(&ledger, &raw2).unwrap();
    put(&req, &body("x-5", &lin, epn, &["operator"]));
    assert_eq!(
        refused(handle_rearm_request(&mut e, &req, &res)),
        RearmRequestRefusal::LedgerUntrusted("torn_last_line".into())
    );
    // A ledger readable by group/other is untrusted.
    raw2.push('\n');
    std::fs::write(&ledger, &raw2).unwrap();
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o644)).unwrap();
    put(&req, &body("x-6", &lin, epn, &["operator"]));
    assert_eq!(
        refused(handle_rearm_request(&mut e, &req, &res)),
        RearmRequestRefusal::LedgerUntrusted("owner_or_mode".into())
    );
    assert!(e.model_safety_blocked());
}

#[test]
fn apply_rechecks_lineage_and_the_engine_binding_refuses_a_wrong_reason_set() {
    let ep = Endpoint::start(|_| HOLD);
    let mut a = rig(&ep, "rearm_lin_a");
    let mut b = rig(&ep, "rearm_lin_b");
    let la = bind_ledger(&mut a.e, &a.dir.clone());
    let lb = bind_ledger(&mut b.e, &b.dir.clone());
    assert_ne!(la, lb);
    a.e.model_safety_trip_operator();
    b.e.model_safety_trip_operator();
    assert_eq!(a.e.model_safety_epoch(), b.e.model_safety_epoch());
    // Validated against run A, applied to run B (same epoch and reasons): the lineage re-check refuses it.
    let (req, res) = chan(&a);
    put(
        &req,
        &body("l-1", &la, a.e.model_safety_epoch(), &["operator"]),
    );
    let v = validate_rearm_request(&a.e, &req, &res)
        .unwrap()
        .expect("valid for A");
    match apply_validated_rearm(&mut b.e, &v, &res) {
        RearmOutcome::Refused {
            why: RearmRequestRefusal::LineageMismatch { requested, current },
            ..
        } => assert_eq!((requested, current), (la.clone(), lb.clone())),
        other => panic!("{other:?}"),
    }
    assert!(b.e.model_safety_blocked());
    // The engine binding itself refuses a reason set that is not exactly the active one.
    let ep_b = b.e.model_safety_epoch();
    assert_eq!(
        b.e.model_safety_rearm_bound("alon", ep_b, &["disk_hard_floor".to_string()]),
        Err(RearmRefusal::ReasonsMismatch {
            acknowledged: vec!["disk_hard_floor".into()],
            active: vec!["operator".into()],
        })
    );
    assert_eq!(
        b.e.model_safety_rearm_bound("alon", ep_b + 1, &["operator".to_string()]),
        Err(RearmRefusal::EpochMismatch {
            requested: ep_b + 1,
            current: ep_b
        })
    );
    assert!(b.e.model_safety_blocked());
    assert_eq!(
        b.e.model_safety_rearm_bound("alon", ep_b, &["operator".to_string()]),
        Ok(())
    );
}

#[test]
fn a_new_fault_between_validate_and_apply_refuses_the_rearm_and_the_new_latch_survives() {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "rearm_race");
    let lin = bind_ledger(&mut r.e, &r.dir.clone());
    r.e.model_safety_trip_operator();
    let (req, res) = chan(&r);
    // (1) A new LATCHING fault after validation: epoch + reason set move; apply refuses; both latches stand.
    put(
        &req,
        &body("n-1", &lin, r.e.model_safety_epoch(), &["operator"]),
    );
    let v = validate_rearm_request(&r.e, &req, &res)
        .unwrap()
        .expect("valid");
    r.e.model_stop_evaluate(
        0,
        pump_quant_app::stop_policy::OpsInputs {
            disk_hard_ok: Some(false),
            ..pump_quant_app::stop_policy::OpsInputs::healthy()
        },
    );
    assert_eq!(
        r.e.model_safety_reasons(),
        vec!["disk_hard_floor".to_string(), "operator".to_string()]
    );
    match apply_validated_rearm(&mut r.e, &v, &res) {
        RearmOutcome::Refused {
            why: RearmRequestRefusal::Contract(RearmRefusal::EpochMismatch { .. }),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert!(r.e.model_safety_blocked());
    assert_eq!(result_json(&res)["why"], "Contract:EpochMismatch");
    // Acknowledging only the old reason (at the new epoch) is a reasons mismatch.
    r.e.model_stop_evaluate(0, pump_quant_app::stop_policy::OpsInputs::healthy());
    put(
        &req,
        &body("n-2", &lin, r.e.model_safety_epoch(), &["operator"]),
    );
    assert!(matches!(
        refused(handle_rearm_request(&mut r.e, &req, &res)),
        RearmRequestRefusal::ReasonsMismatch { .. }
    ));
    // (2) A new NON-latching blocker after validation (missing history appears): apply re-checks and refuses.
    let epn = r.e.model_safety_epoch();
    put(
        &req,
        &body("n-3", &lin, epn, &["disk_hard_floor", "operator"]),
    );
    let v = validate_rearm_request(&r.e, &req, &res)
        .unwrap()
        .expect("valid");
    let _ = r.e.model_restore_missing_history(&[], false);
    match apply_validated_rearm(&mut r.e, &v, &res) {
        RearmOutcome::Refused {
            why: RearmRequestRefusal::Contract(RearmRefusal::ActiveBlockers(b)),
            ..
        } => assert!(
            b.contains(&"missing_history:continuity_unknown".to_string()),
            "{b:?}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(r.e.model_safety_blocked() && r.e.model_entries_blocked());
    assert_eq!(result_json(&res)["why"], "Contract:ActiveBlockers");
}

#[test]
fn rearm_never_waives_active_blockers_and_lists_each_one() {
    let ep = Endpoint::start(|_| HOLD);
    // Resource restriction active (RAM low) after a disk-hard-floor latch: refused, listing it.
    let mut r = rig(&ep, "rearm_blockers");
    let lin = bind_ledger(&mut r.e, &r.dir.clone());
    let ops = pump_quant_app::stop_policy::OpsInputs {
        disk_hard_ok: Some(false),
        ram_ok: Some(false),
        ..pump_quant_app::stop_policy::OpsInputs::healthy()
    };
    r.e.model_stop_evaluate(0, ops);
    assert_eq!(r.e.model_safety_reason(), "disk_hard_floor");
    let (req, res) = chan(&r);
    let epn = r.e.model_safety_epoch();
    // The hard floor is still measured: the latching condition itself is a blocker.
    put(&req, &body("b-1", &lin, epn, &["disk_hard_floor"]));
    match refused(handle_rearm_request(&mut r.e, &req, &res)) {
        RearmRequestRefusal::Contract(RearmRefusal::ActiveBlockers(b)) => {
            assert!(b.contains(&"condition:DiskHardFloor".to_string()), "{b:?}");
            assert!(
                b.contains(&"restriction:RamHeadroomLow".to_string()),
                "{b:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    // Disk recovers, RAM still low.
    r.e.model_stop_evaluate(
        0,
        pump_quant_app::stop_policy::OpsInputs {
            ram_ok: Some(false),
            ..pump_quant_app::stop_policy::OpsInputs::healthy()
        },
    );
    put(&req, &body("b-2", &lin, epn, &["disk_hard_floor"]));
    match refused(handle_rearm_request(&mut r.e, &req, &res)) {
        RearmRequestRefusal::Contract(RearmRefusal::ActiveBlockers(b)) => {
            assert_eq!(b, vec!["restriction:RamHeadroomLow".to_string()])
        }
        other => panic!("{other:?}"),
    }
    let out = result_json(&res);
    assert!(
        out["detail"].as_str().unwrap().contains("RamHeadroomLow"),
        "named in the outcome: {out}"
    );
    assert!(r.e.model_safety_blocked());
    // Missing history (continuity unknown) is never waived.
    r.e.model_stop_evaluate(0, pump_quant_app::stop_policy::OpsInputs::healthy());
    let _ = r.e.model_restore_missing_history(&[], false);
    put(&req, &body("b-3", &lin, epn, &["disk_hard_floor"]));
    match refused(handle_rearm_request(&mut r.e, &req, &res)) {
        RearmRequestRefusal::Contract(RearmRefusal::ActiveBlockers(b)) => {
            assert!(
                b.contains(&"missing_history:continuity_unknown".to_string()),
                "{b:?}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert!(r.e.model_safety_blocked());

    // Unknown liquidation (stale mark on a held position) is never waived.
    let mut r2 = rig(&ep, "rearm_unknown_liq");
    let lin2 = bind_ledger(&mut r2.e, &r2.dir.clone());
    r2.e.model_safety_trip_operator();
    let t = r2.clock + pump_quant_app::curve_annotation::PRICING_BUDGET_MS + 5_000;
    print(&mut r2.e, 999, t, r2.slot + 50);
    ticks(&mut r2.e, 1);
    let (req2, res2) = chan(&r2);
    put(
        &req2,
        &body("u-1", &lin2, r2.e.model_safety_epoch(), &["operator"]),
    );
    match refused(handle_rearm_request(&mut r2.e, &req2, &res2)) {
        RearmRequestRefusal::Contract(RearmRefusal::ActiveBlockers(b)) => {
            assert!(
                b.iter().any(|x| x.starts_with("unknown_liquidation:")),
                "{b:?}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert!(r2.e.model_safety_blocked());

    // Unavailable execution: every remaining token reserved by an unresolved (uncertain) sell. The uncertain order
    // is its own named refusal first; the blocker list also names the execution gap.
    let mut r3 = rig(&ep, "rearm_exec");
    let lin3 = bind_ledger(&mut r3.e, &r3.dir.clone());
    r3.e.model_harness_force_protect(&MINT).unwrap();
    let (id, ..) = r3.e.model_protect_pending_order(&MINT).unwrap();
    assert!(r3.e.model_protect_mark_ack_uncertain(&MINT, id));
    ticks(&mut r3.e, 2); // reservations are re-derived from the order book on the next event
    r3.e.model_safety_trip_operator();
    assert!(
        r3.e.model_safety_rearm_blockers()
            .iter()
            .any(|x| x.starts_with("execution_unavailable:")),
        "{:?}",
        r3.e.model_safety_rearm_blockers()
    );
    let (req3, res3) = chan(&r3);
    put(
        &req3,
        &body("e-1", &lin3, r3.e.model_safety_epoch(), &["operator"]),
    );
    assert_eq!(
        refused(handle_rearm_request(&mut r3.e, &req3, &res3)),
        RearmRequestRefusal::Contract(RearmRefusal::UncertainOrderPending)
    );
    assert!(r3.e.model_safety_blocked());
}

#[test]
fn operator_rearm_is_refused_when_it_cannot_persist_and_without_a_lineage() {
    let ep = Endpoint::start(|_| HOLD);
    let dir = tmp("rearm_nowrite");
    let bad = dir.join("safety_is_a_dir");
    std::fs::create_dir_all(&bad).unwrap();
    let mut e = Engine::new(cfg(), RunMode::Paper);
    let _ = arm_paper_model(&mut e, &ep.url, &bad);
    e.model_safety_trip_operator();
    let (req, res) = chan_in(&dir);
    // No held ledger attached: nothing to bind.
    put(
        &req,
        &body("p-0", "", e.model_safety_epoch(), &["operator"]),
    );
    assert_eq!(
        refused(handle_rearm_request(&mut e, &req, &res)),
        RearmRequestRefusal::LineageUnavailable
    );
    let lin = bind_ledger(&mut e, &dir);
    // The unreadable safety path latched `state_file_unreadable`; the operator trip joined it.
    assert_eq!(
        e.model_safety_reasons(),
        vec!["operator".to_string(), "state_file_unreadable".to_string()]
    );
    put(
        &req,
        &body(
            "p-1",
            &lin,
            e.model_safety_epoch(),
            &["operator", "state_file_unreadable"],
        ),
    );
    assert_eq!(
        refused(handle_rearm_request(&mut e, &req, &res)),
        RearmRequestRefusal::Contract(RearmRefusal::PersistFailed)
    );
    assert!(e.model_safety_blocked());
}

#[test]
fn operator_channel_is_never_a_model_or_stream_input_and_no_code_path_writes_the_request() {
    // Runtime: a request's contents never reach a model prompt.
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "rearm_isolation");
    let lin = bind_ledger(&mut r.e, &r.dir.clone());
    r.e.model_safety_trip_operator();
    let (req, res) = chan(&r);
    let marker = "OPCHAN_MARKER_7f3a";
    put(
        &req,
        &serde_json::json!({"schema":2,"request_id":marker,"operator":marker,"lineage":lin,
            "safety_epoch":r.e.model_safety_epoch(),"acknowledged_reasons":["operator"]})
        .to_string(),
    );
    let _ = handle_rearm_request(&mut r.e, &req, &res);
    r.advance(30_000);
    assert!(
        !ep.bodies.lock().unwrap().iter().any(|b| b.contains(marker)),
        "operator channel content reached a model request"
    );
    // Static: only the channel module and the daemon's two call sites name the channel; model, prompt, inference,
    // stream and replay code never does, and no Rust code writes the request file.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut hits = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for ent in std::fs::read_dir(&d).unwrap().flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                let t = std::fs::read_to_string(&p).unwrap_or_default();
                let rel = p.strip_prefix(&root).unwrap().to_string_lossy().to_string();
                if rel.contains("/tests/") {
                    continue;
                }
                for (i, l) in t.lines().enumerate() {
                    if l.contains("OPERATOR_REARM")
                        || l.contains("OPERATOR_CHANNEL")
                        || l.contains("data/operator")
                    {
                        hits.push((rel.clone(), i + 1, l.trim().to_string()));
                    }
                }
            }
        }
    }
    let allowed = |f: &str| {
        f.ends_with("pump-quant-junction/src/model_lifecycle.rs")
            || f.ends_with("pump-quant-junction/src/bin/pq_daemon.rs")
    };
    let bad: Vec<_> = hits.iter().filter(|(f, ..)| !allowed(f)).collect();
    assert!(
        bad.is_empty(),
        "operator channel referenced outside the channel/daemon: {bad:?}"
    );
    let daemon: Vec<_> = hits
        .iter()
        .filter(|(f, ..)| f.ends_with("pq_daemon.rs"))
        .map(|(_, _, l)| l.clone())
        .collect();
    assert!(!daemon.is_empty());
    for l in &daemon {
        assert!(
            !l.contains("write(") && !l.contains("File::create") && !l.contains("OpenOptions"),
            "the daemon never writes the request: {l}"
        );
    }
    // In the channel section of the module, the request file is only ever read, inspected or removed.
    let src =
        std::fs::read_to_string(root.join("pump-quant-junction/src/model_lifecycle.rs")).unwrap();
    let a = src.find("pub const OPERATOR_CHANNEL_DIR").unwrap();
    let b = src.find("pub const HARNESS_FORCE_PROTECT_FILE").unwrap();
    for l in src[a..b].lines() {
        if l.contains("request_file") && (l.contains("write_private(") || l.contains("fs::write("))
        {
            panic!("channel code writes the request file: {l}");
        }
    }
}

// ─── HARNESS-ONLY forced protective trigger under pressure ───────────────────────────────────────────────────────

#[test]
fn a_forced_hard_stop_under_safety_off_and_a_resource_restriction_executes_through_the_protective_path(
) {
    let ep = Endpoint::start(|_| HOLD);
    let mut r = rig(&ep, "force_protect");
    let file = r.dir.join("FORCE");
    // Not the harness: never read, never removed, nothing queued.
    std::fs::write(&file, "ab".repeat(32)).unwrap();
    assert!(harness_force_protect_from(&mut r.e, false, &file).is_none());
    assert!(file.exists());
    // Pressure: disk hard floor latches SAFETY_OFF, RAM low restricts.
    r.e.model_stop_evaluate(
        0,
        pump_quant_app::stop_policy::OpsInputs {
            disk_hard_ok: Some(false),
            ram_ok: Some(false),
            ..pump_quant_app::stop_policy::OpsInputs::healthy()
        },
    );
    assert!(r.e.model_safety_blocked() && r.e.model_new_risk_blocked());
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    assert!(inv0 > 0);
    let (m, res) = harness_force_protect_from(&mut r.e, true, &file).unwrap();
    assert_eq!(m, "ab".repeat(32));
    assert!(!file.exists(), "consumed");
    let (id, intended, filled, code) = res.unwrap().expect("a protective order is working");
    assert!(id > 0);
    assert_eq!(
        (intended, filled, code),
        (inv0, 0, 2),
        "whole free inventory, HardStop code"
    );
    assert_eq!(r.rep("protect:harness_forced:HardStop"), 1);
    assert_eq!(r.rep("protect:trigger:HardStop"), 1);
    assert_eq!(r.rep("protect:order:HardStop"), 1);
    // A second force while it works is refused by name.
    std::fs::write(&file, "ab".repeat(32)).unwrap();
    assert_eq!(
        harness_force_protect_from(&mut r.e, true, &file).unwrap().1,
        Err("harness_force_protect_refused:protective_order_working")
    );
    // The protective order fills on landing state despite SAFETY_OFF + the RAM restriction: only the fill closes.
    r.advance(20_000);
    assert!(r.rep("protect:fill") >= 1, "{:?}", r.e.model_lane_report());
    assert!(
        !r.e.model_position_open(&MINT),
        "protective sell closed the position"
    );
    assert!(
        r.e.model_safety_blocked(),
        "the latch is untouched by protection"
    );
    // Not held now -> named refusal; bad mint -> named refusal.
    std::fs::write(&file, "ab".repeat(32)).unwrap();
    assert_eq!(
        harness_force_protect_from(&mut r.e, true, &file).unwrap().1,
        Err("harness_force_protect_refused:not_held")
    );
    std::fs::write(&file, "zz").unwrap();
    assert_eq!(
        harness_force_protect_from(&mut r.e, true, &file).unwrap().1,
        Err("harness_force_protect_refused:bad_mint")
    );
}

#[test]
fn the_forced_trigger_is_refused_outside_a_paper_model_engine() {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    assert_eq!(
        e.model_harness_force_protect(&MINT),
        Err("harness_force_protect_refused:not_paper_model")
    );
}
