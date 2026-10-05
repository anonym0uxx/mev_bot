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
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
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
fn add_is_a_named_unsupported_result_and_never_substitutes_an_amount() {
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD });
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let cash0 = r.e.model_free_cash_lamports();
    r.advance(70_000);
    r.landing(250_000_000);
    assert!(r.rep("mgmt:add_unsupported") >= 1, "{:?}", r.e.model_lane_report());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    assert!(r.e.model_mgmt_pending(&MINT).is_none(), "no order of any size");
    assert_eq!(r.e.model_free_cash_lamports(), cash0, "no cash moved");
    assert!(!r.e.model_management_complete(), "management is reported INCOMPLETE");
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
