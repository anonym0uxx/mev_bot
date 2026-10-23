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
    let mut ev = vec![
        AppEvent::LaunchObserved {
            mint: mint(),
            creator: CREATOR,
            launch_unix_ms: T0,
        },
        // The fixture market is SOL-quoted; the lane requires the positive statement.
        AppEvent::CurveQuote { mint: mint(), quote_mint: [0u8; 32] },
    ];
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
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut c = cfg();
    c.bankroll_initial_lamports = bankroll;
    c.floor_fraction_bps = floor_bps;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Script {
        prompts: Arc::clone(&prompts),
        calls: Arc::clone(&calls),
        answer,
    });
    let evs = events(40);
    for ev in &evs {
        e.tick(ev.clone());
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
    assert!(r.rep("mgmt:dispatched") >= 1, "{:?}", r.e.model_lane_report());
    let p = r.prompts.lock().unwrap().clone();
    let first = p.first().expect("a management prompt was rendered");
    assert!(first.starts_with("Decide the next action for a position you already hold."));
    assert!(first.contains("\nSTEP: 0\n"));
    assert!(first.contains("POSITION STATE (at the decision instant, before any action):"));
    assert!(first.contains("  inventory: "));
    // The mint is the base58 address, as in the corpus, never hex.
    assert!(!first.contains(&"ab".repeat(32)));
    assert!(first.contains("holding time: 6"), "held seconds are real: {first}");
}

#[test]
fn reduce_sells_half_the_inventory_through_a_fill_and_only_the_fill_changes_inventory() {
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    let inv0 = r.e.model_inventory_tokens(&MINT).expect("fill established inventory");
    assert!(inv0 > 0);
    r.advance_to_order(120_000);
    // The verdict created an ORDER INTENT; inventory is untouched until a landing state fills it.
    if let Some((_, _, intended, filled)) = r.e.model_mgmt_pending(&MINT) {
        assert_eq!(filled, 0);
        assert_eq!(intended, inv0 / 2, "half of the inventory held at order time, floored");
        assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0), "intent is not a fill");
    }
    r.landing(250_000_000);
    r.landing(260_000_000);
    let fills = r.e.model_mgmt_fills().to_vec();
    assert_eq!(fills.len(), 1, "exactly one fill: {:?}", r.e.model_lane_report());
    assert_eq!(fills[0].tokens, inv0 / 2);
    assert!(!fills[0].closed);
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0 - inv0 / 2),
        "inventory reduced by exactly the filled quantity"
    );
    assert!(r.e.model_position_open(&MINT), "a REDUCE keeps the position open");
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
}

#[test]
fn exit_closes_through_the_fill_and_a_late_duplicate_cannot_repeat_it() {
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD });
    r.advance_to_order(120_000);
    // One landing state fills the EXIT. (A second would let the always-BUY entry stub legitimately
    // re-enter the mint, which is the entry lane's business, not management's.)
    r.landing(250_000_000);
    assert!(!r.e.model_position_open(&MINT), "{:?}", r.e.model_lane_report());
    assert_eq!(r.e.model_mgmt_fills().len(), 1);
    assert!(r.e.model_mgmt_fills()[0].closed);
    // Duplicate / late reconciled report for the same order: refused, nothing booked twice.
    let id = r.e.model_mgmt_fills()[0].order_id;
    assert!(r.e.model_mgmt_apply_reconciled_fill(MINT, id, 1, 22_000).is_err());
    assert_eq!(r.e.model_mgmt_fills().len(), 1);
}

#[test]
fn add_targets_half_the_reconciled_inventory_and_only_the_fill_changes_state() {
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let cash0 = r.e.model_free_cash_lamports();
    r.advance_to_order(120_000);
    let (id, kind, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("ADD order pending");
    assert_eq!(format!("{kind:?}"), "Add");
    assert_eq!(intended, inv0 / 2, "target = floor(50% of reconciled inventory), NOT account capital");
    assert_eq!(filled, 0);
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0), "an intent is not a fill");
    assert!(r.e.model_free_cash_lamports() <= cash0, "the reservation can only reduce free cash");
    r.landing(250_000_000);
    r.landing(260_000_000);
    let fills = r.e.model_mgmt_fills().to_vec();
    assert_eq!(fills.len(), 1, "{:?}", r.e.model_lane_report());
    assert!(fills[0].is_add);
    assert!(fills[0].tokens >= intended, "minimal notional that delivers at least the target");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 + fills[0].tokens));
    assert_eq!(fills[0].cost_lamports, fills[0].spent_lamports
        + fills[0].spent_lamports * u64::from(pump_quant_app::cost_model::venue_fee_bps_per_leg(VSOL + 260_000_000)) / 10_000
        + pump_quant_app::cost_model::FIXED_LAMPORTS_PER_LEG, "all-in cost = notional + landing-venue fee + fixed leg");
    assert_eq!(r.e.model_free_cash_lamports(), cash0.saturating_sub(fills[0].cost_lamports),
        "cash fell by exactly the all-in cost of the fill");
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
        r.e.model_mgmt_fills().first().map(|f| (f.tokens, f.spent_lamports))
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
    assert!(r.e.model_mgmt_pending(&MINT).is_none(), "risk-increasing ADD is invalidated");
    assert!(r.rep("safety:add_order_invalidated") >= 1);
    assert!(r.e.model_mgmt_fills().is_empty(), "nothing was booked");
    // REDUCE under a trip is still permitted and still fills.
    let mut r2 = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r2.advance_to_order(120_000);
    r2.e.model_safety_trip_operator();
    assert!(r2.e.model_mgmt_pending(&MINT).is_some(), "REDUCE survives the trip");
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
    assert!(r.rep("mgmt:refuse:add_blocked_safety_off") >= 1, "{:?}", r.e.model_lane_report());
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
    assert!(r.e.model_mgmt_pending(&MINT).is_some(), "unresolved stays pending");
    assert!(r.e.model_mgmt_fills().is_empty());
    assert!(r.e.model_position_open(&MINT));
    assert!(r.e.model_safety_rearm("alon").is_err(), "re-arm refused with an uncertain order");
    let a = r.e.model_stop_assessment();
    assert!(a.uncertain_orders >= 1 && !a.is_flat_and_reconciled());
    // Only a reconciled report resolves it: exactly the filled quantity, nothing more.
    let (_, _, intended, _) = r.e.model_mgmt_pending(&MINT).unwrap();
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000).unwrap();
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
    r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, part, 1_000_000).unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 + part));
    let (_, _, i2, f2) = r.e.model_mgmt_pending(&MINT).expect("remainder still pending");
    assert_eq!((i2, f2), (intended, part));
    // an over-sized report and a SELL-shaped report for an ADD order are refused
    assert!(r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, intended, 1_000_000).is_err());
    assert!(r.e.model_mgmt_apply_reconciled_fill(MINT, id, 1, 22_000).is_err(), "wrong kind");
    // the remainder completes it; a further report finds nothing pending
    r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, intended - part, 1_000_000).unwrap();
    assert!(r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, 1, 1_000_000).is_err());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 + intended));
}

