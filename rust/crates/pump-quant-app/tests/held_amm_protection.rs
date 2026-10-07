//! (helpers copied from amm_path_e2e) AMM path through the REAL engine (`Engine::tick`) on CAPTURED history, discovered from stream
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

/// Feed one fixture line into the engine exactly as the replay does (one feed path for every test).
fn feed_line(e: &mut Engine, v: &serde_json::Value, m: DomainMint, t: i64) {
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
                event_id: None,
                feature: None,
            });
        }
        "A" => {
            let w = hex32(v["w"].as_str().unwrap());
            e.tick(AppEvent::AmmSwap {
                mint: m,
                pool: hex32(v["pool"].as_str().unwrap()),
                pool_is_canonical: true,
                quote_is_wsol: true,
                token_reserve_pre: v["bres"].as_u64().unwrap(),
                quote_reserve_pre: v["qres"].as_u64().unwrap(),
                fee_bps: v["fee_bps"].as_u64().map(|x| x as u32),
                fee_parts: Some((
                    v["lp"].as_u64().unwrap() as u32,
                    v["pr"].as_u64().unwrap() as u32,
                    v["cr"].as_u64().unwrap() as u32,
                )),
                virtual_quote: v["vq"].as_u64(),
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
    for line in FIXTURE.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
        let t = v["t"].as_i64().unwrap();
        if v["k"] == "A" {
            if stop_before_first_amm {
                break;
            }
            if first_amm_ms == 0 {
                first_amm_ms = t;
            }
        }
        feed_line(&mut e, &v, m, t);
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
    Run {
        e,
        calls,
        first_amm_ms,
        first_position_ms,
    }
}

fn amm_run_with_fill() -> Run {
    let r = replay(false);
    assert!(
        rep(&r.e, "fill:position_opened_amm") >= 1,
        "AMM fill required for these lifecycle cases"
    );
    r
}

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

const POOL: &str = "99f330f5fa2da45f65bcc92828d0b5e793670aa710c44a070e360b691c5fe6d1";

fn held() -> (Run, DomainMint) {
    // Replay the captured history and STOP at the first tick where the AMM position is open and held.
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));
    let mut last_tick = 0i64;
    for line in FIXTURE.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
        let t = v["t"].as_i64().unwrap();
        feed_line(&mut e, &v, m, t);
        if t - last_tick >= 1_000 {
            last_tick = t;
            e.tick(AppEvent::Tick);
            std::thread::sleep(std::time::Duration::from_millis(2));
            if rep(&e, "fill:position_opened_amm") >= 1 && e.model_position_open(m.as_bytes()) {
                return (
                    Run {
                        e,
                        calls,
                        first_amm_ms: 0,
                        first_position_ms: Some(t),
                    },
                    m,
                );
            }
        }
    }
    panic!("no held AMM position was produced by the captured replay");
}

fn swap(m: DomainMint, pool: &str, t: i64, slot: u64, buy: bool, tok: u64, sol: u64) -> AppEvent {
    swap_at(
        m,
        pool,
        t,
        slot,
        buy,
        tok,
        sol,
        200_000_000_000_000,
        60_000_000_000,
    )
}

#[allow(clippy::too_many_arguments)]
fn swap_at(
    m: DomainMint,
    pool: &str,
    t: i64,
    slot: u64,
    buy: bool,
    tok: u64,
    sol: u64,
    bres: u64,
    qres: u64,
) -> AppEvent {
    AppEvent::AmmSwap {
        mint: m,
        pool: hex32(pool),
        pool_is_canonical: true,
        quote_is_wsol: true,
        token_reserve_pre: bres,
        quote_reserve_pre: qres,
        fee_bps: Some(125),
        fee_parts: Some((2, 93, 30)),
        virtual_quote: Some(17_584_505_661),
        is_buy: buy,
        token_amount: tok,
        quote_lamports: sol,
        trader: [7u8; 32],
        fee_lamports: Some(1),
        cu_consumed: Some(1),
        recv_unix_ms: Some(t),
        slot,
    }
}

