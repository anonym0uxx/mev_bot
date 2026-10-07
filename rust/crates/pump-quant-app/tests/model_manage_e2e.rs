//! Model-managed position lane through the REAL engine entry point (`Engine::tick`):
//! entry BUY -> simulated fill -> held position -> (60 s) management snapshot rendered by the trained
//! renderer -> asynchronous stub verdict (keyed on PROMPT CONTENT) -> REDUCE / EXIT order -> paper fill
//! -> updated position. Synthetic events: this proves execution integration and failure handling. It is
//! NOT evidence that management is profitable.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
    c.bankroll_initial_lamports = 2_000_000_000;
    c
}

/// Answers by PROMPT CONTENT: the entry family gets BUY; the management family gets whatever the
/// script says for the step number it can read in the prompt. Records every management prompt.
struct Script {
    prompts: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    answer: fn(step: i64) -> &'static str,
}

const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const REDUCE: &str = "DECISION: REDUCE\nINVALIDATION: none\nEVIDENCE: x";
const EXIT: &str = "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: x";
const ADD: &str = "DECISION: ADD\nINVALIDATION: none\nEVIDENCE: x";

impl ModelSource for Script {
    fn complete(&self, _s: &str, user: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if user.starts_with("Decide the next action for a position you already hold") {
            self.prompts.lock().unwrap().push(user.to_string());
            let step: i64 = user
                .lines()
                .find_map(|l| l.strip_prefix("STEP: "))
                .and_then(|v| v.trim().parse().ok())
                .expect("a management prompt names its step");
            return Ok((self.answer)(step).to_string());
        }
        Ok(BUY.to_string())
    }
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

struct Rig {
    e: Engine,
    prompts: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    clock: i64,
    slot: u64,
    n: u32,
}

fn rig(answer: fn(i64) -> &'static str) -> Rig {
    rig_with(2_000_000_000, answer)
}

fn rig_with(bankroll: u64, answer: fn(i64) -> &'static str) -> Rig {
    rig_floor(bankroll, 2_500, answer)
}

fn rig_floor(bankroll: u64, floor_bps: u32, answer: fn(i64) -> &'static str) -> Rig {
    rig_cfg(bankroll, floor_bps, answer, |_| {})
}

fn rig_cfg(
    bankroll: u64,
    floor_bps: u32,
    answer: fn(i64) -> &'static str,
    tweak: fn(&mut Config),
) -> Rig {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut c = cfg();
    c.bankroll_initial_lamports = bankroll;
    c.floor_fraction_bps = floor_bps;
    tweak(&mut c);
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Script {
        prompts: Arc::clone(&prompts),
        calls: Arc::clone(&calls),
        answer,
    });
    let evs = events(40);
    for ev in &evs {
        e.tick(*ev);
    }
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    assert!(
        e.model_position_open(&MINT),
        "setup: the entry must have filled: {:?}",
        e.model_lane_report()
    );
    Rig {
        e,
        prompts,
        calls,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
    }
}

impl Rig {
    /// Advance the feed clock by `ms` in 5 s prints, ticking so asks and fills can run.
    fn advance(&mut self, ms: i64) {
        let end = self.clock + ms;
        while self.clock < end {
            self.clock += 5_000;
            self.slot += 1;
            self.n += 1;
            curve_quiet(&mut self.e, self.clock, self.slot);
            print(&mut self.e, self.n, self.clock, self.slot);
            ticks(&mut self.e, 2);
        }
    }

    /// Advance in 1 s steps until a management ORDER is pending (or `max_ms` elapses), then stop, so
    /// the fill TTL has not run when the test supplies a landing state.
    fn advance_to_order(&mut self, max_ms: i64) {
        let end = self.clock + max_ms;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.clock += 1_000;
            self.slot += 1;
            self.n += 1;
            curve_quiet(&mut self.e, self.clock, self.slot);
            print(&mut self.e, self.n, self.clock, self.slot);
            ticks(&mut self.e, 2);
        }
    }

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
fn nothing_is_asked_before_the_60s_hold_and_the_first_prompt_is_the_trained_renderer_at_step_0() {
    let mut r = rig(|_| HOLD);
    r.advance(30_000);
    assert_eq!(
        r.rep("mgmt:dispatched"),
        0,
        "the corpus has no decision before 60 s held: {:?}",
        r.e.model_lane_report()
    );
    r.advance(40_000);
    assert!(
        r.rep("mgmt:dispatched") >= 1,
        "{:?}",
        r.e.model_lane_report()
    );
    let p = r.prompts.lock().unwrap().clone();
    let first = p.first().expect("a management prompt was rendered");
    assert!(first.starts_with("Decide the next action for a position you already hold."));
    assert!(first.contains("\nSTEP: 0\n"));
    assert!(first.contains("POSITION STATE (at the decision instant, before any action):"));
    assert!(first.contains("  inventory: "));
    // The mint is the base58 address, as in the corpus, never hex.
    assert!(!first.contains(&"ab".repeat(32)));
    assert!(
        first.contains("holding time: 6"),
        "held seconds are real: {first}"
    );
}

#[test]
fn reduce_sells_half_the_inventory_through_a_fill_and_only_the_fill_changes_inventory() {
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    let inv0 =
        r.e.model_inventory_tokens(&MINT)
            .expect("fill established inventory");
    assert!(inv0 > 0);
    r.advance_to_order(120_000);
    // The verdict created an ORDER INTENT; inventory is untouched until a landing state fills it.
    if let Some((_, _, intended, filled)) = r.e.model_mgmt_pending(&MINT) {
        assert_eq!(filled, 0);
        assert_eq!(
            intended,
            inv0 / 2,
            "half of the inventory held at order time, floored"
        );
        assert_eq!(
            r.e.model_inventory_tokens(&MINT),
            Some(inv0),
            "intent is not a fill"
        );
    }
    r.landing(250_000_000);
    r.landing(260_000_000);
    let fills = r.e.model_mgmt_fills().to_vec();
    assert_eq!(
        fills.len(),
        1,
        "exactly one fill: {:?}",
        r.e.model_lane_report()
    );
    assert_eq!(fills[0].tokens, inv0 / 2);
    assert!(!fills[0].closed);
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0 - inv0 / 2),
        "inventory reduced by exactly the filled quantity"
    );
    assert!(
        r.e.model_position_open(&MINT),
        "a REDUCE keeps the position open"
    );
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
}

#[test]
fn exit_closes_through_the_fill_and_a_late_duplicate_cannot_repeat_it() {
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD });
    r.advance_to_order(120_000);
    // One landing state fills the EXIT. (A second would let the always-BUY entry stub legitimately
    // re-enter the mint, which is the entry lane's business, not management's.)
    r.landing(250_000_000);
    assert!(
        !r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.e.model_mgmt_fills().len(), 1);
    assert!(r.e.model_mgmt_fills()[0].closed);
    // Duplicate / late reconciled report for the same order: refused, nothing booked twice.
    let id = r.e.model_mgmt_fills()[0].order_id;
    assert!(r
        .e
        .model_mgmt_apply_reconciled_fill(MINT, id, 1, 22_000)
        .is_err());
    assert_eq!(r.e.model_mgmt_fills().len(), 1);
}

