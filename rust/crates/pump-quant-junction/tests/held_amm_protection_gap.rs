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

const FIXTURE: &str = include_str!("../../pump-quant-app/tests/fixtures/amm_atoo_events.jsonl");
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
#[allow(dead_code)]
struct Snap {
    open: bool,
    inv: Option<u64>,
    free: u64,
    reserve_age: Option<i64>,
    print_age_minus_clock: Option<i64>,
    realized: i128,
}
#[allow(dead_code)]
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

use pump_quant_app::engine::model_manage::amm_protection_gap;
use pump_quant_junction::model_lifecycle::{held_data_report, StaleCallout};

const BUDGET: i64 = pump_quant_app::curve_annotation::PRICING_BUDGET_MS;

fn status_of(e: &Engine, m: &DomainMint) -> pump_quant_app::engine::model_manage::HeldDataStatus {
    e.model_held_data_status()
        .into_iter()
        .find(|s| &DomainMint::from_bytes(s.mint) == m)
        .expect("held status")
}

fn protected_alerts(c: &mut StaleCallout, e: &Engine, now: i64) -> Vec<String> {
    c.evaluate(e, now, 60_000)
        .into_iter()
        .filter(|l| l.alert && l.text.contains("UNPROTECTED (price-based)"))
        .map(|l| l.text)
        .collect()
}

#[test]
fn time_advancing_with_no_further_swap_surfaces_a_named_protection_gap_through_the_alert_path() {
    let (r, m) = held();
    let mut c = StaleCallout::default();
    let t0 = r.e.model_clock_ms_now();
    // Right after the fill the verified mark is the landing swap: protected, silent.
    assert!(amm_protection_gap(&status_of(&r.e, &m), t0).is_none());
    assert!(protected_alerts(&mut c, &r.e, t0).is_empty());
    // Only the wall/feed clock advances (no swap on this mint). Past the existing 60 s budget: named gap + alert.
    let late = t0 + BUDGET + 1;
    let gap = amm_protection_gap(&status_of(&r.e, &m), late).expect("gap after the budget");
    assert_eq!(gap, "protection_mark_stale:no_valid_update");
    let a = protected_alerts(&mut c, &r.e, late);
    assert_eq!(a.len(), 1, "ONSET alert: {a:?}");
    assert!(a[0].starts_with("ONSET") && a[0].contains("no_valid_update"));
    // Edge-triggered: a second evaluation inside the reminder window repeats nothing.
    assert!(protected_alerts(&mut c, &r.e, late + 1_000).is_empty());
    // Reminder after remind_ms.
    assert_eq!(protected_alerts(&mut c, &r.e, late + 61_000).len(), 1);
}

#[test]
fn hints_and_rejected_swaps_never_refresh_the_verified_mark_time() {
    let (mut r, m) = held();
    let t0 = r.e.model_clock_ms_now();
    let verified0 = status_of(&r.e, &m)
        .protect_mark_ms
        .expect("verified mark at the fill");
    // A hint (instruction) and an out-of-order swap on the bound pool (older than the newest observation).
    r.e.tick(hint(m, t0 + 10_000, u64::MAX));
    r.e.tick(swap_at(
        m,
        POOL,
        t0 - 5_000,
        910_000_001,
        false,
        1_000_000_000_000,
        1_000,
        200_000_000_000_000,
        60_000_000_000,
    ));
    let st = status_of(&r.e, &m);
    assert_eq!(
        st.protect_mark_ms,
        Some(verified0),
        "neither a hint nor a rejected swap is a verified mark"
    );
    // The ignored swap is named and timed, but the AGE comes from the verified mark.
    assert_eq!(st.protect_ignored.map(|x| x.0), Some("out_of_order"));
    let now = r.e.model_clock_ms_now();
    assert_eq!(
        amm_protection_gap(&st, verified0 + BUDGET + 1).as_deref(),
        Some("protection_mark_stale:no_valid_update")
    );
    assert!(
        now >= t0 + 10_000,
        "the clock advanced on the hint; the verified mark did not"
    );
    // A genuine verified swap on the bound pool DOES refresh it and clears the gap, with a RECOVERED line.
    let mut c = StaleCallout::default();
    let late = verified0 + BUDGET + 1;
    assert_eq!(protected_alerts(&mut c, &r.e, late).len(), 1);
    r.e.tick(swap_at(
        m,
        POOL,
        late,
        910_000_050,
        true,
        1_000_000_000_000,
        300_000,
        200_000_000_000_000,
        60_000_000_000,
    ));
    let st2 = status_of(&r.e, &m);
    assert_eq!(st2.protect_mark_ms, Some(late));
    assert!(amm_protection_gap(&st2, late).is_none());
    let lines = c.evaluate(&r.e, late + 1, 60_000);
    assert!(
        lines
            .iter()
            .any(|l| !l.alert && l.text.starts_with("RECOVERED")),
        "{lines:?}"
    );
}