fn hint(m: DomainMint, t: i64, quote: u64) -> AppEvent {
    AppEvent::MarketTrade {
        mint: m,
        price_fp: 0,
        quote_lamports: quote,
        liquidity_lamports: 0,
        signed_base: 1,
        buyer_entity: 1,
        age_slots: 0,
        recv_unix_ms: Some(t),
        trader_pubkey: Some([9u8; 32]),
        slot: Some(1),
        fee_lamports: None,
        cu_consumed: None,
        venue: Some(TradeVenue::PumpSwap),
        event_id: None,
        feature: None,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snap {
    open: bool,
    inv: Option<u64>,
    free: u64,
    reserve_age: Option<i64>,
    print_age_minus_clock: Option<i64>,
    realized: i128,
}
fn snap(e: &Engine, m: &DomainMint) -> Snap {
    let st = e.model_held_data_status();
    let s = st.iter().find(|s| &s.mint == m.as_bytes()).unwrap();
    let v = e.model_accounting_view(m.as_bytes());
    Snap {
        open: e.model_position_open(m.as_bytes()),
        inv: e.model_inventory_tokens(m.as_bytes()),
        free: e.model_free_cash_lamports(),
        // freshness is compared as an ABSOLUTE evidence time: age + clock
        reserve_age: s.reserve_age_ms.map(|a| e.model_clock_ms_now() - a),
        print_age_minus_clock: s.last_print_age_ms.map(|a| e.model_clock_ms_now() - a),
        realized: v.realized,
    }
}

#[test]
fn instruction_hints_cannot_change_a_held_amm_positions_evidence_or_trigger_an_exit() {
    let (mut r, m) = held();
    let before = snap(&r.e, &m);
    assert!(before.open && before.inv.is_some());
    let clock0 = r.e.model_clock_ms_now();
    let rep0 = rep(&r.e, "hint:instruction_print_not_aggregated");
    // unlimited bound, finite bound, zero, and a "crash-looking" zero price: none is evidence
    for (i, q) in [u64::MAX, 68_424_999, 0, 1].into_iter().enumerate() {
        r.e.tick(hint(m, clock0 + 1 + i as i64, q));
    }
    let after = snap(&r.e, &m);
    assert_eq!(
        after, before,
        "a hint changed the mark/freshness/inventory/cash/realized"
    );
    assert!(
        r.e.model_position_open(m.as_bytes()),
        "a hint triggered an exit"
    );
    assert_eq!(rep(&r.e, "hint:instruction_print_not_aggregated"), rep0 + 4);
    assert_eq!(rep(&r.e, "protect:amm_exit"), 0);
    // the clock MAY advance for scheduling; it never refreshes market evidence
    assert!(r.e.model_clock_ms_now() >= clock0 + 4);
    let st = r.e.model_held_data_status();
    assert!(st[0].reserve_age_ms.unwrap() >= before.reserve_age.map_or(0, |_| 0));
}

#[test]
fn a_verified_pool_swap_marks_the_held_position_and_a_crash_exits_once_with_routing_evidence_only()
{
    let (mut r, m) = held();
    let t0 = r.e.model_clock_ms_now();
    let inv0 = r.e.model_inventory_tokens(m.as_bytes()).unwrap();
    let fills0 = r.e.model_all_fills().len();
    // NORMAL: an executed swap near the entry price marks without exiting. Price = sol/tok in PRICE_SCALE.
    let ok_px_tok = 1_000_000_000_000u64; // 1e12 raw tokens
                                          // mark equals roughly the pool's own spot: quote_reserve/token_reserve = 60e9/2e14
    let ok_sol = (u128::from(ok_px_tok) * 60_000_000_000 / 200_000_000_000_000) as u64;
    r.e.tick(swap(
        m,
        POOL,
        t0 + 1_000,
        900_000_001,
        true,
        ok_px_tok,
        ok_sol,
    ));
    assert_eq!(rep(&r.e, "protect:amm_mark_applied"), 1);
    // DUPLICATE: the same executed swap again is not applied twice
    r.e.tick(swap(
        m,
        POOL,
        t0 + 1_000,
        900_000_001,
        true,
        ok_px_tok,
        ok_sol,
    ));
    assert_eq!(rep(&r.e, "protect:amm_mark_applied"), 1);
    assert_eq!(rep(&r.e, "protect:amm_mark_replay_not_applied"), 1);
    // CRASH: executed price collapses by far more than the hard stop
    let cash_before = r.e.model_accounting_view(m.as_bytes()).balance;
    r.e.tick(swap_at(
        m,
        POOL,
        t0 + 2_000,
        900_000_002,
        false,
        1_000_000_000_000,
        1_000,
        200_000_000_000_000,
        3_000_000_000,
    ));
    assert!(
        !r.e.model_position_open(m.as_bytes()),
        "emergency did not close the position"
    );
    assert_eq!(rep(&r.e, "protect:amm_exit"), 1);
    // exactly once: replaying the crash swap changes nothing
    let after = r.e.model_accounting_view(m.as_bytes());
    r.e.tick(swap_at(
        m,
        POOL,
        t0 + 2_000,
        900_000_002,
        false,
        1_000_000_000_000,
        1_000,
        200_000_000_000_000,
        3_000_000_000,
    ));
    assert_eq!(r.e.model_accounting_view(m.as_bytes()), after);
    assert_eq!(rep(&r.e, "protect:amm_exit"), 1);
    // settlement: the held inventory is gone; cash moved by the booked net, and this is routing evidence only
    assert!(
        r.e.model_inventory_tokens(m.as_bytes()).is_none()
            || r.e.model_inventory_tokens(m.as_bytes()) != Some(inv0)
    );
    assert!(after.balance != cash_before || after.realized != 0);
    assert_eq!(
        r.e.model_all_fills().len(),
        fills0,
        "an exit is not a new BUY fill"
    );
    assert!(
        r.e.model_assessable_fills().is_empty(),
        "AMM routing fills are never assessable"
    );
}

#[test]
fn a_wrong_pool_swap_never_marks_or_exits_a_held_position() {
    let (mut r, m) = held();
    let t0 = r.e.model_clock_ms_now();
    let other = "11".repeat(32);
    r.e.tick(swap_at(
        m,
        &other,
        t0 + 1_000,
        900_000_010,
        false,
        1_000_000_000_000,
        1_000,
        200_000_000_000_000,
        3_000_000_000,
    ));
    assert!(
        r.e.model_position_open(m.as_bytes()),
        "an unrelated pool's print exited the position"
    );
    assert_eq!(rep(&r.e, "protect:amm_mark_applied"), 0);
    assert!(rep(&r.e, "protect:amm_mark_ignored_wrong_pool") >= 1);
}

#[test]
fn a_stale_swap_does_not_mark_and_the_position_is_reported_degraded_not_healthy() {
    let (mut r, m) = held();
    let t0 = r.e.model_clock_ms_now();
    // a hint advances the CLOCK two minutes: scheduling only
    r.e.tick(hint(m, t0 + 120_000, 1));
    // an executed swap whose own time is >60 s behind the clock is stale evidence
    r.e.tick(swap(
        m,
        POOL,
        t0 + 1,
        900_000_020,
        false,
        1_000_000_000_000,
        1_000,
    ));
    assert!(r.e.model_position_open(m.as_bytes()));
    assert_eq!(rep(&r.e, "protect:amm_mark_applied"), 0);
    assert!(rep(&r.e, "protect:amm_mark_ignored_stale") >= 1);
    let st = r.e.model_held_data_status();
    assert!(
        !st[0].reserve_fresh,
        "a hint made stale evidence look fresh"
    );
    assert!(
        !r.e.model_held_degraded().is_empty(),
        "position with no fresh evidence must surface DEGRADED, never healthy"
    );
}
