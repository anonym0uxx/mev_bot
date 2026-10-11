//! READINESS MEASUREMENT (paper, no orders leave the process): latency and outcome of model asks
//! through the REAL daemon path — `arm_paper_model` (the code `pq_daemon` runs) -> engine scheduler
//! -> worker pool -> production `InferenceClient` -> a real OpenAI-compatible server -> termination
//! contract -> parser -> verdict application, with the engine's own 3 s deadline and 8 s socket
//! timeout UNCHANGED.
//!
//! A transparent pass-through proxy sits between the client and the server ONLY to timestamp each
//! HTTP exchange and read the server's `finish_reason`/`usage`; it forwards bytes unmodified.
//! Synthetic market events (wall-clock stamped so the engine deadline runs in real time).
//!
//!   cargo run --example serve_daemon_latency -- --upstream 127.0.0.1:8001 --mints 6 --secs 60 \
//!       --out report.json

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_junction::model_lifecycle::{arm_paper_model, CLIENT_TIMEOUT};

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
    _t_req_ms: i64,
    upstream_s: f64,
    status: String,
    finish_reason: Option<String>,
    completion_tokens: Option<u64>,
    kind: &'static str,
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
                let t_req_ms = now_ms();
                let t0 = Instant::now();
                let Ok(mut u) = TcpStream::connect(&upstream) else {
                    return;
                };
                let _ = u.set_read_timeout(Some(Duration::from_secs(300)));
                if u.write_all(&req).is_err() {
                    return;
                }
                let Some((resp, rb)) = read_msg(&mut u) else {
                    return;
                };
                let upstream_s = t0.elapsed().as_secs_f64();
                let status = String::from_utf8_lossy(&resp[..resp.len().min(64)])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let v: serde_json::Value = serde_json::from_slice(&resp[rb..]).unwrap_or_default();
                log.lock().unwrap().push(Exchange {
                    _t_req_ms: t_req_ms,
                    upstream_s,
                    status,
                    finish_reason: v["choices"][0]["finish_reason"]
                        .as_str()
                        .map(str::to_string),
                    completion_tokens: v["usage"]["completion_tokens"].as_u64(),
                    kind,
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
    let start = now_ms();
    for i in 0..mints {
        for ev in history(i, start) {
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
    });
    let text = serde_json::to_string_pretty(&out).unwrap();
    println!("{text}");
    if let Some(p) = arg("--out") {
        std::fs::write(p, &text).unwrap();
    }
}
