//! The paper-model path through the REAL engine entry point (`Engine::tick`):
//! launch + priced prints + curve reserves -> DecisionCache -> existing renderer -> off-thread stub
//! model -> authority -> pending order -> simulated fill -> position.
//!
//! Nothing here hand-builds a `BundleInputs`: the prompt is whatever the engine's own join produced
//! from the events below. Synthetic events prove failure handling; they do NOT establish source
//! compatibility (that is the captured-data test).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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
    // Sized so the model's SMALL clip sits under the 90 bp own-impact limit on a 37.9 SOL pool
    // (~0.34 SOL). The cap is NOT loosened: a larger bankroll would correctly be vetoed.
    c.bankroll_initial_lamports = 2_000_000_000;
    c
}

/// A stub that answers with a fixed completion after an optional delay, counting calls.
struct Stub {
    text: &'static str,
    delay_ms: u64,
    calls: Arc<AtomicUsize>,
    fail: bool,
}

impl ModelSource for Stub {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.delay_ms));
        }
        if self.fail {
            return Err(InferenceError::Transport("stub: unreachable".into()));
        }
        Ok(self.text.to_string())
    }
}

const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const BUY_NO_LIMIT: &str =
    "DECISION: BUY\nSIZE: SMALL\nINVALIDATION: none\nEVIDENCE: flow sustained";
const BUY_TIGHT: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.0000000001\nINVALIDATION: none\nEVIDENCE: x";
const SKIP: &str = "DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: x";
const GARBAGE: &str = "I think this looks pretty good, maybe buy a little?";

#[derive(Clone, Copy)]
struct Feed {
    n: u32,
    with_launch: bool,
    with_meta: bool,
    with_curve: bool,
}

fn events(f: Feed) -> Vec<AppEvent> {
    let mut ev = Vec::new();
    if f.with_launch {
        ev.push(AppEvent::LaunchObserved {
            mint: mint(),
            creator: CREATOR,
            launch_unix_ms: T0,
        });
    }
    for i in 0..f.n {
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
            fee_lamports: if f.with_meta {
                Some(60_000 + u64::from(i) * 100)
            } else {
                None
            },
            cu_consumed: if f.with_meta {
                Some(90_000 + u64::from(i))
            } else {
                None
            },
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    let t_last = T0 + 1_000 + i64::from(f.n) * 2_000;
    if f.with_curve {
        ev.push(AppEvent::CurveObserved {
            mint: mint(),
            v_sol_lamports: VSOL,
            v_tokens: VTOK,
            real_sol_lamports: 7_900_000_000,
            real_tokens: 569_000_000_000_000,
            recv_unix_ms: Some(t_last),
            slot: 2_000,
        });
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

/// Drive the engine, giving the off-thread worker real time to answer between ticks.
fn drive(e: &mut Engine, evs: &[AppEvent], ticks: usize) {
    for ev in evs {
        e.tick(*ev);
    }
    for _ in 0..ticks {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A later curve observation (the "landing state") and ticks to let the fill run.
fn landing(e: &mut Engine, after_ms: i64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(after_ms),
        slot: 2_100,
    });
    for _ in 0..6 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn armed(text: &'static str, delay_ms: u64, fail: bool) -> (Engine, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Stub {
        text,
        delay_ms,
        calls: Arc::clone(&calls),
        fail,
    });
    (e, calls)
}

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

fn t_last(n: u32) -> i64 {
    T0 + 1_000 + i64::from(n) * 2_000
}

#[test]
fn a_successful_buy_flows_from_events_to_a_position_only_after_the_simulated_fill() {
    let (mut e, calls) = armed(BUY, 0, false);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 8);
    let r = e.model_lane_report().clone();
    assert!(
        rep(&e, "snapshot_ok") >= 1,
        "the engine's own join produced a prompt: {r:?}"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "the stub model was asked: {r:?}"
    );
    assert!(rep(&e, "verdict:buy") >= 1, "{r:?}");
    // ORDER CREATION IS NOT A FILL: no landing state yet, so no position.
    assert_eq!(e.model_pending_orders(), 1, "{r:?}");
    assert!(
        !e.model_position_open(&MINT),
        "an order must not open a position"
    );
    // The simulated fill (a reserve observation at/after landing) is what opens it.
    landing(&mut e, t_last(40) + 1_500);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    assert_eq!(e.model_pending_orders(), 0);
    assert_eq!(rep(&e, "fill:position_opened"), 1);
}

#[test]
fn the_legacy_gate_is_never_consulted_in_model_mode() {
    let (mut e, _c) = armed(SKIP, 0, false);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 8);
    let rpt = e.report();
    assert_eq!(rpt.admitted, 0, "no legacy admit in model mode");
    assert_eq!(e.model_pending_orders(), 0);
    assert!(rep(&e, "verdict:skip") >= 1, "{:?}", e.model_lane_report());
}

#[test]
fn an_unparseable_completion_is_a_named_notrade_with_no_heuristic_fallback() {
    let (mut e, _c) = armed(GARBAGE, 0, false);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 8);
    assert!(
        rep(&e, "notrade:off_contract") >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(e.model_pending_orders(), 0);
    assert!(!e.model_position_open(&MINT));
    assert_eq!(e.report().admitted, 0);
}

#[test]
fn an_unreachable_endpoint_is_a_named_notrade() {
    let (mut e, _c) = armed(BUY, 0, true);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 8);
    assert!(
        rep(&e, "notrade:model_unreachable") >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert!(!e.model_position_open(&MINT));
}