#[test]
fn add_targets_half_the_reconciled_inventory_and_only_the_fill_changes_state() {
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let cash0 = r.e.model_free_cash_lamports();
    r.advance_to_order(120_000);
    let (_id, kind, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("ADD order pending");
    assert_eq!(format!("{kind:?}"), "Add");
    assert_eq!(
        intended,
        inv0 / 2,
        "target = floor(50% of reconciled inventory), NOT account capital"
    );
    assert_eq!(filled, 0);
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0),
        "an intent is not a fill"
    );
    assert!(
        r.e.model_free_cash_lamports() <= cash0,
        "the reservation can only reduce free cash"
    );
    r.landing(250_000_000);
    r.landing(260_000_000);
    let fills = r.e.model_mgmt_fills().to_vec();
    assert_eq!(fills.len(), 1, "{:?}", r.e.model_lane_report());
    assert!(fills[0].is_add);
    assert!(
        fills[0].tokens >= intended,
        "minimal notional that delivers at least the target"
    );
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0 + fills[0].tokens)
    );
    assert_eq!(
        fills[0].cost_lamports,
        fills[0].spent_lamports
            + fills[0].spent_lamports
                * u64::from(pump_quant_app::cost_model::venue_fee_bps_per_leg(
                    VSOL + 260_000_000
                ))
                / 10_000
            + pump_quant_app::cost_model::FIXED_LAMPORTS_PER_LEG,
        "all-in cost = notional + landing-venue fee + fixed leg"
    );
    assert_eq!(
        r.e.model_free_cash_lamports(),
        cash0.saturating_sub(fills[0].cost_lamports),
        "cash fell by exactly the all-in cost of the fill"
    );
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
    assert!(r.e.model_management_complete());
}

/// NOTE: this only shows determinism given the executable state. That entry price never enters the
/// amount is STRUCTURAL: `model_mgmt_add_plan_at` takes (need tokens, venue state, cash bounds) and has no
/// entry-price parameter at all (unit-tested in `add_planner_tests`).
#[test]
fn add_amount_is_deterministic_given_the_executable_state() {
    let run = || {
        let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
        r.advance_to_order(120_000);
        r.landing(250_000_000);
        r.landing(260_000_000);
        r.e.model_mgmt_fills()
            .first()
            .map(|f| (f.tokens, f.spent_lamports))
    };
    assert_eq!(run(), run());
    assert!(run().is_some());
}

#[test]
fn a_safety_off_trip_cancels_a_pending_add_but_never_a_reduce_or_exit() {
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
    r.advance_to_order(120_000);
    assert!(r.e.model_mgmt_pending(&MINT).is_some());
    r.e.model_safety_trip_operator();
    assert!(
        r.e.model_mgmt_pending(&MINT).is_none(),
        "risk-increasing ADD is invalidated"
    );
    assert!(r.rep("safety:add_order_invalidated") >= 1);
    assert!(r.e.model_mgmt_fills().is_empty(), "nothing was booked");
    // REDUCE under a trip is still permitted and still fills.
    let mut r2 = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r2.advance_to_order(120_000);
    r2.e.model_safety_trip_operator();
    assert!(
        r2.e.model_mgmt_pending(&MINT).is_some(),
        "REDUCE survives the trip"
    );
    r2.landing(250_000_000);
    r2.landing(260_000_000);
    assert_eq!(r2.e.model_mgmt_fills().len(), 1);
}

#[test]
fn a_new_add_is_refused_by_name_while_safety_off_holds() {
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
    r.e.model_safety_trip_operator();
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.advance(100_000);
    assert!(
        r.rep("mgmt:refuse:add_blocked_safety_off") >= 1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
}

#[test]
fn an_uncertain_management_order_stays_pending_survives_a_trip_and_blocks_rearm_until_resolved() {
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD });
    r.advance_to_order(120_000);
    let (id, _, _, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    r.e.model_safety_trip_operator();
    // The simulator must NOT fill or expire an order whose acknowledgement is unknown.
    r.landing(250_000_000);
    r.landing(260_000_000);
    r.advance(60_000);
    assert!(
        r.e.model_mgmt_pending(&MINT).is_some(),
        "unresolved stays pending"
    );
    assert!(r.e.model_mgmt_fills().is_empty());
    assert!(r.e.model_position_open(&MINT));
    assert!(
        r.e.model_safety_rearm("alon").is_err(),
        "re-arm refused with an uncertain order"
    );
    let a = r.e.model_stop_assessment();
    assert!(a.uncertain_orders >= 1 && !a.is_flat_and_reconciled());
    // Only a reconciled report resolves it: exactly the filled quantity, nothing more.
    let (_, _, intended, _) = r.e.model_mgmt_pending(&MINT).unwrap();
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .unwrap();
    assert!(!r.e.model_position_open(&MINT));
}
#[test]
fn a_late_or_duplicate_add_report_cannot_repeat_the_add() {
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("ADD pending");
    let part = intended / 3;
    // a partial reconciled fill changes ONLY the filled quantity and the spend
    r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, part, 1_000_000)
        .unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 + part));
    let (_, _, i2, f2) =
        r.e.model_mgmt_pending(&MINT)
            .expect("remainder still pending");
    assert_eq!((i2, f2), (intended, part));
    // an over-sized report and a SELL-shaped report for an ADD order are refused
    assert!(r
        .e
        .model_mgmt_apply_reconciled_add_fill(MINT, id, intended, 1_000_000)
        .is_err());
    assert!(
        r.e.model_mgmt_apply_reconciled_fill(MINT, id, 1, 22_000)
            .is_err(),
        "wrong kind"
    );
    // the remainder completes it; a further report finds nothing pending
    r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, intended - part, 1_000_000)
        .unwrap();
    assert!(r
        .e
        .model_mgmt_apply_reconciled_add_fill(MINT, id, 1, 1_000_000)
        .is_err());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 + intended));
}

#[test]
fn partial_fill_keeps_the_remainder_pending_and_monitored() {
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD });
    r.advance_to_order(120_000);
    let (id, _, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("EXIT order pending");
    assert_eq!(filled, 0);
    let part = intended / 3;
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000)
        .unwrap();
    let (_, _, intended2, filled2) =
        r.e.model_mgmt_pending(&MINT)
            .expect("remainder still pending");
    assert_eq!((intended2, filled2), (intended, part));
    assert!(
        r.e.model_position_open(&MINT),
        "a partial fill leaves the position open and monitored"
    );
    // An over-sized report is refused and changes nothing.
    assert!(r
        .e
        .model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .is_err());
    // The remainder fills; only then does the position close.
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended - part, 22_000)
        .unwrap();
    assert!(!r.e.model_position_open(&MINT));
}

#[test]
fn a_verdict_bound_to_an_older_position_version_is_discarded() {
    // Slow answer for step 0; a reconciled fill changes the position version meanwhile.
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r.advance_to_order(120_000);
    // The REDUCE order is pending. Fill part of it: the version bumps.
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("pending");
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended / 2, 22_000)
        .unwrap();
    assert!(r.e.model_inventory_tokens(&MINT).is_some());
    assert_eq!(
        r.rep("mgmt:order:reduce"),
        1,
        "one instruction produced one order"
    );
}

#[test]
fn legacy_exits_do_not_close_a_model_managed_position_but_the_rug_precursor_still_does() {
    let mut r = rig(|_| HOLD);
    r.advance(200_000);
    assert!(
        r.e.model_position_open(&MINT),
        "no legacy time-stop/ladder closed it: {:?}",
        r.e.model_lane_report()
    );
    // A single-print collapse: the agreed hard safeguard, independent of the model.
    r.clock += 1_000;
    r.slot += 1;
    r.e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 20_000,
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 777,
        age_slots: 30,
        recv_unix_ms: Some(r.clock),
        trader_pubkey: Some(wallet(999)),
        slot: Some(r.slot),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
    assert!(
        !r.e.model_position_open(&MINT),
        "the rug precursor must still protect a model-managed position"
    );
    assert!(r.calls.load(Ordering::SeqCst) >= 1);
}