#[test]
fn a_swap_with_no_spot_basis_is_a_named_unprotected_state_and_alerts() {
    let (mut r, m) = held();
    let t0 = r.e.model_clock_ms_now();
    // virtual quote absent: the bound pool's swap cannot yield the spot mark the entry used.
    let mut ev = swap_at(
        m,
        POOL,
        t0 + 1_000,
        910_000_100,
        true,
        1_000_000_000_000,
        300_000,
        200_000_000_000_000,
        60_000_000_000,
    );
    if let AppEvent::AmmSwap { virtual_quote, .. } = &mut ev {
        *virtual_quote = None;
    }
    r.e.tick(ev);
    let st = status_of(&r.e, &m);
    assert_eq!(st.protect_ignored.map(|x| x.0), Some("no_spot_basis"));
    assert_eq!(
        amm_protection_gap(&st, t0 + 1_000).as_deref(),
        Some("protection_mark_unavailable:missing_spot_basis")
    );
    let mut c = StaleCallout::default();
    let a = protected_alerts(&mut c, &r.e, t0 + 1_000);
    assert_eq!(a.len(), 1);
    assert!(a[0].contains("missing_spot_basis"));
    let (report, degraded) = held_data_report(&r.e);
    assert!(degraded, "{report}");
    assert!(
        report.contains("missing_spot_basis") && !report.contains("READY"),
        "never healthy from a hint: {report}"
    );
}

#[test]
fn a_price_moving_swap_then_silence_is_visible_only_to_the_next_pre_trade_state_and_then_the_budget(
) {
    // PRE-TRADE marks: the swap's own impact is NOT in its own mark. A swap that collapses the pool is invisible to
    // protection until some LATER swap reveals the new reserves (or the budget elapses and the gap is raised).
    let (mut r, m) = held();
    let t0 = r.e.model_clock_ms_now();
    // Big sell, pre-trade reserves still the healthy ones: marks healthy; the position stays open.
    r.e.tick(swap_at(
        m,
        POOL,
        t0 + 1_000,
        910_000_200,
        false,
        150_000_000_000_000,
        50_000_000_000,
        200_000_000_000_000,
        60_000_000_000,
    ));
    assert!(
        r.e.model_position_open(m.as_bytes()),
        "its own impact is not visible in its own pre-trade mark"
    );
    // Silence: nothing detects the collapse inside the budget...
    assert!(amm_protection_gap(&status_of(&r.e, &m), t0 + 1_000 + BUDGET).is_none());
    assert!(r.e.model_position_open(m.as_bytes()));
    // ...and after the budget the unprotected state is named, with no forced liquidation.
    assert!(amm_protection_gap(&status_of(&r.e, &m), t0 + 1_000 + BUDGET + 1).is_some());
    assert!(
        r.e.model_position_open(m.as_bytes()),
        "no new automatic liquidation rule"
    );
    // The next swap's PRE-trade state reveals the collapsed pool (qres 60e9 -> 3e9): the hard stop sees it.
    r.e.tick(swap_at(
        m,
        POOL,
        t0 + 5_000,
        910_000_201,
        true,
        1_000_000_000,
        100,
        200_000_000_000_000,
        3_000_000_000,
    ));
    // The safeguard fires: it creates a protective ORDER (an intent moves nothing) ...
    assert!(
        r.e.model_protect_pending_order(m.as_bytes()).is_some(),
        "resulting account state, once observed, triggers the safeguard"
    );
    assert!(
        r.e.model_position_open(m.as_bytes()),
        "an intent closes nothing"
    );
    // ... and a later landing-state swap reconciles the fill.
    r.e.tick(swap_at(
        m,
        POOL,
        t0 + 7_000,
        910_000_202,
        true,
        1_000_000_000,
        100,
        200_000_000_000_000,
        3_000_000_000,
    ));
    assert!(
        !r.e.model_position_open(m.as_bytes()),
        "the reconciled protective fill closes the position"
    );
}