#[test]
fn partial_fill_keeps_the_remainder_pending_and_monitored() {
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD });
    r.advance_to_order(120_000);
    let (id, _, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("EXIT order pending");
    assert_eq!(filled, 0);
    let part = intended / 3;
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000).unwrap();
    let (_, _, intended2, filled2) = r.e.model_mgmt_pending(&MINT).expect("remainder still pending");
    assert_eq!((intended2, filled2), (intended, part));
    assert!(r.e.model_position_open(&MINT), "a partial fill leaves the position open and monitored");
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
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended / 2, 22_000).unwrap();
    assert!(r.e.model_inventory_tokens(&MINT).is_some());
    assert_eq!(r.rep("mgmt:order:reduce"), 1, "one instruction produced one order");
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
    assert_eq!(i128::from(v.balance), (seed + v.realized).clamp(0, i128::from(u64::MAX)), "{tag}: balance == seed + realized: {v:?}");
    if let Some(att) = v.attribution_entry_spend {
        assert_eq!(v.committed, att, "{tag}: committed == the position's attributed cost: {v:?}");
    } else {
        assert_eq!(v.committed, 0, "{tag}: flat => nothing committed: {v:?}");
    }
    assert!(v.free <= v.balance.saturating_sub(v.committed), "{tag}: free never exceeds balance - committed: {v:?}");
}

#[test]
fn wallet_cash_inventory_basis_and_realized_tie_out_through_add_reduce_and_exit() {
    // ADD, then REDUCE, then EXIT, each filled through landing state. After each step the wallet identity
    // holds and the numbers equal hand-derived values.
    let mut r = rig(|step| match step { 0 => ADD, 1 => REDUCE, 2 => EXIT, _ => HOLD });
    assert_wallet_ties(&r, "after entry");
    let v0 = r.e.model_accounting_view(&MINT);
    let inv0 = v0.inventory_tokens.unwrap();
    let basis0 = v0.remaining_cost_basis.unwrap();

    // ---- ADD ----
    r.advance_to_order(120_000);
    r.landing(250_000_000);
    r.landing(260_000_000);
    let add = r.e.model_mgmt_fills().iter().find(|f| f.is_add).copied().expect("ADD filled");
    let v1 = r.e.model_accounting_view(&MINT);
    assert_wallet_ties(&r, "after ADD");
    assert_eq!(v1.inventory_tokens, Some(inv0 + add.tokens), "inventory grew by exactly the filled tokens");
    assert_eq!(v1.realized, v0.realized, "a buy realizes nothing");
    assert_eq!(v1.committed, v0.committed + add.cost_lamports, "committed grew by exactly the all-in ADD cost");
    assert_eq!(v1.free, v0.free - add.cost_lamports, "free cash fell by exactly the all-in cost");
    assert_eq!(v1.remaining_cost_basis, Some(basis0 + add.cost_lamports), "cost basis += all-in ADD cost");

    // ---- REDUCE ---- (the next management ask after the ADD)
    let n_fills = r.e.model_mgmt_fills().len();
    while r.e.model_mgmt_fills().len() == n_fills && r.clock < T0 + 3_000_000 {
        r.advance(10_000);
        r.landing(270_000_000 + (r.clock as u64 % 1_000));
    }
    let red = r.e.model_mgmt_fills().iter().filter(|f| !f.is_add).next().copied();
    assert!(red.is_some(), "the REDUCE leg must actually run, not be skipped: {:?}", r.e.model_lane_report());
    if let Some(red) = red {
        let v2 = r.e.model_accounting_view(&MINT);
        assert_wallet_ties(&r, "after REDUCE");
        assert_eq!(v2.inventory_tokens, Some(v1.inventory_tokens.unwrap() - red.tokens), "inventory fell by exactly the filled tokens");
        assert_eq!(v2.realized, v1.realized + red.net_lamports, "realized changed by exactly the fill's net");
        // Released cost = the sold share of committed cost (floor): committed + released == before.
        let released = v1.committed - v2.committed;
        assert_eq!(released, (u128::from(v1.committed) * u128::from(red.tokens) / u128::from(v1.inventory_tokens.unwrap())) as u64);
        // Proceeds are cash: free rose by released cost + net realized (net already nets cost and fees).
        assert_eq!(i128::from(v2.free), i128::from(v1.free) + i128::from(released) + red.net_lamports, "cash == released basis + realized net");
    }
}

#[test]
fn a_partial_reduce_fill_changes_only_filled_quantity_cash_and_basis() {
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD });
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    let v0 = r.e.model_accounting_view(&MINT);
    let part = intended / 3;
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000).unwrap();
    let v1 = r.e.model_accounting_view(&MINT);
    let f = *r.e.model_mgmt_fills().last().unwrap();
    assert_eq!(f.tokens, part);
    assert_wallet_ties(&r, "after partial");
    assert_eq!(v1.inventory_tokens, Some(v0.inventory_tokens.unwrap() - part));
    assert_eq!(v1.realized, v0.realized + f.net_lamports);
    // The unfilled remainder changed NOTHING: still pending for exactly intended - part.
    let (_, _, i2, f2) = r.e.model_mgmt_pending(&MINT).unwrap();
    assert_eq!((i2, f2), (intended, part));
    // Cost basis: the position store keeps cost * remaining_bps; the released share matches the fraction.
    let frac = u128::from(part) * 10_000 / u128::from(v0.inventory_tokens.unwrap());
    let expect_basis = (u128::from(v0.remaining_cost_basis.unwrap()) * (10_000 - frac) / 10_000) as u64;
    assert!(v1.remaining_cost_basis.unwrap().abs_diff(expect_basis) <= 1, "{:?} vs {expect_basis}", v1.remaining_cost_basis);
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
    assert_eq!((v1.inventory_tokens, v1.committed, v1.realized), (v0.inventory_tokens, v0.committed, v0.realized),
        "an unacknowledged order moves neither inventory, committed capital nor realized cash");
    assert!(r.e.model_mgmt_fills().is_empty());
    assert_eq!(r.e.model_free_cash_lamports(), reserved_free, "its cash stays RESERVED, not spent and not released");
    assert!(r.e.model_stop_assessment().uncertain_orders >= 1);
    // A reconciled report resolves it for exactly what the chain says (here: a partial of the intended size).
    let part = intended / 2;
    r.e.model_mgmt_apply_reconciled_add_fill(MINT, id, part, 1_000_000).unwrap();
    let v2 = r.e.model_accounting_view(&MINT);
    assert_eq!(v2.inventory_tokens, Some(v0.inventory_tokens.unwrap() + part));
    assert_wallet_ties(&r, "after reconciled uncertain ADD");
}

