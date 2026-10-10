//! READINESS MEASUREMENT v2 (paper; no order leaves the process). Real daemon code path:
//! `arm_paper_model` (what `pq_daemon` runs) -> engine scheduler -> worker pool (MODEL_WORKERS) -> production
//! `InferenceClient` (8 s socket) -> timing-only pass-through proxy -> OpenAI-compatible server -> termination
//! contract -> parser -> verdict application. Engine deadline 3 s and socket 8 s UNCHANGED.
//!
//! v2 adds over v1: (a) `--mgmt-held FILE`: restore a held-exposure ledger (the daemon's own restore path,
//! `restore_held_state`) so MANAGEMENT asks run without depending on a Qwen BUY; (b) a per-request log
//! (`--requests FILE`, one JSON line per HTTP exchange: kind, mint, step, request start, upstream seconds,
//! finish_reason, prompt/completion tokens, whether the client was still waiting when the response came back);
//! (c) the proxy CLOSES the upstream connection the moment the client hangs up, so whether the server stops
//! generating on disconnect can be read from the server's own /metrics (`num_requests_running`, abort counter).
//!
//!   cargo run --example serve_daemon_latency_v2 -- --upstream 127.0.0.1:8002 --mints 4 --secs 60 \
//!       [--mgmt-held held.json] --requests req.jsonl --out report.json

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_junction::model_lifecycle::{
    arm_paper_model, restore_held_state, StartupRestore, CLIENT_TIMEOUT,
};

const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1))
        .cloned()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[derive(Debug, Clone)]
struct Exchange {
    t_req_ms: i64,
    upstream_s: f64,
    status: String,
    finish_reason: Option<String>,
    completion_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
    kind: &'static str,
    mint: Option<String>,
    step: Option<String>,
    /// The client had already hung up (socket timeout) before the upstream answered.
    client_gone: bool,
    /// The proxy aborted the upstream request because the client hung up first.
    upstream_aborted: bool,
}

fn field(body: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let user = v["messages"][1]["content"].as_str()?.to_string();
    user.lines()
        .find_map(|l| l.strip_prefix(key).map(|s| s.trim().to_string()))
}

/// Read one HTTP message (headers + Content-Length body) from `s`.
fn read_msg(s: &mut TcpStream) -> Option<(Vec<u8>, usize)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..h]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < h + 4 + len {
                let n = s.read(&mut tmp).ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            return Some((buf, h + 4));
        }
    }
}

/// Transparent pass-through: forwards the request bytes to `upstream` and the response bytes
/// back, unmodified; records timing and the server's own finish_reason / usage.
fn proxy(upstream: String, log: Arc<Mutex<Vec<Exchange>>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut c) = conn else { continue };
            let (upstream, log) = (upstream.clone(), Arc::clone(&log));
            std::thread::spawn(move || {
                let _ = c.set_read_timeout(Some(Duration::from_secs(30)));
                let Some((req, hb)) = read_msg(&mut c) else {
                    return;
                };
                let body = String::from_utf8_lossy(&req[hb..]).to_string();
                let kind =
                    if body.contains("Decide the next action for a position you already hold") {
                        "management"
                    } else {
                        "entry"
                    };
                let (mint, step) = if kind == "management" {
                    (field(&body, "MINT:"), field(&body, "STEP:"))
                } else {
                    (None, None)
                };
                let t_req_ms = now_ms();
                let t0 = Instant::now();
                let Ok(mut u) = TcpStream::connect(&upstream) else {
                    return;
                };
                let _ = u.set_read_timeout(Some(Duration::from_secs(300)));
                if u.write_all(&req).is_err() {
                    return;
                }
                let u2 = u.try_clone().ok();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(read_msg(&mut u));
                });
                // Watch the CLIENT: a 0-byte peek means it hung up (8 s socket). Then close upstream so the
                // server sees the disconnect (that is what a direct client would cause).
                let _ = c.set_nonblocking(true);
                let mut client_gone = false;
                let mut upstream_aborted = false;
                let got = loop {
                    match rx.recv_timeout(Duration::from_millis(20)) {
                        Ok(r) => break r,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
                        Err(_) => {}
                    }
                    let mut pb = [0u8; 1];
                    if !client_gone {
                        if let Ok(0) = c.peek(&mut pb) {
                            client_gone = true;
                            if let Some(u2) = &u2 {
                                let _ = u2.shutdown(std::net::Shutdown::Both);
                                upstream_aborted = true;
                            }
                        }
                    }
                };
                let _ = c.set_nonblocking(false);
                let upstream_s = t0.elapsed().as_secs_f64();
                let Some((resp, rb)) = got else {
                    log.lock().unwrap().push(Exchange {
                        t_req_ms,
                        upstream_s,
                        status: "aborted".into(),
                        finish_reason: None,
                        completion_tokens: None,
                        prompt_tokens: None,
                        kind,
                        mint,
                        step,
                        client_gone,
                        upstream_aborted,
                    });
                    return;
                };
                let status = String::from_utf8_lossy(&resp[..resp.len().min(64)])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let v: serde_json::Value = serde_json::from_slice(&resp[rb..]).unwrap_or_default();
                log.lock().unwrap().push(Exchange {
                    t_req_ms,
                    upstream_s,
                    status,
                    finish_reason: v["choices"][0]["finish_reason"]
                        .as_str()
                        .map(str::to_string),
                    completion_tokens: v["usage"]["completion_tokens"].as_u64(),
                    prompt_tokens: v["usage"]["prompt_tokens"].as_u64(),
                    kind,
                    mint,
                    step,
                    client_gone,
                    upstream_aborted,
                });
                let _ = c.write_all(&resp);
            });
        }
    });
    url
}