#[test]
fn sent_state_age_measures_the_observation_inside_the_prompt_not_the_cut_instant() {
    // Entry path. The feed prints every 2 s; the engine's clock advances on a curve observation that
    // arrives 1.5 s after the newest print, so the state that is SENT is at least that old.
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        prompts,
        calls: Arc::new(AtomicUsize::new(0)),
        answer: |_| HOLD,
    });
    for ev in events(40) {
        e.tick(ev);
    }
    ticks(&mut e, 8);
    let r = e.model_lane_report().clone();
    let n = *r.get("sent_state_n").unwrap_or(&0);
    let sum = *r.get("sent_state_age_ms_sum").unwrap_or(&0);
    assert!(n >= 1, "{r:?}");
    assert!(
        sum > 0,
        "the sent-state age must be able to show staleness (it was identically 0): {r:?}"
    );
}

/// The wallet identity that must hold after EVERY step, from the engine's own money view:
///   balance == seed + realized
///   free    == balance - committed - (pending entry clips) - (ADD reservations)
///   committed == the open position's attributed all-in cost (single position in the rig)
/// Stated independently of any one action's formula, so a leak in any path breaks it.
fn assert_wallet_ties(r: &Rig, tag: &str) {
    let v = r.e.model_accounting_view(&MINT);
    let seed = i128::from(v.seed);
    assert_eq!(
        i128::from(v.balance),
        (seed + v.realized).clamp(0, i128::from(u64::MAX)),
        "{tag}: balance == seed + realized: {v:?}"
    );
    if let Some(att) = v.attribution_entry_spend {
        assert_eq!(
            v.committed, att,
            "{tag}: committed == the position's attributed cost: {v:?}"
        );
    } else {
        assert_eq!(v.committed, 0, "{tag}: flat => nothing committed: {v:?}");
    }
    assert!(
        v.free <= v.balance.saturating_sub(v.committed),
        "{tag}: free never exceeds balance - committed: {v:?}"
    );
}

#[test]
fn wallet_cash_inventory_basis_and_realized_tie_out_through_add_reduce_and_exit() {
    // ADD, then REDUCE, then EXIT, each filled through landing state. After each step the wallet identity
    // holds and the numbers equal hand-derived values.
    let mut r = rig(|step| match step {
        0 => ADD,
        1 => REDUCE,
        2 => EXIT,
        _ => HOLD,
    });
    assert_wallet_ties(&r, "after entry");
    let v0 = r.e.model_accounting_view(&MINT);
    let inv0 = v0.inventory_tokens.unwrap();
    let basis0 = v0.remaining_cost_basis.unwrap();

    // ---- ADD ----
    r.advance_to_order(120_000);
    r.landing(250_000_000);
    r.landing(260_000_000);
    let add =
        r.e.model_mgmt_fills()
            .iter()
            .find(|f| f.is_add)
            .copied()
            .expect("ADD filled");
    let v1 = r.e.model_accounting_view(&MINT);
    assert_wallet_ties(&r, "after ADD");
    assert_eq!(
        v1.inventory_tokens,
        Some(inv0 + add.tokens),
        "inventory grew by exactly the filled tokens"
    );
    assert_eq!(v1.realized, v0.realized, "a buy realizes nothing");
    assert_eq!(
        v1.committed,
        v0.committed + add.cost_lamports,
        "committed grew by exactly the all-in ADD cost"
    );
    assert_eq!(
        v1.free,
        v0.free - add.cost_lamports,
        "free cash fell by exactly the all-in cost"
    );
    assert_eq!(
        v1.remaining_cost_basis,
        Some(basis0 + add.cost_lamports),
        "cost basis += all-in ADD cost"
    );

    // ---- REDUCE ---- (the next management ask after the ADD)
    let n_fills = r.e.model_mgmt_fills().len();
    while r.e.model_mgmt_fills().len() == n_fills && r.clock < T0 + 3_000_000 {
        r.advance(10_000);
        r.landing(270_000_000 + (r.clock as u64 % 1_000));
    }
    let red = r.e.model_mgmt_fills().iter().find(|f| !f.is_add).copied();
    assert!(
        red.is_some(),
        "the REDUCE leg must actually run, not be skipped: {:?}",
        r.e.model_lane_report()
    );
    if let Some(red) = red {
        let v2 = r.e.model_accounting_view(&MINT);
        assert_wallet_ties(&r, "after REDUCE");
        assert_eq!(
            v2.inventory_tokens,
            Some(v1.inventory_tokens.unwrap() - red.tokens),
            "inventory fell by exactly the filled tokens"
        );
        assert_eq!(
            v2.realized,
            v1.realized + red.net_lamports,
            "realized changed by exactly the fill's net"
        );
        // Released cost = the sold share of committed cost (floor): committed + released == before.
        let released = v1.committed - v2.committed;
        assert_eq!(
            released,
            (u128::from(v1.committed) * u128::from(red.tokens)
                / u128::from(v1.inventory_tokens.unwrap())) as u64
        );
        // Proceeds are cash: free rose by released cost + net realized (net already nets cost and fees).
        assert_eq!(
            i128::from(v2.free),
            i128::from(v1.free) + i128::from(released) + red.net_lamports,
            "cash == released basis + realized net"
        );
    }
}

#[test]
fn a_partial_reduce_fill_changes_only_filled_quantity_cash_and_basis() {
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    let v0 = r.e.model_accounting_view(&MINT);
    let part = intended / 3;
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000)
        .unwrap();
    let v1 = r.e.model_accounting_view(&MINT);
    let f = *r.e.model_mgmt_fills().last().unwrap();
    assert_eq!(f.tokens, part);
    assert_wallet_ties(&r, "after partial");
    assert_eq!(
        v1.inventory_tokens,
        Some(v0.inventory_tokens.unwrap() - part)
    );
    assert_eq!(v1.realized, v0.realized + f.net_lamports);
    // The unfilled remainder changed NOTHING: still pending for exactly intended - part.
    let (_, _, i2, f2) = r.e.model_mgmt_pending(&MINT).unwrap();
    assert_eq!((i2, f2), (intended, part));
    // Cost basis: the position store keeps cost * remaining_bps; the released share matches the fraction.
    let frac = u128::from(part) * 10_000 / u128::from(v0.inventory_tokens.unwrap());
    let expect_basis =
        (u128::from(v0.remaining_cost_basis.unwrap()) * (10_000 - frac) / 10_000) as u64;
    assert!(
        v1.remaining_cost_basis.unwrap().abs_diff(expect_basis) <= 1,
        "{:?} vs {expect_basis}",
        v1.remaining_cost_basis
    );
}

#[test]
fn an_uncertain_add_changes_nothing_in_the_wallet_until_it_is_reconciled() {
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("ADD pending");
    let v0 = r.e.model_accounting_view(&MINT);
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    let reserved_free = r.e.model_free_cash_lamports();
    r.landing(250_000_000);
    r.landing(260_000_000);
    r.advance(60_000);
    let v1 = r.e.model_accounting_view(&MINT);
    assert_eq!(
        (v1.inventory_tokens, v1.committed, v1.realized),
        (v0.inventory_tokens, v0.committed, v0.realized),
        "an unacknowledged order moves neither inventory, committed capital nor realized cash"
    );
    assert!(r.e.model_mgmt_fills().is_empty());
    assert_eq!(
        r.e.model_free_cash_lamports(),
        reserved_free,
        "its cash stays RESERVED, not spent and not released"
    );
    assert!(r.e.model_stop_assessment().uncertain_orders >= 1);
    // A reconciled report resolves it for exactly what the chain says (here: a partial of the intended size).
    let part = intended / 2;
    r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, part, 1_000_000)
        .unwrap();
    let v2 = r.e.model_accounting_view(&MINT);
    assert_eq!(
        v2.inventory_tokens,
        Some(v0.inventory_tokens.unwrap() + part)
    );
    assert_wallet_ties(&r, "after reconciled uncertain ADD");
}