#[test]
fn held_data_readiness_is_measured_and_a_stale_reserve_degrades_while_protection_and_the_bound_stay() {
    let mut r = rig(|_| HOLD);
    // Fresh: the rig's curve observation is current, a prompt can be cut.
    r.advance(10_000);
    let st = r.e.model_held_data_status();
    assert_eq!(st.len(), 1);
    assert!(st[0].reserve_fresh, "{st:?}");
    assert_eq!(st[0].management_ready, Ok(()), "{st:?}");
    assert!(st[0].reserve_age_ms.unwrap() <= 60_000);
    assert!(r.e.model_held_degraded().is_empty());
    assert_eq!(r.e.model_held_mints(), vec![MINT], "held mints are exactly what the daemon must keep fed");

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
    assert!(st[0].last_print_age_ms.unwrap() < 10_000, "the newest PRINT is fresh - that proves nothing about the reserve: {st:?}");
    let why = st[0].management_ready.clone().unwrap_err();
    assert!(why.contains("curve"), "the refusal names the stale component: {why}");
    assert_eq!(r.e.model_held_degraded().len(), 1);
    // The bound was NOT loosened to make it pass, the position was not closed or abandoned, and the
    // print-driven safeguards are still armed (the rug precursor closes it on a collapse print).
    assert!(r.e.model_position_open(&MINT));
    r.clock += 1_000;
    r.slot += 1;
    r.e.tick(AppEvent::MarketTrade {
        mint: mint(), price_fp: 20_000, quote_lamports: 900_000_000, liquidity_lamports: VSOL,
        signed_base: -90_000_000_000, buyer_entity: 777, age_slots: 30, recv_unix_ms: Some(r.clock),
        trader_pubkey: Some(wallet(999)), slot: Some(r.slot), fee_lamports: Some(70_000),
        cu_consumed: Some(95_000), venue: Some(TradeVenue::PumpFun),
    });
    assert!(!r.e.model_position_open(&MINT), "independent protection still works while degraded");
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
    assert!(led.held[0].step >= 1, "management history exists to be preserved: {:?}", led.held[0]);
    drop(r);

    let mut e2 = fresh_engine(2_000_000_000, &hp);
    let rep = e2.model_held_restore().expect("restore").expect("a ledger existed");
    assert_eq!(rep.positions, 1);
    assert_eq!(rep.inventory_unknown, 0);
    assert!(e2.model_position_open(&MINT));
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv), "inventory restored exactly");
    let after = e2.model_accounting_view(&MINT);
    assert_eq!(after.seed, before.seed);
    assert_eq!(after.realized, before.realized);
    assert_eq!(after.committed, before.committed, "committed capital restored");
    assert_eq!(after.remaining_cost_basis, basis, "remaining cost basis restored");
    assert_eq!(after.attribution_entry_spend, before.attribution_entry_spend);
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
    assert_eq!(deg.len(), 1, "a restored position with no recovered history is DEGRADED, by name");
    assert!(deg[0].management_ready.is_err());
    // History recovery: replay the captured launch + flow for the held mint (the same feed a restart
    // re-reads), then the live curve resumes.
    for ev in &events(40) {
        e2.tick(ev.clone());
    }
    for _ in 0..8 {
        e2.tick(AppEvent::Tick);
    }
    let mut rr = Rig { e: e2, prompts: Arc::new(Mutex::new(Vec::new())), calls, clock, slot: 2_600, n: 500 };
    // The curve plane must be fresh again before management can ask (the stale-reserve gate stays).
    curve(&mut rr.e, rr.clock + 1_000, 2_600, 200_000_000);
    rr.clock += 1_000;
    rr.advance_to_order(180_000);
    let (id, kind, _, _) = rr.e.model_mgmt_pending(&MINT).unwrap_or_else(|| panic!("{:?}", rr.e.model_lane_report()));
    assert_eq!(format!("{kind:?}"), "Exit");
    assert!(rr.e.model_position_open(&MINT), "an instruction is not a fill");
    assert!(rr.clock - fill_ms > 60_000, "the position kept its TRUE age through the restart");
    rr.landing(250_000_000);
    assert!(!rr.e.model_position_open(&MINT), "{:?}", rr.e.model_lane_report());
    assert!(rr.e.model_mgmt_fills().iter().any(|f| f.order_id == id && f.closed));
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
    assert!(a.uncertain_orders >= 1 && !a.is_flat_and_reconciled(), "unresolved stays unresolved: {a:?}");
    assert!(e2.model_safety_rearm("alon").is_err() || !e2.model_safety_blocked());
    // The simulator must not settle an order whose acknowledgement died with the old process.
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    e2.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000).unwrap();
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 - intended), "exactly the filled quantity");
    assert!(e2.model_mgmt_pending(&MINT).is_none());
    // A duplicate report cannot repeat it.
    assert!(e2.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000).is_err());
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
    assert_eq!(e.model_accounting_view(&MINT).committed, e.model_accounting_view(&MINT).attribution_entry_spend.unwrap());

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
    assert!(r.e.model_safety_blocked(), "unrecoverable exposure => entries blocked");
    assert_eq!(r.rep("safety:tripped:held_state_unpersistable"), 1, "{:?}", r.e.model_lane_report());
    // Held positions are still monitored and managed (REDUCE/EXIT unaffected).
    assert!(r.e.model_position_open(&MINT));
}

