//! CORPUS-RECONSTRUCTION replay: normalized captured tape + captured curve reserves + launch rows,
//! driven through the REAL engine (`Engine::tick`) with a deterministic stub model.
//!
//! WHAT THIS IS NOT. The tape is the normalized corpus, not the daemon's wire format, so this is NOT
//! a production-decoder test; daemon-facing wire compatibility is a separate (Windows) acceptance
//! item. It establishes that, given corpus-equivalent history, the engine's join produces prompts,
//! and it MEASURES how often and why it cannot -- split by venue, token age and warm-up.
//!
//! Input: `/tmp/replay_events.jsonl` (built by the harness script; ordered by receive time).
//! Skips (passes vacuously, loudly) when absent. Writes `/tmp/replay_report.json`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const PATH: &str = "/tmp/replay_events.jsonl";
const SKIP: &str = "DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: replay stub";

struct Stub(Arc<AtomicUsize>);
impl ModelSource for Stub {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(SKIP.to_string())
    }
}

fn hex32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    out
}

fn entity(w: &[u8; 32]) -> u64 {
    let mut x = 0u64;
    for b in &w[..8] {
        x = (x << 8) | u64::from(*b);
    }
    x | 1
}

#[test]
fn captured_history_through_the_real_engine_reports_coverage() {
    let Ok(f) = std::fs::File::open(PATH) else {
        eprintln!("SKIP: {PATH} not present");
        return;
    };
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));

    let mut n_events = 0u64;
    let mut n_market_trades = 0u64;
    let mut last_tick_ms = 0i64;
    let mut ticks = 0u64;
    let mut by_kind: BTreeMap<&'static str, u64> = BTreeMap::new();
    for line in BufReader::new(f).lines() {
        let line = line.expect("read");
        let v: serde_json::Value = serde_json::from_str(&line).expect("json");
        let kind = v["k"].as_str().expect("k");
        let mint = DomainMint::from_bytes(hex32(v["m"].as_str().expect("m")));
        let t = v["t"].as_i64().expect("t");
        n_events += 1;
        match kind {
            "L" => {
                *by_kind.entry("launch").or_default() += 1;
                e.tick(AppEvent::LaunchObserved {
                    mint,
                    creator: hex32(v["c"].as_str().expect("c")),
                    launch_unix_ms: t,
                });
            }
            "T" => {
                let w = hex32(v["w"].as_str().expect("w"));
                let sol = v["sol"].as_u64().expect("sol");
                let base = v["base"].as_i64().expect("base");
                let venue = if v["venue"] == "pumpswap" {
                    TradeVenue::PumpSwap
                } else {
                    TradeVenue::PumpFun
                };
                let rv = v["rv"].as_array();
                let (price_fp, liq) = match rv {
                    Some(r) => {
                        let vsol = r[0].as_u64().expect("vsol");
                        let vtok = r[1].as_u64().expect("vtok");
                        (
                            if vtok > 0 {
                                (u128::from(vsol) * 1_000_000_000 / u128::from(vtok)) as i128
                            } else {
                                0
                            },
                            vsol,
                        )
                    }
                    None => (0, 0),
                };
                *by_kind
                    .entry(if rv.is_some() {
                        "trade_priced"
                    } else {
                        "trade_unpriced"
                    })
                    .or_default() += 1;
                if let Some(r) = rv {
                    // The captured post-trade reserves ARE the curve observation at this print's clock.
                    e.tick(AppEvent::CurveObserved {
                        mint,
                        v_sol_lamports: r[0].as_u64().expect("a"),
                        v_tokens: r[1].as_u64().expect("b"),
                        real_sol_lamports: r[2].as_u64().expect("c"),
                        real_tokens: r[3].as_u64().expect("d"),
                        recv_unix_ms: Some(t),
                        slot: v["slot"].as_u64().expect("slot"),
                    });
                    // NO CurveModeObserved: the normalized tape carries no curve-mode byte, so the mode is UNKNOWN
                    // and (operator decision 2026-10-09) these markets are refused as `curve_mode_unknown`. Injecting
                    // `mayhem: false` here would be a guess.
                }
                n_market_trades += 1;
                e.tick(AppEvent::MarketTrade {
                    mint,
                    price_fp,
                    quote_lamports: sol,
                    liquidity_lamports: liq,
                    signed_base: base,
                    buyer_entity: entity(&w),
                    age_slots: 30,
                    recv_unix_ms: Some(t),
                    trader_pubkey: Some(w),
                    slot: v["slot"].as_u64(),
                    fee_lamports: v["fee"].as_u64(),
                    cu_consumed: v["cu"].as_u64(),
                    venue: Some(venue),
                    event_id: None,
                    feature: None,
                });
            }
            _ => panic!("kind"),
        }
        // One evaluation tick per second of FEED time (never per event, never wall-clock).
        if t - last_tick_ms >= 1_000 {
            last_tick_ms = t;
            ticks += 1;
            e.tick(AppEvent::Tick);
        }
    }
    // Let in-flight stub answers land.
    for _ in 0..20 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let ingest = {
        let c = e.model_ingest_counters();
        serde_json::json!({"accepted": c.accepted, "no_clock": c.no_clock, "no_price": c.no_price, "out_of_order": c.out_of_order, "duplicate": c.duplicate})
    };
    let report = serde_json::json!({
        "events": n_events,
        "market_trades_fed": n_market_trades,
        "ticks": ticks,
        "kinds": by_kind,
        "model_calls": calls.load(Ordering::SeqCst),
        "lane": e.model_lane_report(),
        "ingest": ingest,
        "funnel": e.model_funnel(),
        "admission_comparison": e.model_admission_comparison(),
    });
    std::fs::write(
        "/tmp/replay_report.json",
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "replayed {n_events} events, {ticks} ticks, {} model calls",
        calls.load(Ordering::SeqCst)
    );
}