#[test]
fn held_data_readiness_is_measured_and_a_stale_reserve_degrades_while_protection_and_the_bound_stay(
) {
    let mut r = rig(|_| HOLD);
    // Fresh: the rig's curve observation is current, a prompt can be cut.
    r.advance(10_000);
    let st = r.e.model_held_data_status();
    assert_eq!(st.len(), 1);
    assert!(st[0].reserve_fresh, "{st:?}");
    assert_eq!(st[0].management_ready, Ok(()), "{st:?}");
    assert!(st[0].reserve_age_ms.unwrap() <= 60_000);
    assert!(r.e.model_held_degraded().is_empty());
    assert_eq!(
        r.e.model_held_mints(),
        vec![MINT],
        "held mints are exactly what the daemon must keep fed"
    );

    // Fresh TRADES keep arriving but the reserve stops updating (the account feed died): after the 60 s
    // pricing bound the position is DEGRADED, named, and a fresh print does not hide it.
    let t0 = r.clock;
    while r.clock < t0 + 75_000 {
        r.clock += 5_000;
        r.slot += 1;
        r.n += 1;
        print(&mut r.e, r.n, r.clock, r.slot); // trades only; NO curve observation
        ticks(&mut r.e, 2);
    }
    let st = r.e.model_held_data_status();
    assert!(!st[0].reserve_fresh, "{st:?}");
    assert!(st[0].reserve_age_ms.unwrap() > 60_000, "{st:?}");
    assert!(
        st[0].last_print_age_ms.unwrap() < 10_000,
        "the newest PRINT is fresh - that proves nothing about the reserve: {st:?}"
    );
    let why = st[0].management_ready.clone().unwrap_err();
    assert!(
        why.contains("curve"),
        "the refusal names the stale component: {why}"
    );
    assert_eq!(r.e.model_held_degraded().len(), 1);
    // The bound was NOT loosened to make it pass, the position was not closed or abandoned, and the
    // print-driven safeguards are still armed (the rug precursor closes it on a collapse print).
    assert!(r.e.model_position_open(&MINT));
    r.clock += 1_000;
    r.slot += 1;
    r.e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 20_000,
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 777,
        age_slots: 30,
        recv_unix_ms: Some(r.clock),
        trader_pubkey: Some(wallet(999)),
        slot: Some(r.slot),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
    assert!(
        !r.e.model_position_open(&MINT),
        "independent protection still works while degraded"
    );
    // Recovery: a fresh reserve observation restores readiness without any threshold change.
}

// ===================== HELD-STATE RESTORE =====================

fn held_path(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pq_held_e2e_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("held.json")
}

/// A brand-new engine, same config, armed the same way, NOTHING carried over except the file.
fn fresh_engine(bankroll: u64, held: &std::path::Path) -> Engine {
    let mut c = cfg();
    c.bankroll_initial_lamports = bankroll;
    c.floor_fraction_bps = 2_500;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Script {
        prompts: Arc::new(Mutex::new(Vec::new())),
        calls: Arc::new(AtomicUsize::new(0)),
        answer: |_| HOLD,
    });
    e.model_held_attach(held);
    e
}

#[test]
fn a_restart_rebuilds_the_position_the_wallet_and_the_management_history_from_the_file_alone() {
    let hp = held_path("a");
    let mut r = rig(|_| HOLD);
    r.e.model_held_attach(&hp);
    r.advance(130_000);
    assert!(r.e.model_held_persist_now());
    let before = r.e.model_accounting_view(&MINT);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let basis = before.remaining_cost_basis;
    let led = r.e.model_held_ledger();
    assert_eq!(led.held.len(), 1);
    assert!(
        led.held[0].step >= 1,
        "management history exists to be preserved: {:?}",
        led.held[0]
    );
    drop(r);

    let mut e2 = fresh_engine(2_000_000_000, &hp);
    let rep = e2
        .model_held_restore()
        .expect("restore")
        .expect("a ledger existed");
    assert_eq!(rep.positions, 1);
    assert_eq!(rep.inventory_unknown, 0);
    assert!(e2.model_position_open(&MINT));
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        Some(inv),
        "inventory restored exactly"
    );
    let after = e2.model_accounting_view(&MINT);
    assert_eq!(after.seed, before.seed);
    assert_eq!(after.realized, before.realized);
    assert_eq!(
        after.committed, before.committed,
        "committed capital restored"
    );
    assert_eq!(
        after.remaining_cost_basis, basis,
        "remaining cost basis restored"
    );
    assert_eq!(
        after.attribution_entry_spend,
        before.attribution_entry_spend
    );
    assert_eq!(after.balance, before.balance);
    // The restored ledger is identical to the one written: nothing was invented or dropped.
    let mut a = e2.model_held_ledger();
    let mut b = led;
    a.written_wall_ms = 0;
    b.written_wall_ms = 0;
    assert_eq!(a, b);
    // True history kept: the fill time and step were not reset.
    assert_eq!(a.held[0].fill_ms, b.held[0].fill_ms);
    assert_eq!(a.held[0].step, b.held[0].step);
}

#[test]
fn a_restored_position_is_still_managed_by_the_model_and_still_closes_only_through_a_fill() {
    let hp = held_path("b");
    let mut r = rig(|_| HOLD);
    r.e.model_held_attach(&hp);
    r.advance(100_000);
    assert!(r.e.model_held_persist_now());
    let (fill_ms, clock) = (r.e.model_held_ledger().held[0].fill_ms, r.clock);
    drop(r);

    let calls = Arc::new(AtomicUsize::new(0));
    let mut c = cfg();
    c.floor_fraction_bps = 2_500;
    let mut e2 = Engine::new(c, RunMode::Paper);
    e2.enable_paper_model(Script {
        prompts: Arc::new(Mutex::new(Vec::new())),
        calls: Arc::clone(&calls),
        answer: |_| EXIT,
    });
    e2.model_held_attach(&hp);
    e2.model_held_restore().unwrap().unwrap();
    // Restore rebuilds the BOOKS; the prompt's launch/flow history is not in the ledger. Until history
    // recovery supplies it, management REFUSES by name instead of inventing it.
    e2.tick(AppEvent::Tick);
    let deg = e2.model_held_degraded();
    assert_eq!(
        deg.len(),
        1,
        "a restored position with no recovered history is DEGRADED, by name"
    );
    assert!(deg[0].management_ready.is_err());
    // History recovery: replay the captured launch + flow for the held mint (the same feed a restart
    // re-reads), then the live curve resumes.
    for ev in &events(40) {
        e2.tick(*ev);
    }
    for _ in 0..8 {
        e2.tick(AppEvent::Tick);
    }
    let mut rr = Rig {
        e: e2,
        prompts: Arc::new(Mutex::new(Vec::new())),
        calls,
        clock,
        slot: 2_600,
        n: 500,
    };
    // The curve plane must be fresh again before management can ask (the stale-reserve gate stays).
    curve(&mut rr.e, rr.clock + 1_000, 2_600, 200_000_000);
    rr.clock += 1_000;
    rr.advance_to_order(180_000);
    let (id, kind, _, _) =
        rr.e.model_mgmt_pending(&MINT)
            .unwrap_or_else(|| panic!("{:?}", rr.e.model_lane_report()));
    assert_eq!(format!("{kind:?}"), "Exit");
    assert!(
        rr.e.model_position_open(&MINT),
        "an instruction is not a fill"
    );
    assert!(
        rr.clock - fill_ms > 60_000,
        "the position kept its TRUE age through the restart"
    );
    rr.landing(250_000_000);
    assert!(
        !rr.e.model_position_open(&MINT),
        "{:?}",
        rr.e.model_lane_report()
    );
    assert!(rr
        .e
        .model_mgmt_fills()
        .iter()
        .any(|f| f.order_id == id && f.closed));
}