#[test]
fn the_ledger_follows_the_exposure_through_reduce_and_exit_and_a_restart_after_the_exit_restores_flat() {
    let hp = held_path("e");
    let mut r = rig(|step| if step == 0 { REDUCE } else if step == 1 { EXIT } else { HOLD });
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
    assert_eq!(e2.model_accounting_view(&MINT).realized, realized, "realized survives the restart");
    if closed {
        assert!(!e2.model_position_open(&MINT), "an exited position is not resurrected");
        assert!(e2.model_stop_assessment().is_flat_and_reconciled());
    } else {
        assert!(e2.model_position_open(&MINT));
    }
}

/// ENTRY-BASELINE cohort: the v16 PROD_HOLD control. Entry still comes from the model; management
/// never asks the model (no management prompt is rendered or dispatched), the position is HELD, and
/// at fill + barrier an EXIT of ALL inventory is placed through the ordinary order path (intent, not a
/// fill). Protection (rug precursor / hard stop) is the position store's and is not touched here.
#[test]
fn prod_hold_control_never_asks_the_model_and_exits_all_inventory_at_the_barrier() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| REDUCE); // a REDUCE script that must never be consulted
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    assert_eq!(r.e.management_policy(), ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    let entry_prompts = r.prompts.lock().unwrap().len();
    let inv0 = r.e.model_inventory_tokens(&MINT).expect("fill established inventory");
    r.advance(200_000);
    assert_eq!(r.rep("mgmt:dispatched"), 0, "{:?}", r.e.model_lane_report());
    assert!(r.rep("mgmt:prod_hold:hold") >= 1, "{:?}", r.e.model_lane_report());
    assert_eq!(r.prompts.lock().unwrap().len(), entry_prompts, "no management prompt rendered");
    assert!(r.e.model_mgmt_pending(&MINT).is_none(), "no order before the barrier");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    r.advance_to_order(200_000);
    let (_, _, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("barrier EXIT order placed");
    assert_eq!(filled, 0, "an intent, not a fill");
    assert_eq!(intended, inv0, "EXIT = all remaining inventory");
    assert_eq!(r.rep("mgmt:prod_hold:barrier_exit"), 1);
    assert_eq!(r.rep("mgmt:dispatched"), 0, "the model was never asked for management");
    assert_eq!(r.rep("mgmt:order:exit"), 1);
}

/// A canonical PumpSwap print on the held mint after graduation (reserves ≈ the curve's terminal
/// state so the price is continuous), stamping the engine's swap context for the same tick.
fn amm_print(e: &mut Engine, ts: i64, slot: u64, i: u32) {
    e.tick(AppEvent::AmmSwap {
        mint: mint(),
        pool: [0x50; 32],
        pool_is_canonical: true,
        quote_is_wsol: true,
        token_reserve_pre: 206_900_000_000_000 - u64::from(i) * 1_000_000_000,
        quote_reserve_pre: 85_000_000_000 + u64::from(i) * 500_000,
        fee_bps: Some(30),
        fee_parts: Some((20, 5, 5)),
        virtual_quote: Some(0),
        is_buy: i % 2 == 0,
        token_amount: 1_000_000_000,
        quote_lamports: 410_000,
        trader: wallet(900 + i),
        fee_lamports: Some(5_000),
        cu_consumed: Some(60_000),
        recv_unix_ms: Some(ts),
        slot,
    });
}

/// Position-lifecycle coverage for a CURVE-entered position that graduates mid-hold under the
/// PROD_HOLD control: the barrier EXIT is routed to the pool (AMM pricing through the real quote
/// path, fee labelled unverified), inventory goes to zero exactly once, cost basis / fill clock are
/// the original ones, and the model is never asked. Synthetic events: execution integration only.
#[test]
fn curve_position_that_migrates_mid_hold_exits_through_the_pool_at_the_barrier() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| REDUCE);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    let inv0 = r.e.model_inventory_tokens(&MINT).expect("fill established inventory");
    let fills_before = r.e.model_all_fills().len();
    r.advance(100_000);
    // Graduation: curve complete, then pool prints only.
    r.clock += 1_000;
    r.slot += 1;
    r.e.tick(AppEvent::Migration { mint: mint(), slot: r.slot });
    for i in 0..40u32 {
        r.clock += 5_000;
        r.slot += 1;
        amm_print(&mut r.e, r.clock, r.slot, i);
        ticks(&mut r.e, 2);
    }
    assert!(r.e.model_position_open(&MINT), "migration must not vanish the position");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0), "quantity preserved across migration");
    assert_eq!(r.rep("mgmt:dispatched"), 0, "PROD_HOLD never asks the model: {:?}", r.e.model_lane_report());
    // Barrier: EXIT ALL intent, now an AMM order.
    let end = r.clock + 300_000;
    while r.clock < end && r.e.model_mgmt_pending(&MINT).is_none() {
        r.clock += 1_000;
        r.slot += 1;
        amm_print(&mut r.e, r.clock, r.slot, 100 + (r.slot % 7) as u32);
        ticks(&mut r.e, 2);
    }
    let (_, _, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("barrier EXIT placed after migration");
    assert_eq!((intended, filled), (inv0, 0));
    assert_eq!(r.rep("mgmt:prod_hold:barrier_exit"), 1);
    // Landing state from the pool (≥ 400 ms after the order, later slot) prices the exit.
    r.clock += 1_000;
    r.slot += 5;
    amm_print(&mut r.e, r.clock, r.slot, 7);
    ticks(&mut r.e, 2);
    let rep = r.e.model_lane_report();
    assert_eq!(r.rep("mgmt:fill_amm_sell_fee_unverified"), 1, "exit priced via the pool quote path: {rep:?}");
    assert_eq!(r.rep("mgmt:fill_curve"), 0, "a completed curve is never used to price the exit: {rep:?}");
    assert_eq!(r.rep("mgmt:fill:closed"), 1, "{rep:?}");
    assert!(!r.e.model_position_open(&MINT), "closed exactly once");
    let fills = r.e.model_mgmt_fills();
    assert_eq!(fills.len(), 1);
    assert!(fills[0].closed && fills[0].tokens == inv0);
    assert_eq!(r.e.model_all_fills().len(), fills_before, "no duplicate entry fill");
}

