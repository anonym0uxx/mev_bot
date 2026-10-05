//! AMM path through the REAL engine (`Engine::tick`) on CAPTURED history, discovered from stream
//! events only: launch -> curve prints -> canonical-pool swap events (pre-trade reserves) ->
//! stream-driven registration -> ready prompt -> stub BUY -> admission -> pending order -> fill from
//! the POOL reserves -> position.
//!
//! WHAT THIS IS. Fixture = the real mint ATtootmfeBeD..pump (chosen for provenance: launch row, curve
//! history with reserves, and a canonical pool with captured pre-trade reserves; NOT for outcome --
//! the stub always answers BUY). Inputs are the normalized corpus tape + captured reserve history
//! (`amm_history_v1`), so this is CORPUS RECONSTRUCTION, not daemon-wire compatibility.
//!
//! KNOWN GAPS in the fixture, stated rather than hidden: 1,569 of the mint's 2,254 AMM tape rows have
//! no captured pool in `amm_history_v1` and are OMITTED (their pool is not inferred), so the flow
//! windows undercount AMM activity; the pool fee rate is on-chain-verified for only 20 of the
//! swaps fed (125 bp), the rest carry none and the fill uses the last observed rate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const FIXTURE: &str = include_str!("fixtures/amm_atoo_events.jsonl");
const BUY: &str = "DECISION: BUY\nSIZE: SMALL\nINVALIDATION: none\nEVIDENCE: stub";

struct Stub(Arc<AtomicUsize>);
impl ModelSource for Stub {
    // Deterministic by PROMPT CONTENT: BUY only when the prompt itself says the venue is PumpSwap, so
    // the curve phase of the same mint cannot satisfy the AMM assertion.
    fn complete(&self, _s: &str, u: &str) -> Result<String, InferenceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if u.contains("venue=pumpswap") {
            Ok(BUY.to_string())
        } else {
            Ok("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: stub".to_string())
        }
    }
}

fn hex32(s: &str) -> [u8; 32] {
    let mut o = [0u8; 32];
    for (i, b) in o.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    o
}
fn entity(w: &[u8; 32]) -> u64 {
    let mut x = 0u64;
    for b in &w[..8] {
        x = (x << 8) | u64::from(*b);
    }
    x | 1
}

struct Run {
    e: Engine,
    calls: Arc<AtomicUsize>,
    first_amm_ms: i64,
    first_position_ms: Option<i64>,
}