#[test]
fn pending_orders_restore_as_uncertain_and_only_a_reconciled_report_resolves_them() {
    let hp = held_path("c");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r.e.model_held_attach(&hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    assert!(r.e.model_held_persist_now());
    drop(r);

    let mut e2 = fresh_engine(2_000_000_000, &hp);
    let rep = e2.model_held_restore().unwrap().unwrap();
    assert_eq!(rep.pending_uncertain, 1);
    let (id2, kind, int2, filled) = e2.model_mgmt_pending(&MINT).expect("order restored");
    assert_eq!((id2, int2, filled), (id, intended, 0));
    assert_eq!(format!("{kind:?}"), "Reduce");
    let a = e2.model_stop_assessment();
    assert!(
        a.uncertain_orders >= 1 && !a.is_flat_and_reconciled(),
        "unresolved stays unresolved: {a:?}"
    );
    assert!(e2.model_safety_rearm("alon").is_err() || !e2.model_safety_blocked());
    // The simulator must not settle an order whose acknowledgement died with the old process.
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    e2.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .unwrap();
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        Some(inv0 - intended),
        "exactly the filled quantity"
    );
    assert!(e2.model_mgmt_pending(&MINT).is_none());
    // A duplicate report cannot repeat it.
    assert!(e2
        .model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .is_err());
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 - intended));
}

#[test]
fn restore_refuses_the_whole_file_by_name_and_applies_nothing() {
    let hp = held_path("d");
    let mut r = rig(|_| HOLD);
    r.e.model_held_attach(&hp);
    assert!(r.e.model_held_persist_now());
    drop(r);

    // (1) Different seed bankroll: the books were kept under another wallet.
    let mut e = fresh_engine(1_500_000_000, &hp);
    let err = e.model_held_restore().unwrap_err();
    assert!(format!("{err:?}").contains("SeedMismatch"), "{err:?}");
    assert!(!e.model_position_open(&MINT), "nothing applied");
    assert_eq!(e.model_accounting_view(&MINT).committed, 0);

    // (2) A corrupted file is untrusted, never repaired.
    let raw = std::fs::read_to_string(&hp).unwrap();
    std::fs::write(&hp, raw.replace("\"entry_spend\"", "\"entry_spendX\"")).unwrap();
    let mut e = fresh_engine(2_000_000_000, &hp);
    assert!(matches!(
        e.model_held_restore(),
        Err(pump_quant_app::engine::model_restore::RestoreOutcomeError::Untrusted(_))
    ));
    assert!(!e.model_position_open(&MINT));
    std::fs::write(&hp, "{ not json").unwrap();
    let mut e = fresh_engine(2_000_000_000, &hp);
    assert!(e.model_held_restore().is_err());

    // (3) A second restore on an engine that already restored is refused (never double-applied).
    std::fs::write(&hp, raw).unwrap();
    let mut e = fresh_engine(2_000_000_000, &hp);
    e.model_held_restore().unwrap().unwrap();
    let again = e.model_held_restore().unwrap_err();
    assert!(format!("{again:?}").contains("EngineNotFresh"), "{again:?}");
    assert_eq!(
        e.model_accounting_view(&MINT).committed,
        e.model_accounting_view(&MINT)
            .attribution_entry_spend
            .unwrap()
    );

    // (4) No file at all is a clean start, not an error.
    let mut e = fresh_engine(2_000_000_000, &held_path("nofile"));
    assert!(e.model_held_restore().unwrap().is_none());
}

#[test]
fn an_unpersistable_held_state_trips_safety_off_and_a_flat_engine_writes_nothing_spurious() {
    // Exposure exists but the ledger cannot be written: fail closed.
    let mut r = rig(|_| HOLD);
    r.e.model_held_attach(std::path::Path::new("/nonexistent_dir_pq/held.json"));
    r.advance(20_000);
    assert!(r.e.model_held_persist_failures() >= 1);
    assert!(
        r.e.model_safety_blocked(),
        "unrecoverable exposure => entries blocked"
    );
    assert_eq!(
        r.rep("safety:tripped:held_state_unpersistable"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
    // Held positions are still monitored and managed (REDUCE/EXIT unaffected).
    assert!(r.e.model_position_open(&MINT));
}

#[test]
fn the_ledger_follows_the_exposure_through_reduce_and_exit_and_a_restart_after_the_exit_restores_flat(
) {
    let hp = held_path("e");
    let mut r = rig(|step| {
        if step == 0 {
            REDUCE
        } else if step == 1 {
            EXIT
        } else {
            HOLD
        }
    });
    r.e.model_held_attach(&hp);
    r.advance_to_order(120_000);
    r.landing(250_000_000);
    r.advance(120_000);
    // Persist happens on change during ticks; one more now covers the last quiet interval.
    assert!(r.e.model_held_persist_now());
    let l = r.e.model_held_ledger();
    let closed = !r.e.model_position_open(&MINT);
    let realized = r.e.model_accounting_view(&MINT).realized;
    drop(r);
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    let rep = e2.model_held_restore().unwrap().unwrap();
    assert_eq!(rep.positions, l.held.len());
    assert_eq!(
        e2.model_accounting_view(&MINT).realized,
        realized,
        "realized survives the restart"
    );
    if closed {
        assert!(
            !e2.model_position_open(&MINT),
            "an exited position is not resurrected"
        );
        assert!(e2.model_stop_assessment().is_flat_and_reconciled());
    } else {
        assert!(e2.model_position_open(&MINT));
    }
}

// ===================== LEGACY AUTHORITY CANNOT REGAIN THE MODEL PATH =====================

/// Every legacy discretionary-exit / sizing knob pushed to its most aggressive value. If ANY of them could
/// still act on a model-managed position, this configuration would close it on the first ordinary print.
fn aggressive_legacy(c: &mut Config) {
    c.lc_tp1_bps = 10_050; // +0.5%
    c.lc_tp1_frac_bps = 10_000; // sell everything
    c.lc_tp2_bps = 10_100;
    c.lc_tp3_bps = 10_150;
    c.lc_trail_base_bps = 1; // 0.01% trail
    c.lc_trail_max_bps = 1;
    c.lc_stall_ticks = 1;
    c.lc_max_hold_ticks = 1;
    c.lc_cvd_hold_frac_bps = 10_000;
    c.thesis_persist_obs = 1;
    c.derived_targets_enable = true;
    c.into_strength_exit_enable = true;
    c.vol_stop_enable = true;
    c.alpha_exit_pressure_enable = true;
    c.entry_mode_leaves_enable = true;
    c.probe_budget_enable = true;
    c.setup_classifier_enable = true;
    c.brain_enable = true;
    c.brain_haircut_enable = true;
    c.scale_confirm_auth_min_bp = 0; // any flow "confirms" a legacy scale-in
}

#[test]
fn no_legacy_config_value_can_close_or_resize_a_model_managed_position() {
    let mut r = rig_cfg(2_000_000_000, 2_500, |_| HOLD, aggressive_legacy);
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let basis0 = r.e.model_accounting_view(&MINT).remaining_cost_basis;
    // Ordinary, mildly rising prints for a long while: any ladder/trail/stall/time/thesis/into-strength
    // rule that still had authority would fire here (TP1 is +0.5% and sells 100%; max-hold is 1 tick).
    r.advance(240_000);
    for k in 0..40 {
        r.clock += 1_000;
        r.slot += 1;
        r.n += 1;
        curve(&mut r.e, r.clock, r.slot, 200_000_000 + 2_000_000 * (k + 1));
        print(&mut r.e, r.n, r.clock, r.slot);
        ticks(&mut r.e, 2);
    }
    assert!(
        r.e.model_position_open(&MINT),
        "no legacy rule may close it: {:?}",
        r.e.model_lane_report()
    );
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0),
        "no legacy rule may resize it (no ladder sell, no scale-in)"
    );
    assert_eq!(
        r.e.model_accounting_view(&MINT).remaining_cost_basis,
        basis0
    );
    assert!(
        r.e.model_mgmt_fills().is_empty(),
        "the only thing that can create a management fill is a model instruction"
    );
    // Control: the SAME tape with the SAME aggressive config but a LEGACY (unmanaged) lane is not what is
    // under test here - the store-level control lives in position.rs. What this proves: only the model's own
    // verdict (or the two agreed safeguards) moves this position.
    let mut m = rig_cfg(
        2_000_000_000,
        2_500,
        |step| if step == 0 { EXIT } else { HOLD },
        aggressive_legacy,
    );
    m.advance_to_order(120_000);
    m.landing(250_000_000);
    assert!(
        !m.e.model_position_open(&MINT),
        "the model's own EXIT still closes it"
    );
    assert!(m.e.model_mgmt_fills().iter().any(|f| f.closed));
}