/// Same lifecycle, but the pool feed is STALE at the barrier (no landing-state print): the EXIT
/// intent expires unfilled and the position remains open as explicit exposure — never valued from
/// the completed curve, never closed at a last price, never dropped.
#[test]
fn migrated_position_without_fresh_pool_state_stays_explicit_exposure() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| REDUCE);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.advance(100_000);
    r.clock += 1_000;
    r.slot += 1;
    r.e.tick(AppEvent::Migration { mint: mint(), slot: r.slot });
    for i in 0..3u32 {
        r.clock += 5_000;
        r.slot += 1;
        amm_print(&mut r.e, r.clock, r.slot, i);
        ticks(&mut r.e, 2);
    }
    // Pool goes quiet; only the clock advances (time signals, stale curve prints are NOT usable).
    let end = r.clock + 320_000;
    while r.clock < end && r.e.model_mgmt_pending(&MINT).is_none() {
        r.clock += 1_000;
        r.slot += 1;
        curve_quiet(&mut r.e, r.clock, r.slot);
        ticks(&mut r.e, 2);
    }
    assert!(r.e.model_mgmt_pending(&MINT).is_some(), "barrier EXIT intent placed: {:?}", r.e.model_lane_report());
    // An AMM exit intent waits up to MODEL_AMM_EXIT_TTL_MS (60 s) for a landing-capable swap; with
    // none it expires and the standing intent retries on the declared schedule.
    for _ in 0..70 {
        r.clock += 1_000;
        r.slot += 1;
        curve_quiet(&mut r.e, r.clock, r.slot);
        ticks(&mut r.e, 2);
    }
    let rep = r.e.model_lane_report();
    assert_eq!(r.rep("mgmt:fill_curve"), 0, "completed curve never prices the exit: {rep:?}");
    assert_eq!(r.rep("mgmt:fill_amm_sell_fee_unverified"), 0, "{rep:?}");
    assert!(r.rep("mgmt:order_expired_unfilled") >= 1, "{rep:?}");
    assert!(r.e.model_position_open(&MINT), "exposure stays explicit");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    assert!(r.e.model_mgmt_fills().is_empty(), "no fabricated close");
}

/// Populated-state recovery under the PROD_HOLD control: a filled, 100 s-old position is persisted,
/// a brand-new engine restores it from the file alone, and the barrier fires at the ORIGINAL
/// fill + barrier (not restart + barrier), with exactly one EXIT and no duplicate entry fill. The
/// persisted protection trail (peak/trough), inventory and cost basis come back exactly.
#[test]
fn prod_hold_restart_keeps_the_original_fill_clock_and_exits_exactly_once() {
    use pump_quant_app::engine::ManagementPolicy;
    let hp = held_path("prod_hold_restart");
    let mut r = rig(|_| REDUCE);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    r.e.model_held_attach(&hp);
    r.advance(100_000);
    assert!(r.e.model_held_persist_now());
    let led = r.e.model_held_ledger().clone();
    let h = &led.held[0];
    let (fill_ms, clock, inv0) = (h.fill_ms, r.clock, h.inventory_tokens.expect("inventory known"));
    let (peak, trough, cost) = (h.peak_fp, h.trough_fp, h.cost_lamports);
    assert!(fill_ms > 0 && clock - fill_ms >= 100_000);
    drop(r);

    let calls = Arc::new(AtomicUsize::new(0));
    let mut c = cfg();
    c.floor_fraction_bps = 2_500;
    let mut e2 = Engine::new(c, RunMode::Paper);
    e2.enable_paper_model(Script { prompts: Arc::new(Mutex::new(Vec::new())), calls: Arc::clone(&calls), answer: |_| EXIT });
    e2.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    e2.model_held_attach(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let l2 = e2.model_held_ledger();
    assert_eq!((l2.held[0].fill_ms, l2.held[0].peak_fp, l2.held[0].trough_fp, l2.held[0].cost_lamports), (fill_ms, peak, trough, cost));
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0));
    assert!(e2.model_all_fills().is_empty(), "restore rebuilds books, it does not replay the entry fill");
    // History recovery replays prints from BEFORE our fill: they must not fire protection or
    // close the restored position (the defect this test was written against).
    for ev in &events(40) {
        e2.tick(ev.clone());
    }
    for _ in 0..8 {
        e2.tick(AppEvent::Tick);
    }
    assert!(e2.model_position_open(&MINT), "replayed pre-fill history must not close the position: {:?}", e2.model_lane_report());
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0));
    assert!(e2.model_lane_report().get("protection:skip_pre_fill_print").copied().unwrap_or(0) >= 1);
    assert_eq!(e2.model_lane_report().get("dispatched").copied().unwrap_or(0), 0, "no duplicate entry on the held mint");
    let mut rr = Rig { e: e2, prompts: Arc::new(Mutex::new(Vec::new())), calls, clock, slot: 2_600, n: 500 };
    curve(&mut rr.e, rr.clock + 1_000, 2_600, 200_000_000);
    rr.clock += 1_000;
    rr.advance_to_order(400_000);
    let (id, kind, intended, _) = rr.e.model_mgmt_pending(&MINT).unwrap_or_else(|| panic!("{:?}", rr.e.model_lane_report()));
    assert_eq!(format!("{kind:?}"), "Exit");
    assert_eq!(intended, inv0, "inventory now {:?}; report {:?}", rr.e.model_inventory_tokens(&MINT), rr.e.model_lane_report());
    let age_at_barrier = rr.clock - fill_ms;
    assert!((300_000..=312_000).contains(&age_at_barrier), "barrier at ORIGINAL fill+300 s (age {age_at_barrier} ms), not restart+300 s");
    assert_eq!(rr.rep("mgmt:dispatched"), 0, "the model was never asked: {:?}", rr.e.model_lane_report());
    rr.landing(250_000_000);
    assert!(!rr.e.model_position_open(&MINT), "{:?}", rr.e.model_lane_report());
    let closes: Vec<_> = rr.e.model_mgmt_fills().iter().filter(|f| f.closed).collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].order_id, id);
    assert_eq!(rr.rep("mgmt:order:exit"), 1);
    assert!(rr.e.model_all_fills().is_empty(), "no entry fill was re-booked across the restart");
}

