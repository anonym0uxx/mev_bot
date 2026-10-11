//! Measure TIME TO A COMPLETE, VALIDATED VERDICT against a real llama-server endpoint (Windows step).
//!
//! The engine deadline (3 s) and socket timeout (8 s) are PROVISIONAL until this is run against the real
//! Qwen serving path. This tool sends the trained management and entry prompts (from the parity fixtures)
//! through the PRODUCTION `InferenceClient` (non-streaming, full completion - never early-headline), parses
//! each completion with the same contract parser the engine uses, and reports percentiles of wall time to a
//! VALID verdict, plus counts of transport errors, truncations and malformed completions.
//!
//!   pq-serving-latency --endpoint http://127.0.0.1:8080 --fixtures <dir with management.jsonl decision.jsonl> \
//!                      [--n 100] [--concurrency 1] [--out report.json]
//!
//! It makes no trading decision and writes only the report.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pump_quant_inference::seam::parse_decision_payload;
use pump_quant_inference::InferenceClient;

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1))
        .cloned()
}

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((s.len() as f64 * p).ceil() as usize).clamp(1, s.len()) - 1;
    s[idx]
}

fn load(path: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["expected"].as_str().map(str::to_string))
        .collect()
}

fn main() -> ExitCode {
    let Some(endpoint) = arg("--endpoint") else {
        eprintln!("usage: pq-serving-latency --endpoint URL --fixtures DIR [--n N] [--concurrency C] [--out FILE]");
        return ExitCode::from(2);
    };
    let fixtures = std::path::PathBuf::from(arg("--fixtures").unwrap_or_else(|| ".".into()));
    let n: usize = arg("--n").and_then(|v| v.parse().ok()).unwrap_or(100);
    let conc: usize = arg("--concurrency")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    // Generous measurement timeout: we want the TRUE latency, not a censored one.
    let client = Arc::new(InferenceClient::new(&endpoint, Duration::from_secs(120)));
    let mgmt = load(&fixtures.join("management.jsonl"));
    let dec = load(&fixtures.join("decision.jsonl"));
    if mgmt.is_empty() && dec.is_empty() {
        eprintln!("no prompts found under {}", fixtures.display());
        return ExitCode::from(2);
    }
    let sys_m = pump_quant_proposal::system_prompt(pump_quant_proposal::PromptFamily::Management);
    let sys_d = pump_quant_proposal::system_prompt(pump_quant_proposal::PromptFamily::Decision);
    let jobs: Vec<(&'static str, &'static str, String)> = (0..n)
        .map(|i| {
            if !mgmt.is_empty() && (dec.is_empty() || i % 2 == 0) {
                ("management", sys_m, mgmt[(i / 2) % mgmt.len()].clone())
            } else {
                ("entry", sys_d, dec[(i / 2) % dec.len()].clone())
            }
        })
        .collect();
    let jobs = Arc::new(Mutex::new(jobs.into_iter().enumerate().collect::<Vec<_>>()));
    #[allow(clippy::type_complexity)]
    // (job name, latency, status) tuple rows; a type alias would not add clarity
    let results: Arc<Mutex<Vec<(&'static str, f64, &'static str)>>> =
        Arc::new(Mutex::new(Vec::new()));
    // Warm-up is excluded from the statistics and reported separately (first-request cost is real but different).
    let t_warm = Instant::now();
    let warm = client.warmup().is_ok();
    let warm_s = t_warm.elapsed().as_secs_f64();
    let mut hs = Vec::new();
    for _ in 0..conc {
        let (client, jobs, results) =
            (Arc::clone(&client), Arc::clone(&jobs), Arc::clone(&results));
        hs.push(std::thread::spawn(move || loop {
            let Some((_, (kind, sys, user))) = jobs.lock().unwrap().pop() else {
                return;
            };
            let t = Instant::now();
            let r = client.complete_with_meta(sys, &user);
            let dt = t.elapsed().as_secs_f64();
            let outcome = match r {
                Err(_) => "transport_error",
                Ok(c) if c.truncated() => "truncated",
                // Termination contract (same check the engine applies): no/unaccepted finish_reason,
                // a template marker in the content, or a repeated decision field.
                Ok(c) if c.termination().is_err() => "unterminated",
                Ok(c) => match parse_decision_payload(&c.text) {
                    Ok(_) => "valid",
                    Err(_) => "malformed",
                },
            };
            results.lock().unwrap().push((kind, dt, outcome));
        }));
    }
    for h in hs {
        let _ = h.join();
    }
    let res = results.lock().unwrap().clone();
    let mut report = serde_json::json!({
        "endpoint": endpoint, "n": res.len(), "concurrency": conc, "warmup_ok": warm, "warmup_s": warm_s,
        "provisional_bounds_s": {"engine_deadline": 3.0, "socket_timeout": 8.0},
        "note": "time = full non-streaming completion, valid only; invalid/truncated/errored requests are counted, never timed as success",
    });
    for kind in ["management", "entry", "all"] {
        let sel: Vec<_> = res
            .iter()
            .filter(|r| kind == "all" || r.0 == kind)
            .collect();
        let valid: Vec<f64> = sel.iter().filter(|r| r.2 == "valid").map(|r| r.1).collect();
        let count = |o: &str| sel.iter().filter(|r| r.2 == o).count();
        let over = |b: f64| valid.iter().filter(|t| **t > b).count();
        report[kind] = serde_json::json!({
            "requests": sel.len(), "valid": valid.len(), "malformed": count("malformed"),
            "truncated": count("truncated"), "unterminated": count("unterminated"), "transport_error": count("transport_error"),
            "valid_latency_s": {"p50": pct(&valid,0.5), "p90": pct(&valid,0.9), "p99": pct(&valid,0.99), "max": pct(&valid,1.0)},
            "valid_over_3s": over(3.0), "valid_over_8s": over(8.0),
        });
    }
    let text = serde_json::to_string_pretty(&report).unwrap();
    println!("{text}");
    if let Some(out) = arg("--out") {
        let _ = std::fs::write(out, &text);
    }
    ExitCode::SUCCESS
}