// ---------------------------------------------------------------------------------------------------------------
// MFE/MAE ("max favourable / adverse so far") tracker: causal, once-only, and restart-safe.
//
// Time basis: `fill_ms` is the FEED clock at the moment the paper fill was applied (the newest receive time seen),
// and a print's own time is its wire `recv_unix_ms`. Both are on the same wire clock. A print with
// recv < fill_ms was received before the position existed; a print with recv >= fill_ms was not. Equal time is
// NOT distinguishable from timestamps alone, so it is treated as "after the fill" (the conservative side for a
// held position: an equal-time adverse print is counted, never silently dropped).
// ---------------------------------------------------------------------------------------------------------------

fn held_extrema(e: &Engine) -> (u64, u64, u64, i64) {
    let l = e.model_held_ledger();
    let h = &l.held[0];
    (h.entry_price_fp, h.peak_fp, h.trough_fp, h.fill_ms)
}

fn print_at(e: &mut Engine, i: u32, ts: i64, slot: u64, price: i128) {
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: price,
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

#[test]
fn excursions_ignore_pre_fill_prints_count_post_fill_ones_once_and_the_equal_time_print() {
    let mut r = rig(|_| HOLD);
    let (entry, peak0, trough0, fill_ms) = held_extrema(&r.e);
    assert_eq!(
        peak0, entry,
        "a fresh position's extrema start at its entry price"
    );
    assert_eq!(trough0, entry);
    // Independently calculated expectations (fixed-point price units, no engine arithmetic reused).
    let high = entry + entry / 5; // +20 %
    let low = entry - entry / 10; // -10 %  (well above the 35 % hard stop, so the position stays open)
    let mut i = 5_000u32;
    let mut slot = 9_000u64;
    let mut next = |dt: i64| {
        i += 1;
        slot += 1;
        (i, fill_ms + dt, slot)
    };
    // 1. PRE-FILL prints (an overlap replay re-delivers history): a wildly high and a wildly low price,
    //    received BEFORE the fill. Neither may touch the extrema.
    // The cache refuses out-of-order prints, so deliver them through a fresh restart below as well; here a
    // directly-late print is the in-process analogue.
    let (a, t, s) = next(-1);
    print_at(&mut r.e, a, t, s, i128::from(entry) * 10);
    let (a, t, s) = next(-2);
    print_at(&mut r.e, a, t, s, 1);
    let (_, p, tr, _) = held_extrema(&r.e);
    assert_eq!(
        (p, tr),
        (entry, entry),
        "pre-fill prints must not move the extrema"
    );
    // 2. A genuine POST-fill high and low move them to exactly those values.
    let (a, t, s) = next(1_000);
    print_at(&mut r.e, a, t, s, i128::from(high));
    let (a, t, s) = next(2_000);
    print_at(&mut r.e, a, t, s, i128::from(low));
    let (_, p, tr, _) = held_extrema(&r.e);
    assert_eq!(p, high, "post-fill high");
    assert_eq!(tr, low, "post-fill low");
    // 3. A DUPLICATE delivery (same identity) applies once: re-delivering the high-then-low pair changes nothing,
    //    and a lower-than-peak, higher-than-trough print does not regress either extremum.
    let (a, t, s) = (5_003u32, fill_ms + 3_000, 9_050u64);
    print_at(&mut r.e, a, t, s, i128::from(entry));
    let (_, p, tr, _) = held_extrema(&r.e);
    assert_eq!(
        (p, tr),
        (high, low),
        "an in-range print must not regress either extremum"
    );
}

#[test]
fn an_equal_time_print_is_counted_as_after_the_fill_and_a_one_ms_earlier_print_is_not() {
    let r = rig(|_| HOLD);
    let mut e = r.e;
    let (entry, _, _, fill_ms) = held_extrema(&e);
    let low = entry - entry / 20; // -5 %, independently computed
                                  // One millisecond BEFORE the fill: ignored.
    print_at(&mut e, 7_001, fill_ms - 1, 9_101, i128::from(low));
    assert_eq!(
        held_extrema(&e).2,
        entry,
        "recv = fill_ms - 1 is before the fill"
    );
    // Exactly AT the fill millisecond: counted (documented conservative boundary).
    print_at(&mut e, 7_002, fill_ms, 9_102, i128::from(low));
    assert_eq!(
        held_extrema(&e).2,
        low,
        "recv = fill_ms is treated as after the fill"
    );
}

#[test]
fn restored_extrema_survive_an_overlap_replay_which_applies_each_new_observation_once() {
    let hp = held_path("exc");
    let mut r = rig(|_| HOLD);
    r.e.model_held_attach(&hp);
    let (entry, _, _, fill_ms) = held_extrema(&r.e);
    let high = entry + entry / 5;
    let low = entry - entry / 10;
    print_at(&mut r.e, 6_001, fill_ms + 1_000, 9_201, i128::from(high));
    print_at(&mut r.e, 6_002, fill_ms + 2_000, 9_202, i128::from(low));
    ticks(&mut r.e, 2);
    assert!(r.e.model_held_persist_now());
    let led = r.e.model_held_ledger();
    assert_eq!((led.held[0].peak_fp, led.held[0].trough_fp), (high, low));
    drop(r);

    let mut e2 = fresh_engine(2_000_000_000, &hp);
    e2.model_held_restore().expect("restore").expect("ledger");
    assert_eq!(
        held_extrema(&e2),
        (entry, high, low, fill_ms),
        "restored extrema and fill time are the ones written, not reset"
    );
    // OVERLAP REPLAY: the whole pre-fill history is delivered again (extreme prices, all before the fill), then the
    // two post-fill prints again, then one genuinely NEW post-fill observation.
    for k in 0..20u32 {
        let px = if k % 2 == 0 {
            i128::from(entry) * 10
        } else {
            1
        };
        print_at(
            &mut e2,
            100 + k,
            fill_ms - 20_000 + i64::from(k) * 500,
            9_300 + u64::from(k),
            px,
        );
    }
    print_at(&mut e2, 6_001, fill_ms + 1_000, 9_201, i128::from(high));
    print_at(&mut e2, 6_002, fill_ms + 2_000, 9_202, i128::from(low));
    let (_, p, t, _) = held_extrema(&e2);
    assert_eq!((p, t), (high, low), "replay changed nothing it should not");
    let new_low = low - entry / 20; // a genuinely new, lower post-fill price (-15 %, above the hard stop)
    print_at(&mut e2, 6_003, fill_ms + 4_000, 9_203, i128::from(new_low));
    let (_, p, t, _) = held_extrema(&e2);
    assert_eq!(p, high, "peak preserved");
    assert_eq!(t, new_low, "the genuinely new low applies exactly once");
    assert!(
        e2.model_position_open(&MINT),
        "no spurious exit from replayed history"
    );
}

fn mark_of(prompt: &str) -> f64 {
    let line = prompt
        .lines()
        .find(|l| {
            l.trim_start()
                .starts_with("mark price (lamports per raw token):")
        })
        .expect("a management prompt shows its mark");
    line.rsplit(':')
        .next()
        .unwrap()
        .trim()
        .parse()
        .expect("mark parses")
}

#[test]
fn a_post_fill_replay_cannot_regress_the_mark_even_when_it_leaves_the_extrema_alone() {
    let mut r = rig(|_| HOLD);
    let (entry, _, _, _) = held_extrema(&r.e);
    // Newest print: +2 % of entry, inside the extrema range, so replay can matter ONLY through the mark.
    let last_px = entry + entry / 50;
    r.advance(62_000);
    let t_last = r.clock + 1_000;
    print_at(&mut r.e, 8_001, t_last, 9_401, i128::from(last_px));
    let extrema = held_extrema(&r.e);
    // Replay AFTER it: an older in-range post-fill print, and a duplicate identity of the newest carrying another price.
    print_at(&mut r.e, 8_002, t_last - 500, 9_402, i128::from(entry) + 3);
    print_at(&mut r.e, 8_001, t_last, 9_401, i128::from(entry) + 5);
    assert_eq!(held_extrema(&r.e), extrema, "replay left the extrema alone");
    // Let the management cadence come due with NO newer print: curve observations advance the feed clock only.
    let before = r.prompts.lock().unwrap().len();
    let mut t = t_last;
    for k in 0..8u64 {
        t += 5_000;
        curve_quiet(&mut r.e, t, 9_500 + k);
        ticks(&mut r.e, 3);
    }
    let ps = r.prompts.lock().unwrap().clone();
    assert!(
        ps.len() > before,
        "a management prompt must be asked after the replay"
    );
    let got = mark_of(ps.last().unwrap());
    let want = last_px as f64 / 1e9; // independent: fixed-point / PRICE_SCALE
    assert!(
        ((got - want) / want).abs() < 1e-9,
        "mark {got} must be the newest accepted print {want}, not a replayed older/duplicate price"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Settled-order identity and terminal evidence across a restart (no financial effect is replayed).
// ---------------------------------------------------------------------------------------------------------------
use pump_quant_app::engine::model_admit::{
    Evidence, EvidenceResult, FillReport, OrderState, ReconcileOutcome,
};

fn filled_report() -> ReconcileOutcome {
    ReconcileOutcome::Filled(FillReport {
        entry_price_fp: 22_000,
        reserve_sol_lamports: VSOL,
    })
}

/// Entry filled, then closed through an EXIT fill; first terminal evidence (Filled) applied; ledger written.
fn closed_order_world(tag: &str) -> (std::path::PathBuf, u64, u64, i128, u64) {
    let hp = held_path(tag);
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD });
    r.e.model_held_attach(&hp);
    let id = r.e.model_position_order_id(&MINT).expect("entry order id");
    let q = r.e.model_order_rec(id).unwrap().clip_lamports;
    r.advance_to_order(120_000);
    r.landing(250_000_000);
    assert!(!r.e.model_position_open(&MINT));
    assert_eq!(r.e.model_order_rec(id).unwrap().state, OrderState::Closed);
    let first = r.e.model_ingest_evidence(Evidence {
        order_id: id,
        attempt: 1,
        clip_lamports: q,
        outcome: filled_report(),
    });
    assert_eq!(first, EvidenceResult::Applied);
    assert!(r.e.model_held_persist_now());
    let realized = r.e.model_accounting_view(&MINT).realized;
    let seq = r.e.model_held_ledger().model_order_seq;
    (hp, id, q, realized, seq)
}

#[test]
fn a_closed_order_keeps_its_identity_and_terminal_evidence_across_a_restart_without_replaying_money(
) {
    let (hp, id, q, realized, seq) = closed_order_world("ord_a");
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    e2.model_held_restore().unwrap().unwrap();
    let rec = e2
        .model_order_rec(id)
        .expect("settled order restored as a record");
    assert_eq!(rec.state, OrderState::Closed);
    assert_eq!(rec.clip_lamports, q);
    assert_eq!(rec.terminal, Some(filled_report()));
    // Money came from the ledger totals, never from replaying the order.
    let v = e2.model_accounting_view(&MINT);
    assert_eq!(v.realized, realized);
    assert_eq!(v.committed, 0);
    assert!(
        e2.model_all_fills().is_empty(),
        "no fill record was re-created"
    );
    assert!(e2.model_mgmt_fills().is_empty());
    assert_eq!(
        e2.model_held_ledger().model_order_seq,
        seq,
        "ids never repeat"
    );

    // (1) IDENTICAL late report -> duplicate, nothing changes.
    let before = e2.model_held_ledger();
    assert_eq!(
        e2.model_ingest_evidence(Evidence {
            order_id: id,
            attempt: 1,
            clip_lamports: q,
            outcome: filled_report()
        }),
        EvidenceResult::Duplicate
    );
    let mut after = e2.model_held_ledger();
    let (mut b, a) = (before, &mut after);
    b.written_wall_ms = 0;
    a.written_wall_ms = 0;
    assert_eq!(&b, a, "a duplicate changes nothing");
    assert_eq!(e2.model_accounting_view(&MINT).realized, realized);
    assert!(e2.model_recon_faults().is_empty());

    // (2) CONFLICTING report -> evidence preserved, the established fault raised, books untouched.
    assert_eq!(
        e2.model_ingest_evidence(Evidence {
            order_id: id,
            attempt: 1,
            clip_lamports: q,
            outcome: ReconcileOutcome::NotFilled
        }),
        EvidenceResult::Fault
    );
    let f = e2.model_recon_faults().get(&id).expect("fault");
    assert_eq!(
        f.first,
        Some(filled_report()),
        "first terminal evidence kept verbatim"
    );
    assert_eq!(f.contradicting, vec![ReconcileOutcome::NotFilled]);
    assert_eq!(
        e2.model_accounting_view(&MINT).realized,
        realized,
        "no second credit or debit"
    );
    assert!(
        e2.model_mint_is_blocked(&MINT),
        "new exposure on the faulted mint is blocked"
    );

    // (3) A genuinely unknown id (never issued) stays unresolved and is not applied.
    let unknown = seq + 50;
    assert_eq!(
        e2.model_ingest_evidence(Evidence {
            order_id: unknown,
            attempt: 1,
            clip_lamports: q,
            outcome: filled_report()
        }),
        EvidenceResult::Rejected("unknown_order")
    );
    assert!(e2.model_order_rec(unknown).is_none());
    assert_eq!(e2.model_accounting_view(&MINT).realized, realized);
}

#[test]
fn an_unresolved_fault_survives_a_second_restart_and_still_blocks_the_mint() {
    let (hp, id, q, _realized, _seq) = closed_order_world("ord_b");
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    e2.model_held_restore().unwrap().unwrap();
    assert_eq!(
        e2.model_ingest_evidence(Evidence {
            order_id: id,
            attempt: 1,
            clip_lamports: q,
            outcome: ReconcileOutcome::NotFilled
        }),
        EvidenceResult::Fault
    );
    assert!(e2.model_held_persist_now());
    let mut e3 = fresh_engine(2_000_000_000, &hp);
    e3.model_held_restore().unwrap().unwrap();
    let f = e3.model_recon_faults().get(&id).expect("fault restored");
    assert_eq!(f.first, Some(filled_report()));
    assert_eq!(f.contradicting, vec![ReconcileOutcome::NotFilled]);
    assert!(
        e3.model_mint_is_blocked(&MINT),
        "the block survives the restart"
    );
}

#[test]
fn compaction_names_a_dropped_order_and_never_drops_a_faulted_one() {
    let (hp, id, q, realized, _seq) = closed_order_world("ord_c");
    let ev = |outcome| Evidence {
        order_id: id,
        attempt: 1,
        clip_lamports: q,
        outcome,
    };
    // (a) Unfaulted settled record + cap 0: compacted on the next tick, floor raised, and a late report is
    // NAMED compacted (unresolved, never applied) -- not a plain stranger and not silently harmless.
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    e2.model_held_restore().unwrap().unwrap();
    e2.model_set_settled_order_cap(0);
    e2.tick(AppEvent::Tick);
    assert!(e2.model_order_rec(id).is_none(), "record compacted");
    assert_eq!(
        e2.model_held_ledger().order_floor,
        id,
        "floor = highest dropped id"
    );
    assert_eq!(
        e2.model_ingest_evidence(ev(ReconcileOutcome::NotFilled)),
        EvidenceResult::Rejected("compacted_order")
    );
    assert_eq!(e2.model_accounting_view(&MINT).realized, realized);
    assert!(e2.model_held_persist_now());
    // The floor survives another restart, so the name does too.
    let mut e3 = fresh_engine(2_000_000_000, &hp);
    e3.model_held_restore().unwrap().unwrap();
    assert_eq!(
        e3.model_ingest_evidence(ev(ReconcileOutcome::Filled(FillReport {
            entry_price_fp: 1,
            reserve_sol_lamports: 1
        }))),
        EvidenceResult::Rejected("compacted_order")
    );

    // (b) The same order with an UNRESOLVED FAULT is never compacted, whatever the cap.
    let (hp2, id2, q2, _r2, _s2) = closed_order_world("ord_d");
    let mut f = fresh_engine(2_000_000_000, &hp2);
    f.model_held_restore().unwrap().unwrap();
    let ev2 = |outcome| Evidence {
        order_id: id2,
        attempt: 1,
        clip_lamports: q2,
        outcome,
    };
    assert_eq!(
        f.model_ingest_evidence(ev2(ReconcileOutcome::NotFilled)),
        EvidenceResult::Fault
    );
    f.model_set_settled_order_cap(0);
    f.tick(AppEvent::Tick);
    assert!(
        f.model_order_rec(id2).is_some(),
        "a faulted order is retained"
    );
    assert_eq!(f.model_held_ledger().order_floor, 0);
    assert_eq!(
        f.model_ingest_evidence(ev2(ReconcileOutcome::NotFilled)),
        EvidenceResult::Fault
    );
    assert!(f.model_mint_is_blocked(&MINT));
}

/// A world stopped with the ENTRY order accepted but not yet filled (no landing state has arrived).
fn pending_entry_world(tag: &str) -> (std::path::PathBuf, u64, u64) {
    let hp = held_path(tag);
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut c = cfg();
    c.bankroll_initial_lamports = 2_000_000_000;
    c.floor_fraction_bps = 2_500;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Script {
        prompts,
        calls,
        answer: |_| HOLD,
    });
    e.model_held_attach(&hp);
    for ev in &events(40) {
        e.tick(*ev);
    }
    ticks(&mut e, 8);
    assert!(
        !e.model_position_open(&MINT),
        "setup: no fill yet: {:?}",
        e.model_lane_report()
    );
    let (id, _attempt, q) = e
        .model_pending_order(&MINT)
        .expect("setup: the BUY verdict made a pending entry order");
    assert!(e.model_held_persist_now());
    (hp, id, q)
}