/// Missing-data contract on the protection path: a price-less print (instruction/identity print,
/// `price_fp == 0` = unknown) must not fire the rug precursor or hard stop, must not move
/// peak/trough, and must leave the position open with its inventory. (Live defect: cohort-001
/// segment A booked a −0.50 SOL "exit" from such a print 0.8 s after the fill.)
#[test]
fn a_priceless_print_never_fires_protection() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| REDUCE);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.advance(10_000);
    r.clock += 1_000;
    r.slot += 1;
    r.e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 0,
        quote_lamports: 0,
        liquidity_lamports: 0,
        signed_base: -90_000_000_000,
        buyer_entity: 777,
        age_slots: 30,
        recv_unix_ms: Some(r.clock),
        trader_pubkey: Some(wallet(999)),
        slot: Some(r.slot),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(TradeVenue::PumpFun),
    });
    ticks(&mut r.e, 2);
    assert!(r.e.model_position_open(&MINT), "a price-less print is unknown, not a crash: {:?}", r.e.model_lane_report());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    assert!(r.rep("protection:skip_no_price_print") >= 1, "{:?}", r.e.model_lane_report());
    assert!(r.e.model_mgmt_fills().is_empty());
    // A REAL single-print collapse still protects.
    r.clock += 1_000;
    r.slot += 1;
    r.e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 20_000,
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 778,
        age_slots: 30,
        recv_unix_ms: Some(r.clock),
        trader_pubkey: Some(wallet(998)),
        slot: Some(r.slot),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(TradeVenue::PumpFun),
    });
    assert!(!r.e.model_position_open(&MINT), "the rug precursor still protects on a priced collapse");
}

/// The exact failing tape of cohort-001 segment A (request 507), in authenticated order: after the fill
/// each real trade arrived as TWO representations — an instruction/identity print with `price_fp = 0`
/// and the priced reserve print — at +142, +147 and +471/472 ms. Price ratios are the recorded ones
/// (0.99931, 0.99860, 0.99225 of the accept price). Under the corrected contract: no exit, peak/trough
/// move only on priced prints, the position and inventory survive, and the two representations of one
/// trade do not read as a drop. (Live journal booked −0.5015 SOL from the +472 ms zero print.)
#[test]
fn segment_a_failing_tape_replayed_does_not_fire_protection() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| REDUCE);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 1_800_000 });
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let hp = held_path("seg_a_tape");
    r.e.model_held_attach(&hp);
    assert!(r.e.model_held_persist_now());
    let entry = r.e.model_held_ledger().held[0].entry_price_fp;
    let peak0 = r.e.model_held_ledger().held[0].peak_fp;
    let ratios = [(142i64, 0.99931f64), (147, 0.99860), (471, 0.99225)];
    for (dt, ratio) in ratios {
        let px = (entry as f64 * ratio) as u64;
        let ts = r.clock + dt;
        r.slot += 1;
        // identity print (no price), then the priced reserve print of the SAME trade
        for price_fp in [0u64, px] {
            r.e.tick(AppEvent::MarketTrade {
                mint: mint(),
                price_fp: price_fp as i128,
                quote_lamports: if price_fp == 0 { 0 } else { 6_000_000_000 },
                liquidity_lamports: if price_fp == 0 { 0 } else { VSOL },
                signed_base: -125_093_404_721,
                buyer_entity: 4242,
                age_slots: 30,
                recv_unix_ms: Some(ts),
                trader_pubkey: Some(wallet(4242)),
                slot: Some(r.slot),
                fee_lamports: Some(5_000),
                cu_consumed: Some(60_000),
                venue: Some(TradeVenue::PumpFun),
            });
        }
        ticks(&mut r.e, 1);
    }
    let rep = r.e.model_lane_report();
    assert!(r.e.model_position_open(&MINT), "no exit on the failing tape: {rep:?}");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    assert_eq!(r.rep("protection:skip_no_price_print"), 3, "{rep:?}");
    assert!(r.e.model_mgmt_fills().is_empty());
    assert!(r.e.model_held_persist_now());
    let h = &r.e.model_held_ledger().held[0];
    assert_eq!(h.peak_fp, peak0, "a zero print never becomes a peak/trough; small priced dips do not raise the peak");
    assert!(h.trough_fp >= (entry as f64 * 0.992) as u64, "trough from PRICED prints only: {}", h.trough_fp);
    assert_eq!(h.inventory_tokens, Some(inv0));
}

/// Model-mode hard stop fires at entry·(1−35 %) and NOT on an ordinary dip below entry (the
/// level was computed as max(trail@0, stop) == entry: any tick below entry closed the position).
#[test]
fn model_mode_hard_stop_is_35_percent_below_entry_not_any_dip() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| REDUCE);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 1_800_000 });
    let hp = held_path("hard_stop_level");
    r.e.model_held_attach(&hp);
    assert!(r.e.model_held_persist_now());
    let entry = r.e.model_held_ledger().held[0].entry_price_fp;
    let mut dip = |r: &mut Rig, ratio: f64| {
        r.clock += 1_000;
        r.slot += 1;
        r.e.tick(AppEvent::MarketTrade {
            mint: mint(),
            price_fp: ((entry as f64 * ratio) as u64) as i128,
            quote_lamports: 300_000_000,
            liquidity_lamports: VSOL,
            signed_base: -20_000_000_000,
            buyer_entity: 5151,
            age_slots: 30,
            recv_unix_ms: Some(r.clock),
            trader_pubkey: Some(wallet(5151)),
            slot: Some(r.slot),
            fee_lamports: Some(5_000),
            cu_consumed: Some(60_000),
            venue: Some(TradeVenue::PumpFun),
        });
        ticks(&mut r.e, 1);
    };
    // gradual decline, no single step ≥ 30 %: −1 %, −10 %, −20 %, −30 %, −34 % → still held
    for ratio in [0.99, 0.90, 0.80, 0.70, 0.66] {
        dip(&mut r, ratio);
        assert!(r.e.model_position_open(&MINT), "held at {ratio}: {:?}", r.e.model_lane_report());
    }
    dip(&mut r, 0.649);
    assert!(!r.e.model_position_open(&MINT), "hard stop at entry·0.65");
}

/// Cohort cutoff racing a BUY answer: the request was dispatched BEFORE the cutoff, its BUY answer
/// arrives AFTER the entry block is set → refused by name (EntriesBlocked), no order, no position,
/// and the lane keeps ticking (no SAFETY trip).
#[test]
fn cutoff_blocks_a_buy_answer_to_a_request_dispatched_before_the_cutoff() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script { prompts: Arc::clone(&prompts), calls: Arc::clone(&calls), answer: |_| REDUCE });
    for ev in &events(40) {
        e.tick(ev.clone());
    }
    // the ask has been dispatched (worker has it); the cutoff lands now
    e.set_model_entries_blocked(true);
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    let rep = e.model_lane_report().clone();
    assert!(!e.model_position_open(&MINT), "no position may open after the cutoff: {rep:?}");
    assert_eq!(e.model_pending_orders(), 0);
    assert!(rep.get("discard:entries_blocked").copied().unwrap_or(0) >= 1 || rep.get("refuse:entries_blocked").copied().unwrap_or(0) >= 1, "{rep:?}");
    // further candidate ticks never dispatch while blocked
    let before = calls.load(Ordering::SeqCst);
    for i in 41..60 {
        print(&mut e, i, t_last + 1_500 + i64::from(i) * 2_000, 2_100 + u64::from(i));
        ticks(&mut e, 1);
    }
    assert_eq!(calls.load(Ordering::SeqCst), before, "no new dispatch while draining");
}