fn mint_of(i: usize) -> [u8; 32] {
    let mut m = [0u8; 32];
    m[0] = 0xA0;
    m[1] = i as u8 + 1;
    m[31] = 7;
    m
}

/// A launch plus 40 prior trades, ending ~1 s before `t_end` (wall clock), then live reserves.
fn history(i: usize, t_end: i64) -> Vec<AppEvent> {
    let mint = DomainMint::from_bytes(mint_of(i));
    let t0 = t_end - 90_000;
    let mut creator = [0xCDu8; 32];
    creator[1] = i as u8;
    let mut ev = vec![AppEvent::LaunchObserved {
        mint,
        creator,
        launch_unix_ms: t0,
    }];
    for k in 0..40u32 {
        let mut w = [0u8; 32];
        w[0] = (k % 200) as u8 + 1;
        w[1] = i as u8;
        w[31] = 1;
        let buy = k % 3 != 0;
        ev.push(AppEvent::MarketTrade {
            mint,
            price_fp: 22_000 + i128::from(k),
            quote_lamports: 500_000_000 + u64::from(k),
            liquidity_lamports: VSOL,
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(k) + 1000 * i as u64,
            age_slots: 30,
            recv_unix_ms: Some(t0 + 1_000 + i64::from(k) * 2_000),
            trader_pubkey: Some(w),
            slot: Some(1_000 + u64::from(k)),
            fee_lamports: Some(60_000 + u64::from(k) * 100),
            cu_consumed: Some(90_000 + u64::from(k)),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint,
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

fn history_for(m: [u8; 32], salt: usize, t_end: i64) -> Vec<AppEvent> {
    let mut ev = history(salt, t_end);
    let dm = DomainMint::from_bytes(m);
    for x in &mut ev {
        match x {
            AppEvent::LaunchObserved { mint, .. }
            | AppEvent::MarketTrade { mint, .. }
            | AppEvent::OnchainConfirm { mint, .. } => *mint = dm,
            _ => {}
        }
    }
    ev
}

fn curve_for(m: [u8; 32], ts: i64, slot: u64) -> AppEvent {
    AppEvent::CurveObserved {
        mint: DomainMint::from_bytes(m),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    }
}

fn print_for(m: [u8; 32], salt: usize, i: u32, ts: i64, slot: u64) -> AppEvent {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = salt as u8;
    w[31] = 3;
    AppEvent::MarketTrade {
        mint: DomainMint::from_bytes(m),
        price_fp: 45_300 + i128::from(i % 7),
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: VSOL,
        signed_base: 30_000_000_000,
        buyer_entity: 50_000 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(w),
        slot: Some(slot),
        fee_lamports: Some(60_000),
        cu_consumed: Some(90_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    }
}

fn curve(i: usize, ts: i64, slot: u64) -> AppEvent {
    AppEvent::CurveObserved {
        mint: DomainMint::from_bytes(mint_of(i)),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    }
}

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() as f64 * p).ceil() as usize).clamp(1, s.len()) - 1]
}

fn main() {
    let upstream = arg("--upstream").unwrap_or_else(|| "127.0.0.1:8001".into());
    let mints: usize = arg("--mints").and_then(|v| v.parse().ok()).unwrap_or(6);
    let secs: u64 = arg("--secs").and_then(|v| v.parse().ok()).unwrap_or(60);
    let log = Arc::new(Mutex::new(Vec::new()));
    let url = proxy(upstream.clone(), Arc::clone(&log));
    let dir = std::env::temp_dir().join(format!("pq_serve_lat_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    let armed = arm_paper_model(&mut e, &url, &dir.join("safety.json"));
    assert!(!armed.blocked_at_start, "{armed:?}");
    // RESTORED EXPOSURE: the daemon's own restore path. The fixture is COPIED (never edited) and its held mints
    // are then fed history + live reserves like any other mint, so management asks do not depend on a BUY.
    let mut held_mints: Vec<[u8; 32]> = Vec::new();
    let restore = match arg("--mgmt-held") {
        Some(src) => {
            let dst = dir.join("held.json");
            std::fs::copy(&src, &dst).expect("copy held fixture");
            let r = restore_held_state(&mut e, &dst);
            if let StartupRestore::Restored(_) = &r {
                for h in e.model_held_ledger().held {
                    held_mints.push(h.mint);
                }
            }
            format!("{r:?}")
        }
        None => "none".into(),
    };
    let start = now_ms();
    for i in 0..mints {
        for ev in history(i, start) {
            e.tick(ev);
        }
    }
    for (k, m) in held_mints.iter().enumerate() {
        for ev in history_for(*m, 100 + k, start) {
            e.tick(ev);
        }
    }
    let t_run = Instant::now();
    let mut slot = 2_100u64;
    while t_run.elapsed() < Duration::from_secs(secs) {
        slot += 1;
        let ts = now_ms();
        for i in 0..mints {
            e.tick(curve(i, ts, slot));
        }
        for (k, m) in held_mints.iter().enumerate() {
            e.tick(curve_for(*m, ts, slot));
            e.tick(print_for(*m, 100 + k, slot as u32, ts, slot));
        }
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(50));
    }
    // Drain: keep the clock moving until every in-flight HTTP exchange has returned (or 30 s),
    // so late answers are offered to the engine and their rejection is counted.
    let t_drain = Instant::now();
    while t_drain.elapsed() < Duration::from_secs(30) {
        slot += 1;
        let ts = now_ms();
        for i in 0..mints {
            e.tick(curve(i, ts, slot));
        }
        for m in &held_mints {
            e.tick(curve_for(*m, ts, slot));
        }
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(50));
    }
    let rep = e.model_lane_report().clone();
    let ex = log.lock().unwrap().clone();
    let up: Vec<f64> = ex.iter().map(|x| x.upstream_s).collect();
    let mut fr = std::collections::BTreeMap::<String, u64>::new();
    for x in &ex {
        *fr.entry(format!(
            "{}|{}",
            x.kind,
            x.finish_reason.clone().unwrap_or("none".into())
        ))
        .or_default() += 1;
    }
    let toks: Vec<f64> = ex
        .iter()
        .filter_map(|x| x.completion_tokens.map(|t| t as f64))
        .collect();
    let pick = |p: &str| -> serde_json::Value {
        rep.iter()
            .filter(|(k, _)| k.starts_with(p))
            .map(|(k, v)| (k.clone(), serde_json::json!(v)))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    let out = serde_json::json!({
        "upstream": upstream, "mints": mints, "run_s": secs, "drain_s": 30,
        "engine_deadline_ms": pump_quant_app::freshness::CHAMPION_MAX_DECISION_AGE_MS,
        "client_socket_timeout_s": CLIENT_TIMEOUT.as_secs(),
        "http_exchanges": ex.len(),
        "first_exchange": ex.first().map(|x| serde_json::json!({"upstream_s": x.upstream_s, "finish_reason": x.finish_reason, "completion_tokens": x.completion_tokens, "status": x.status})),
        "upstream_latency_s": {"p50": pct(&up, 0.5), "p90": pct(&up, 0.9), "p99": pct(&up, 0.99), "max": pct(&up, 1.0)},
        "completion_tokens": {"p50": pct(&toks, 0.5), "max": pct(&toks, 1.0)},
        "finish_reason_by_kind": fr,
        "within_3s": up.iter().filter(|t| **t <= 3.0).count(),
        "within_8s": up.iter().filter(|t| **t <= 8.0).count(),
        "engine": {
            "dispatched": rep.get("dispatched"), "verdicts": pick("verdict:"), "notrade": pick("notrade:"),
            "mgmt": pick("mgmt:"), "endpoint": pick("endpoint:"), "discard": pick("discard:"),
            "abandoned_deadline": rep.get("request_abandoned_deadline"),
            "dispatch_refused": pick("dispatch"),
        },
        "engine_report_full": rep,
        "restore": restore,
        "held_mints": held_mints.len(),
        "client_gone": ex.iter().filter(|x| x.client_gone).count(),
        "upstream_aborted": ex.iter().filter(|x| x.upstream_aborted).count(),
    });
    if let Some(p) = arg("--requests") {
        let mut s = String::new();
        for x in &ex {
            s.push_str(&serde_json::json!({"t_req_ms": x.t_req_ms, "kind": x.kind, "mint": x.mint, "step": x.step,
                "upstream_s": x.upstream_s, "status": x.status, "finish_reason": x.finish_reason,
                "prompt_tokens": x.prompt_tokens, "completion_tokens": x.completion_tokens,
                "client_gone": x.client_gone, "upstream_aborted": x.upstream_aborted}).to_string());
            s.push('\n');
        }
        std::fs::write(p, s).unwrap();
    }
    let text = serde_json::to_string_pretty(&out).unwrap();
    println!("{text}");
    if let Some(p) = arg("--out") {
        std::fs::write(p, &text).unwrap();
    }
}