fn landing_after(e: &mut Engine, dsol: u64) {
    let t = T0 + 1_000 + 40 * 2_000 + 1_500;
    curve(e, t, 2_100, dsol);
}

#[test]
fn an_uncertain_entry_order_is_restored_unbooked_then_filled_exactly_once() {
    let (hp, id, q) = pending_entry_world("unc_a");
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    let rep = e2.model_held_restore().unwrap().unwrap();
    assert_eq!(rep.pending_uncertain, 1);
    // Restored, not resubmitted, not booked.
    assert_eq!(
        e2.model_pending_order(&MINT).map(|p| (p.0, p.2)),
        Some((id, q))
    );
    assert!(!e2.model_position_open(&MINT));
    let v = e2.model_accounting_view(&MINT);
    assert_eq!((v.realized, v.committed), (0, 0));
    assert_eq!(
        e2.model_order_rec(id).unwrap().state,
        OrderState::PendingUncertain
    );
    // The simulator must not settle an order whose acknowledgement died with the old process.
    landing_after(&mut e2, 200_000_000);
    assert!(
        !e2.model_position_open(&MINT),
        "uncertain: never auto-filled"
    );
    assert_eq!(e2.model_pending_orders(), 1);
    // Reconcile FILLED through the normal path.
    let ev = Evidence {
        order_id: id,
        attempt: 1,
        clip_lamports: q,
        outcome: filled_report(),
    };
    assert_eq!(e2.model_ingest_evidence(ev), EvidenceResult::Applied);
    landing_after(&mut e2, 210_000_000);
    assert!(
        e2.model_position_open(&MINT),
        "{:?}",
        e2.model_lane_report()
    );
    assert_eq!(e2.model_pending_orders(), 0);
    let once = e2.model_accounting_view(&MINT);
    assert_eq!(once.committed, once.attribution_entry_spend.unwrap());
    let fills = e2.model_all_fills().len();
    assert_eq!(fills, 1);
    // Repeats: exactly-once.
    for _ in 0..3 {
        assert_eq!(e2.model_ingest_evidence(ev), EvidenceResult::Duplicate);
        landing_after(&mut e2, 220_000_000);
    }
    assert_eq!(e2.model_all_fills().len(), 1, "no second fill");
    let again = e2.model_accounting_view(&MINT);
    assert_eq!(
        (again.committed, again.balance, again.realized),
        (once.committed, once.balance, once.realized)
    );
}