/// Cutoff with a pending fill and then an open position: the accepted BUY's fill still lands
/// (pending-fill reconciliation continues), the position is supervised under PROD_HOLD and exits at
/// the barrier while entries stay blocked; no new ask is ever dispatched.
#[test]
fn cutoff_lets_a_pending_fill_land_and_supervises_the_position_to_its_barrier_exit() {
    use pump_quant_app::engine::ManagementPolicy;
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    e.enable_paper_model(Script { prompts: Arc::clone(&prompts), calls: Arc::clone(&calls), answer: |_| REDUCE });
    for ev in &events(40) {
        e.tick(ev.clone());
    }
    ticks(&mut e, 8); // BUY accepted → pending order awaiting its landing state
    assert_eq!(e.model_pending_orders(), 1, "{:?}", e.model_lane_report());
    e.set_model_entries_blocked(true); // cutoff lands with the fill pending
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    assert!(e.model_position_open(&MINT), "the already-accepted fill lands: {:?}", e.model_lane_report());
    assert_eq!(e.model_pending_orders(), 0);
    let asks_at_cutoff = calls.load(Ordering::SeqCst);
    let mut r = Rig { e, prompts, calls, clock: t_last + 1_500, slot: 2_100, n: 41 };
    r.advance_to_order(400_000);
    assert!(r.e.model_mgmt_pending(&MINT).is_some(), "barrier EXIT submitted while draining");
    r.landing(0);
    assert!(!r.e.model_position_open(&MINT), "flat after the barrier exit");
    assert_eq!(r.e.model_pending_orders(), 0);
    assert_eq!(r.calls.load(Ordering::SeqCst), asks_at_cutoff, "no entry ask while draining (the PROD_HOLD control asks no model)");
}

#[test]
fn a_system_error_reversal_voids_the_position_without_a_sale_and_keeps_the_equity_identity() {
    let hp = held_path("rev");
    let mut r = rig(|_| HOLD);
    r.e.model_held_attach(&hp);
    r.advance(130_000);
    let before = r.e.model_accounting_view(&MINT);
    assert!(before.committed > 0, "a position is held");
    let seed = before.seed as i128;
    // Equity identity before: seed + realized == balance; committed is carried in positions.
    let realized_before = before.realized;
    // Reverse a (hypothetical) earlier booked loss of 0.157 SOL on this mint and void the open fill.
    let rec = r.e.model_system_error_reversal(&MINT, 157_263_531, "quote_mismatch_test");
    assert_eq!(rec["applied"], true);
    assert_eq!(rec["voided_basis_lamports"].as_u64(), before.remaining_cost_basis);
    assert!(!r.e.model_position_open(&MINT), "position voided");
    let after = r.e.model_accounting_view(&MINT);
    assert_eq!(after.realized, realized_before + 157_263_531, "realized reversed by exactly the evidenced amount");
    assert_eq!(after.committed, 0, "no committed capital remains");
    assert_eq!(after.balance as i128, seed + after.realized, "seed + realized == balance");
    // No exit was booked: nothing in the fills/exits counters of the lane changed for a sale.
    assert_eq!(r.rep("mgmt:exit_filled"), 0);
    assert_eq!(r.rep("mgmt:cancelled_by_reversal") + r.rep("fill:"), r.rep("fill:"), "no sale counters moved");
    // Idempotent on a flat mint: a second application voids nothing and reverses only what is asked.
    let rec2 = r.e.model_system_error_reversal(&MINT, 0, "noop");
    assert_eq!(rec2["voided_basis_lamports"], serde_json::Value::Null);
    assert_eq!(r.e.model_accounting_view(&MINT).realized, after.realized);
    // Crash/restart: the correction is durable and applied exactly once — the restored ledger has
    // no position, the reversed realized figure and the same balance.
    assert!(r.e.model_held_persist_now());
    drop(r);
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    let rep = e2.model_held_restore().expect("restore").expect("ledger");
    assert_eq!(rep.positions, 0);
    assert!(!e2.model_position_open(&MINT));
    let restored = e2.model_accounting_view(&MINT);
    assert_eq!(restored.realized, after.realized);
    assert_eq!(restored.committed, 0);
    assert_eq!(restored.balance, after.balance);
}

/// A trade print on an UNRELATED mint: moves the feed clock without giving the held mint any
/// landing-capable state (no curve observation, no print on it).
fn clock_only(e: &mut Engine, ts: i64, slot: u64) {
    e.tick(AppEvent::MarketTrade {
        mint: DomainMint::from_bytes([0x77; 32]),
        price_fp: 10_000,
        quote_lamports: 1_000_000,
        liquidity_lamports: VSOL,
        signed_base: 1_000_000,
        buyer_entity: 7,
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(wallet(7)),
        slot: Some(slot),
        fee_lamports: Some(5_000),
        cu_consumed: Some(60_000),
        venue: Some(TradeVenue::PumpFun),
    });
}

