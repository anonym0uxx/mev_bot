//! Operator decision 2026-10-09: SKIP Mayhem-mode coins. The paper model lane excludes a pump.fun curve whose
//! decoded `is_mayhem_mode` is true, and -- fail-closed -- one whose mode was never decoded, BY NAME, before the
//! model is asked; a verdict that comes back after the mode turned out Mayhem is discarded; a management ADD on a
//! Mayhem curve is refused while REDUCE/EXIT stay allowed.
//!
//! Synthetic events through the REAL engine entry point (`Engine::tick`). The mode event is the same
//! `AppEvent::CurveModeObserved` the daemon emits from decoded account bytes (see the junction test
//! `mayhem_exclusion_wire.rs` for the captured-bytes leg).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_mayhem::CurveModeExclusion;
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

const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const ADD: &str = "DECISION: ADD\nINVALIDATION: none\nEVIDENCE: x";
const REDUCE: &str = "DECISION: REDUCE\nINVALIDATION: none\nEVIDENCE: x";

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

/// Entry prompts get BUY (after an optional delay); management prompts get `mgmt`.
struct Stub {
    calls: Arc<AtomicUsize>,
    delay_ms: u64,
    mgmt: &'static str,
}

impl ModelSource for Stub {
    fn complete(&self, _s: &str, user: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.delay_ms));
        }
        if user.starts_with("Decide the next action for a position you already hold") {
            return Ok(self.mgmt.to_string());
        }
        Ok(BUY.to_string())
    }
}

fn armed(delay_ms: u64, mgmt: &'static str) -> (Engine, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Stub {
        calls: Arc::clone(&calls),
        delay_ms,
        mgmt,
    });
    (e, calls)
}

fn mode(mayhem: bool) -> AppEvent {
    AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem,
        slot: 1_999,
    }
}