#[test]
fn an_uncertain_entry_order_reconciled_not_filled_is_cleared_once_and_never_filled_later() {
    let (hp, id, q) = pending_entry_world("unc_b");
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    e2.model_held_restore().unwrap().unwrap();
    let ev = Evidence {
        order_id: id,
        attempt: 1,
        clip_lamports: q,
        outcome: ReconcileOutcome::NotFilled,
    };
    assert_eq!(e2.model_ingest_evidence(ev), EvidenceResult::Applied);
    assert_eq!(e2.model_pending_orders(), 0);
    assert!(!e2.model_position_open(&MINT));
    let v = e2.model_accounting_view(&MINT);
    assert_eq!((v.committed, v.realized), (0, 0));
    // Repeated report: duplicate. A later contradicting FILLED report: fault, not applied.
    assert_eq!(e2.model_ingest_evidence(ev), EvidenceResult::Duplicate);
    landing_after(&mut e2, 200_000_000);
    assert!(
        !e2.model_position_open(&MINT),
        "a cleared order never fills"
    );
    let fill = Evidence {
        outcome: filled_report(),
        ..ev
    };
    assert_eq!(e2.model_ingest_evidence(fill), EvidenceResult::Fault);
    assert!(!e2.model_position_open(&MINT));
    assert!(e2.model_mint_is_blocked(&MINT));
    assert_eq!(e2.model_accounting_view(&MINT).committed, 0);
}