/// The barrier EXIT is ONE standing intent: with no landing state the first order expires and is
/// NOT re-placed every tick — only on the backoff schedule (30 s) or when new state arrives; the
/// eventual fill closes the position exactly once. Attempts are counted and journaled.
#[test]
fn prod_hold_barrier_exit_is_an_idempotent_intent_with_backoff_not_a_fresh_order_per_tick() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| REDUCE);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    r.advance(200_000);
    assert!(r.e.model_mgmt_pending(&MINT).is_none());
    // Reach the barrier with clock-only steps (no state on the held mint).
    let mut guard = 0;
    while r.e.model_mgmt_pending(&MINT).is_none() && guard < 400 {
        r.clock += 1_000;
        r.slot += 1;
        clock_only(&mut r.e, r.clock, r.slot);
        ticks(&mut r.e, 1);
        guard += 1;
    }
    assert_eq!(r.rep("mgmt:order:exit"), 1, "first attempt placed at the barrier");
    assert_eq!(r.rep("mgmt:exit_attempt|first"), 1);
    let t_first = r.clock;
    // 20 s of clock with no state: the order expires (TTL 5 s) and is NOT re-placed (backoff 30 s).
    for _ in 0..20 {
        r.clock += 1_000;
        r.slot += 1;
        clock_only(&mut r.e, r.clock, r.slot);
        ticks(&mut r.e, 1);
    }
    assert_eq!(r.rep("mgmt:order_expired_unfilled"), 1);
    assert_eq!(r.rep("mgmt:order:exit"), 1, "no fresh order per tick: {:?}", r.e.model_lane_report());
    assert!(r.rep("mgmt:exit_waiting") >= 1);
    // Past the 30 s schedule: exactly one more attempt.
    while r.clock - t_first < 32_000 {
        r.clock += 1_000;
        r.slot += 1;
        clock_only(&mut r.e, r.clock, r.slot);
        ticks(&mut r.e, 1);
    }
    assert_eq!(r.rep("mgmt:order:exit"), 2, "{:?}", r.e.model_lane_report());
    assert_eq!(r.rep("mgmt:exit_attempt|schedule"), 1);
    // New landing-capable state arrives: the pending attempt fills; inventory closes exactly once.
    r.clock += 1_000;
    r.slot += 5;
    curve_quiet(&mut r.e, r.clock, r.slot);
    ticks(&mut r.e, 2);
    assert!(!r.e.model_position_open(&MINT), "closed through a real landing fill");
    assert_eq!(r.rep("mgmt:fill_curve"), 1);
    assert!(r.rep("mgmt:order:exit") <= 3, "no order storm: {:?}", r.e.model_lane_report());
}

/// End-of-run `report()` values the open book (force-close at last mark) — in paper-model mode
/// that valuation never moves the authoritative ledger and never overwrites the durable
/// MODEL_HELD: a successor restores the position exactly as it was.
#[test]
fn a_report_only_valuation_does_not_touch_realized_or_the_durable_held_ledger() {
    let hp = held_path("report_only");
    let mut r = rig(|_| HOLD);
    r.e.model_held_attach(&hp);
    r.advance(130_000);
    assert!(r.e.model_held_persist_now());
    let before = r.e.model_accounting_view(&MINT);
    let led_before = r.e.model_held_ledger();
    assert_eq!(led_before.held.len(), 1);
    let _ = r.e.report();
    assert_eq!(r.e.model_accounting_view(&MINT).realized, before.realized, "valuation is not a sale");
    assert!(!r.e.model_held_persist_now(), "no durable write after a report-only valuation");
    drop(r);
    let mut e2 = fresh_engine(2_000_000_000, &hp);
    let rep = e2.model_held_restore().expect("restore").expect("ledger");
    assert_eq!(rep.positions, 1, "the successor still owns the position");
    assert!(e2.model_position_open(&MINT));
    assert_eq!(e2.model_accounting_view(&MINT).realized, before.realized);
}

/// A quiet pool never emits a landing swap, so the standing EXIT intent would wait forever. The
/// daemon's RPC snapshot of the CURRENT pool state (vaults + signed virtual quote) is a valid,
/// size-feasible liquidation source: the exit lands on it exactly once at the program's own
/// arithmetic. Entries never fill on a snapshot; a non-held mint is ignored.
#[test]
fn a_pending_exit_on_a_quiet_pool_lands_on_the_rpc_pool_state_snapshot_exactly_once() {
    use pump_quant_app::engine::ManagementPolicy;
    let mut r = rig(|_| HOLD);
    r.e.set_management_policy(ManagementPolicy::ProdHoldControl { barrier_ms: 300_000 });
    r.advance(60_000);
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    // Graduate and print from the pool so the market is AMM-regime.
    r.e.tick(AppEvent::Migration { mint: mint(), slot: r.slot });
    for i in 0..5 {
        r.clock += 1_000;
        r.slot += 1;
        amm_print(&mut r.e, r.clock, r.slot, i);
        ticks(&mut r.e, 2);
    }
    // Reach the barrier with a quiet pool.
    let end = r.clock + 320_000;
    while r.clock < end && r.e.model_mgmt_pending(&MINT).is_none() {
        r.clock += 1_000;
        r.slot += 1;
        curve_quiet(&mut r.e, r.clock, r.slot);
        ticks(&mut r.e, 2);
    }
    assert!(r.e.model_mgmt_pending(&MINT).is_some());
    let fills_before = r.e.model_all_fills().len();
    // A snapshot for a mint that is not held does nothing.
    assert!(!r.e.model_on_amm_state_snapshot([0x99; 32], [0x50; 32], 1, 1, 0, r.slot + 5, r.clock + 1_000));
    assert!(r.e.model_position_open(&MINT));
    // The real pool state (drained pool: 1.05 SOL vault, 969.5M tokens, vq +17.58 SOL) prices the exit.
    r.clock += 1_000;
    r.slot += 5;
    let filled = r.e.model_on_amm_state_snapshot(MINT, [0x50; 32], 969_511_247_820_886, 1_055_445_943, 17_584_504_840, r.slot, r.clock);
    assert!(filled, "{:?}", r.e.model_lane_report());
    assert!(!r.e.model_position_open(&MINT), "closed exactly once");
    assert_eq!(r.rep("mgmt:fill_source|rpc_state_snapshot"), 1);
    assert_eq!(r.rep("mgmt:fill_amm_sell_fee_unverified"), 1);
    let f = r.e.model_mgmt_fills().last().unwrap().clone();
    assert!(f.closed && f.tokens == inv0);
    assert_eq!(r.e.model_all_fills().len(), fills_before, "no entry fill was created by a snapshot");
    // Proceeds follow the program arithmetic on the EFFECTIVE quote (vault + vq), not the raw vault.
    let gross = pump_quant_protocol::pumpswap_event::sell_gross_quote_out(969_511_247_820_886, 1_055_445_943, 17_584_504_840, u128::from(inv0)).unwrap();
    let px = f.price_fp as u128;
    let expect_px = gross * 1_000_000_000 / u128::from(inv0);
    assert_eq!(px, expect_px, "priced at the effective-quote sell arithmetic");
    // A second snapshot after the close books nothing.
    assert!(!r.e.model_on_amm_state_snapshot(MINT, [0x50; 32], 969_511_247_820_886, 1_055_445_943, 17_584_504_840, r.slot + 1, r.clock + 1_000));
}