/// Launch + 40 priced prints + a curve observation; `mode_ev` (if any) is fed right after the curve.
fn events(mode_ev: Option<bool>) -> Vec<AppEvent> {
    let mut ev = vec![AppEvent::LaunchObserved {
        mint: mint(),
        creator: CREATOR,
        launch_unix_ms: T0,
    }];
    for i in 0..40u32 {
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
    ev.push(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(t_last()),
        slot: 2_000,
    });
    if let Some(m) = mode_ev {
        ev.push(mode(m));
    }
    ev.push(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

fn t_last() -> i64 {
    T0 + 1_000 + 40 * 2_000
}

fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn drive(e: &mut Engine, evs: &[AppEvent], n: usize) {
    for ev in evs {
        e.tick(*ev);
    }
    ticks(e, n);
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
}

fn print(e: &mut Engine, i: u32, ts: i64, slot: u64) {
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
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

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

#[test]
fn control_a_decoded_canonical_curve_is_in_the_cohort_and_reaches_a_position() {
    let (mut e, calls) = armed(0, HOLD);
    drive(&mut e, &events(Some(false)), 8);
    assert_eq!(e.model_curve_mode(&MINT), Some(false));
    assert_eq!(e.model_curve_mode_exclusion(&MINT), None);
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(e.model_pending_orders(), 1, "{:?}", e.model_lane_report());
    curve(&mut e, t_last() + 1_500, 2_100, 200_000_000);
    ticks(&mut e, 6);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    assert_eq!(rep(&e, "refuse:mayhem_mode_excluded"), 0);
    assert_eq!(rep(&e, "refuse:curve_mode_unknown"), 0);
}

#[test]
fn a_mayhem_curve_is_refused_by_name_and_never_reaches_the_model() {
    let (mut e, calls) = armed(0, HOLD);
    drive(&mut e, &events(Some(true)), 8);
    assert_eq!(
        e.model_curve_mode_exclusion(&MINT),
        Some(CurveModeExclusion::MayhemMode)
    );
    assert!(
        rep(&e, "refuse:mayhem_mode_excluded") >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(rep(&e, "uniq_mayhem_mode_excluded"), 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no model call on an excluded market"
    );
    assert_eq!(rep(&e, "dispatched"), 0);
    assert_eq!(
        rep(&e, "snapshot_ok"),
        0,
        "an excluded market never counts as ready"
    );
    assert_eq!(e.model_pending_orders(), 0);
    curve(&mut e, t_last() + 1_500, 2_100, 200_000_000);
    ticks(&mut e, 6);
    assert!(!e.model_position_open(&MINT));
    // The funnel names WHY it never became ready.
    let f = e.model_funnel();
    assert!(
        f.keys().any(|k| k.ends_with("last=mayhem_mode_excluded")),
        "{f:?}"
    );
}

#[test]
fn an_unknown_curve_mode_is_refused_fail_closed_never_read_as_not_mayhem() {
    let (mut e, calls) = armed(0, HOLD);
    drive(&mut e, &events(None), 8);
    assert_eq!(e.model_curve_mode(&MINT), None);
    assert_eq!(
        e.model_curve_mode_exclusion(&MINT),
        Some(CurveModeExclusion::ModeUnknown)
    );
    assert!(
        rep(&e, "refuse:curve_mode_unknown") >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(e.model_pending_orders(), 0);
    // Learning the mode later (canonical) re-admits it on the next observation: the refusal was not a guess.
    e.tick(mode(false));
    curve(&mut e, t_last() + 500, 2_001, 0);
    ticks(&mut e, 8);
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "{:?}",
        e.model_lane_report()
    );
}

#[test]
fn a_mayhem_observation_is_sticky_and_a_conflict_is_counted() {
    let (mut e, calls) = armed(0, HOLD);
    let mut evs = events(Some(true));
    // A later disagreeing (canonical) observation must NOT readmit the market.
    let pos = evs.len() - 1;
    evs.insert(pos, mode(false));
    drive(&mut e, &evs, 8);
    assert_eq!(e.model_curve_mode(&MINT), Some(true));
    assert_eq!(rep(&e, "mayhem:mode_conflict"), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(e.model_pending_orders(), 0);
}

#[test]
fn a_verdict_returning_after_the_curve_turned_out_mayhem_is_discarded_by_name() {
    // The ask is dispatched on a canonical reading; the mode flips to Mayhem while the model is thinking.
    let (mut e, calls) = armed(150, HOLD);
    for ev in events(Some(false)) {
        e.tick(ev);
    }
    e.tick(AppEvent::Tick);
    assert_eq!(
        e.model_lane_report().get("dispatched").copied(),
        Some(1),
        "{:?}",
        e.model_lane_report()
    );
    // A conflicting Mayhem observation lands while the ask is in flight (sticky -> excluded).
    e.tick(mode(true));
    std::thread::sleep(Duration::from_millis(300));
    ticks(&mut e, 4);
    assert!(calls.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        rep(&e, "discard:mayhem_mode_excluded"),
        1,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(
        e.model_pending_orders(),
        0,
        "no order on an excluded market"
    );
    curve(&mut e, t_last() + 1_500, 2_100, 200_000_000);
    ticks(&mut e, 6);
    assert!(!e.model_position_open(&MINT));
}

/// Open a position on a canonical curve, then let `mgmt` drive management for ~100 s of feed time.
fn held_then_manage(mgmt: &'static str, flip_to_mayhem: bool) -> Engine {
    let (mut e, _calls) = armed(0, mgmt);
    drive(&mut e, &events(Some(false)), 8);
    curve(&mut e, t_last() + 1_500, 2_100, 200_000_000);
    ticks(&mut e, 6);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    if flip_to_mayhem {
        e.tick(mode(true));
    }
    let (mut clock, mut slot) = (t_last() + 1_500, 2_100u64);
    for n in 0..20u32 {
        clock += 5_000;
        slot += 1;
        curve(&mut e, clock, slot, 200_000_000);
        print(&mut e, 41 + n, clock, slot);
        ticks(&mut e, 2);
    }
    e
}

#[test]
fn a_management_add_on_a_mayhem_curve_is_refused_by_name() {
    let e = held_then_manage(ADD, true);
    assert!(
        rep(&e, "mgmt:refuse:add_mayhem_mode_excluded") >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert!(e.model_mgmt_pending(&MINT).is_none(), "no ADD order placed");
}

#[test]
fn control_a_management_add_on_a_canonical_curve_is_placed() {
    let e = held_then_manage(ADD, false);
    assert_eq!(rep(&e, "mgmt:refuse:add_mayhem_mode_excluded"), 0);
    assert!(
        e.model_mgmt_pending(&MINT).is_some() || !e.model_mgmt_fills().is_empty(),
        "{:?}",
        e.model_lane_report()
    );
}

#[test]
fn an_add_placed_on_a_canonical_curve_that_turns_mayhem_before_landing_is_never_filled_from_curve_formulas(
) {
    // Reaches the ADD landing-state guard (model_mgmt_add_state) PAST the placement-time refusal: the ADD is
    // placed while the curve is canonical, then the curve is observed Mayhem before the landing state arrives.
    let (mut e, _calls) = armed(0, ADD);
    drive(&mut e, &events(Some(false)), 8);
    curve(&mut e, t_last() + 1_500, 2_100, 200_000_000);
    ticks(&mut e, 6);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    let (mut clock, mut slot) = (t_last() + 1_500, 2_100u64);
    let mut placed = false;
    for n in 0..20u32 {
        clock += 5_000;
        slot += 1;
        print(&mut e, 41 + n, clock, slot);
        ticks(&mut e, 2);
        if e.model_mgmt_pending(&MINT)
            .is_some_and(|p| p.1 == pump_quant_app::engine::model_manage::MgmtKind::Add)
        {
            placed = true;
            break;
        }
        curve(&mut e, clock, slot, 200_000_000);
        ticks(&mut e, 2);
    }
    assert!(
        placed,
        "an ADD order was placed on the canonical curve: {:?}",
        e.model_lane_report()
    );
    assert!(e.model_mgmt_fills().is_empty(), "nothing filled yet");
    e.tick(mode(true));
    for n in 0..20u32 {
        clock += 5_000;
        slot += 1;
        curve(&mut e, clock, slot, 200_000_000);
        print(&mut e, 61 + n, clock, slot);
        ticks(&mut e, 2);
    }
    assert!(
        e.model_mgmt_fills().is_empty(),
        "no ADD filled from Mayhem reserves: {:?}",
        e.model_lane_report()
    );
}

#[test]
fn reduce_is_never_blocked_by_the_mayhem_exclusion() {
    let e = held_then_manage(REDUCE, true);
    assert_eq!(rep(&e, "mgmt:refuse:add_"), 0);
    // The REDUCE is still PLACED (the exclusion stops new exposure only). Operator 15:40 PT (offset slice):
    // a Mayhem curve must not inherit the ordinary curve QUOTE formulas, so the placed order is refused BY NAME
    // at pricing and never settled from Mayhem reserves (it was previously filled via curve_sell).
    assert!(
        rep(&e, "mgmt:order:reduce") >= 1,
        "a REDUCE order is still placed on a Mayhem curve: {:?}",
        e.model_lane_report()
    );
    assert!(
        rep(
            &e,
            "mgmt:quote_unavailable:curve_quote_unsupported:mayhem_mode"
        ) >= 1,
        "{:?}",
        e.model_lane_report()
    );
    assert!(e.model_mgmt_fills().is_empty());
}

#[test]
fn with_the_lane_off_the_mode_event_is_a_no_op() {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.tick(mode(true));
    assert_eq!(e.model_curve_mode(&MINT), None);
    assert!(e.model_lane_report().is_empty());
}