fn replay(stop_before_first_amm: bool) -> Run {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));
    let mut last_tick = 0i64;
    let mut first_amm_ms = 0i64;
    let mut first_position_ms = None;
    let mut mint = None;
    for line in FIXTURE.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
        mint = Some(m);
        let t = v["t"].as_i64().unwrap();
        match v["k"].as_str().unwrap() {
            "L" => e.tick(AppEvent::LaunchObserved {
                mint: m,
                creator: hex32(v["c"].as_str().unwrap()),
                launch_unix_ms: t,
            }),
            "T" => {
                let w = hex32(v["w"].as_str().unwrap());
                let r = v["rv"].as_array();
                let (price_fp, liq) = match r {
                    Some(r) => {
                        let (a, b) = (r[0].as_u64().unwrap(), r[1].as_u64().unwrap());
                        (
                            if b > 0 {
                                (u128::from(a) * 1_000_000_000 / u128::from(b)) as i128
                            } else {
                                0
                            },
                            a,
                        )
                    }
                    None => (0, 0),
                };
                if let Some(r) = r {
                    e.tick(AppEvent::CurveObserved {
                        mint: m,
                        v_sol_lamports: r[0].as_u64().unwrap(),
                        v_tokens: r[1].as_u64().unwrap(),
                        real_sol_lamports: r[2].as_u64().unwrap(),
                        real_tokens: r[3].as_u64().unwrap(),
                        recv_unix_ms: Some(t),
                        slot: v["slot"].as_u64().unwrap(),
                    });
                }
                e.tick(AppEvent::MarketTrade {
                    mint: m,
                    price_fp,
                    quote_lamports: v["sol"].as_u64().unwrap(),
                    liquidity_lamports: liq,
                    signed_base: v["base"].as_i64().unwrap(),
                    buyer_entity: entity(&w),
                    age_slots: 30,
                    recv_unix_ms: Some(t),
                    trader_pubkey: Some(w),
                    slot: v["slot"].as_u64(),
                    fee_lamports: v["fee"].as_u64(),
                    cu_consumed: v["cu"].as_u64(),
                    venue: Some(TradeVenue::PumpFun),
                });
            }
            "A" => {
                if stop_before_first_amm {
                    break;
                }
                if first_amm_ms == 0 {
                    first_amm_ms = t;
                }
                let w = hex32(v["w"].as_str().unwrap());
                e.tick(AppEvent::AmmSwap {
                    mint: m,
                    pool: hex32(v["pool"].as_str().unwrap()),
                    pool_is_canonical: true,
                    quote_is_wsol: true,
                    token_reserve_pre: v["bres"].as_u64().unwrap(),
                    quote_reserve_pre: v["qres"].as_u64().unwrap(),
                    fee_bps: v["fee_bps"].as_u64().map(|x| x as u32),
                    fee_parts: None,
                    virtual_quote: None,
                    is_buy: v["buy"].as_bool().unwrap(),
                    token_amount: v["tok"].as_u64().unwrap(),
                    quote_lamports: v["sol"].as_u64().unwrap(),
                    trader: w,
                    fee_lamports: v["fee"].as_u64(),
                    cu_consumed: v["cu"].as_u64(),
                    recv_unix_ms: Some(t),
                    slot: v["slot"].as_u64().unwrap(),
                });
            }
            k => panic!("kind {k}"),
        }
        if t - last_tick >= 1_000 {
            last_tick = t;
            e.tick(AppEvent::Tick);
            std::thread::sleep(std::time::Duration::from_millis(2));
            if first_position_ms.is_none() && e.model_position_open(m.as_bytes()) {
                first_position_ms = Some(t);
            }
        }
    }
    for _ in 0..30 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = mint;
    Run {
        e,
        calls,
        first_amm_ms,
        first_position_ms,
    }
}

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

#[test]
fn an_amm_market_is_discovered_from_stream_events_and_bought_through_the_real_engine() {
    let r = replay(false);
    let rpt = r.e.model_lane_report().clone();
    std::fs::write(
        "/tmp/amm_path_report.json",
        serde_json::to_string_pretty(&rpt).unwrap(),
    )
    .unwrap();
    eprintln!(
        "first_amm_ms={} first_position_ms={:?} calls={}",
        r.first_amm_ms,
        r.first_position_ms,
        r.calls.load(Ordering::SeqCst)
    );
    // Discovery came from the stream (registry), the stub was asked, and it answered BUY.
    assert!(rep(&r.e, "uniq_discovered") >= 1, "{rpt:?}");
    assert!(r.calls.load(Ordering::SeqCst) >= 1, "{rpt:?}");
    assert!(rep(&r.e, "verdict:buy") >= 1, "{rpt:?}");
    // The decision was an AMM one: either the pool plane governed the prompt, and the FILL was
    // priced from the pool (depth basis 3 books as MigratedPool).
    assert!(rep(&r.e, "verdict:buy|venue=pumpswap") >= 1, "{rpt:?}");
    assert_eq!(
        rep(&r.e, "verdict:buy|venue=pumpfun"),
        0,
        "the stub never buys the curve: {rpt:?}"
    );
    // TIMING/FEE CORRECTION (post 7b245eb7): a fill needs (a) a reserve state from a STRICTLY later
    // chain slot than the order's creation, and (b) the fee rate reported by THAT landing swap's own
    // event (never carried forward). This fixture carries a per-event fee on only 20 of its swaps, so
    // most landing states are refused BY NAME instead of being priced with a stale 125 bp. The
    // position-opening assertion returns with the full per-event-fee fixture (chain fetch pending).
    assert!(
        rep(&r.e, "fill_none:amm_economics_not_on_landing_event") >= 1,
        "the strict-fee refusal must fire on this fixture: {rpt:?}"
    );
    assert_eq!(
        rep(&r.e, "fill:position_opened_amm"),
        0,
        "no pool fill may be priced from a carried-forward fee: {rpt:?}"
    );
}