#[test]
fn an_armed_lane_with_no_source_fails_closed_at_the_real_admit_branch() {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    // Arm the mode flag with NO source: the misconfiguration the lane must refuse.
    e.arm_model_mode_without_source_for_test();
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 6);
    assert!(
        rep(&e, "fault:MissingSource") >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(e.report().admitted, 0, "no fallback to the legacy gate");
    assert_eq!(rep(&e, "dispatched"), 0);
}

#[test]
fn missing_launch_fee_meta_or_curve_are_named_refusals_that_never_reach_the_model() {
    for (f, want) in [
        (
            Feed {
                n: 40,
                with_launch: false,
                with_meta: true,
                with_curve: true,
            },
            "refuse:join_launch_unknown",
        ),
        (
            Feed {
                n: 40,
                with_launch: true,
                with_meta: false,
                with_curve: true,
            },
            "refuse:join_flow_meta_missing",
        ),
        (
            Feed {
                n: 40,
                with_launch: true,
                with_meta: true,
                with_curve: false,
            },
            "refuse:join_curve_absent",
        ),
    ] {
        let (mut e, calls) = armed(BUY, 0, false);
        drive(&mut e, &events(f), 8);
        assert!(
            rep(&e, want) >= 1,
            "want {want}: {:?}",
            e.model_lane_report()
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "{want}: no model call on a refused snapshot"
        );
        assert!(!e.model_position_open(&MINT));
    }
}

#[test]
fn a_slow_model_never_blocks_the_engine_and_a_late_answer_creates_no_order() {
    // The model takes far longer than the decision deadline (feed clock).
    let (mut e, calls) = armed(BUY, 400, false);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    let started = std::time::Instant::now();
    for ev in events(f) {
        e.tick(ev);
    }
    for _ in 0..4 {
        e.tick(AppEvent::Tick);
    }
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "ticking must not wait on the model (took {:?})",
        started.elapsed()
    );
    // The feed clock moves on past the deadline while the worker is still busy.
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(t_last(40) + 20_000),
        slot: 2_050,
    });
    std::thread::sleep(Duration::from_millis(600));
    for _ in 0..4 {
        e.tick(AppEvent::Tick);
    }
    assert!(calls.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        e.model_pending_orders(),
        0,
        "a late verdict must not create an order: {:?}",
        e.model_lane_report()
    );
    assert!(!e.model_position_open(&MINT));
    assert!(
        rep(&e, "discard:late") + rep(&e, "discard:abandoned") >= 1,
        "{:?}",
        e.model_lane_report()
    );
}

#[test]
fn a_duplicate_request_for_a_held_mint_is_never_dispatched_twice() {
    let (mut e, calls) = armed(BUY, 0, false);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 8);
    landing(&mut e, t_last(40) + 1_500);
    assert!(e.model_position_open(&MINT));
    let asked = calls.load(Ordering::SeqCst);
    // Keep the feed alive and keep ticking: the held mint must not be asked about again.
    for k in 0..6 {
        e.tick(AppEvent::CurveObserved {
            mint: mint(),
            v_sol_lamports: VSOL + 200_000_000,
            v_tokens: VTOK - 4_000_000_000_000,
            real_sol_lamports: 8_100_000_000,
            real_tokens: 565_000_000_000_000,
            recv_unix_ms: Some(t_last(40) + 40_000 + k * 20_000),
            slot: 3_000 + k as u64,
        });
        for _ in 0..3 {
            e.tick(AppEvent::Tick);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        asked,
        "no second model call for a held mint"
    );
    assert_eq!(rep(&e, "fill:position_opened"), 1, "exactly one position");
}

#[test]
fn the_price_limit_constrains_the_fill_and_an_absent_limit_does_not() {
    // A limit far below the market price cannot be met: the order is refused at the FILL.
    let (mut tight, _c) = armed(BUY_TIGHT, 0, false);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut tight, &events(f), 8);
    landing(&mut tight, t_last(40) + 1_500);
    assert!(
        !tight.model_position_open(&MINT),
        "{:?}",
        tight.model_lane_report()
    );
    assert!(
        rep(&tight, "fill_none:limit") >= 1,
        "{:?}",
        tight.model_lane_report()
    );
    // An absent limit is permitted by the grammar: the fill goes ahead.
    let (mut free, _c) = armed(BUY_NO_LIMIT, 0, false);
    drive(&mut free, &events(f), 8);
    landing(&mut free, t_last(40) + 1_500);
    assert!(
        free.model_position_open(&MINT),
        "{:?}",
        free.model_lane_report()
    );
}

#[test]
fn paper_mode_cannot_reach_live_submission() {
    // A Live engine refuses every model admission by name and dispatches nothing.
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Live);
    e.enable_paper_model(Stub {
        text: BUY,
        delay_ms: 0,
        calls: Arc::clone(&calls),
        fail: false,
    });
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 6);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no model call, no order, in Live"
    );
    assert!(rep(&e, "refuse:live_forbidden") >= 1 || e.model_lane_report().is_empty());
    assert!(!e.model_position_open(&MINT));
}

#[test]
fn blocking_entries_stops_new_asks_but_the_engine_keeps_ticking() {
    let (mut e, calls) = armed(BUY, 0, false);
    e.set_model_entries_blocked(true);
    let f = Feed {
        n: 40,
        with_launch: true,
        with_meta: true,
        with_curve: true,
    };
    drive(&mut e, &events(f), 8);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "SAFETY-OFF style block: no new model asks"
    );
    assert!(
        rep(&e, "refuse:entries_blocked") >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert!(!e.model_position_open(&MINT));
}
