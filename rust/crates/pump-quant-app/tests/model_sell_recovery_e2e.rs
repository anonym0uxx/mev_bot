//! Management sells (REDUCE / EXIT / ADD) across a restart. A report states the order's CUMULATIVE filled quantity,
//! so applying it is idempotent. Expected values are computed independently from the fixture's own numbers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_manage::{MgmtKind, SellReportResult, SellState};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const T0: i64 = 1_800_000_000_000;
const CREATOR: [u8; 32] = [0xCD; 32];
const MINT: [u8; 32] = [0xAB; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;
const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const REDUCE: &str = "DECISION: REDUCE\nINVALIDATION: none\nEVIDENCE: x";
const EXIT: &str = "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: x";
const ADD: &str = "DECISION: ADD\nINVALIDATION: none\nEVIDENCE: x";

struct Script {
    calls: Arc<AtomicUsize>,
    answer: fn(i64) -> &'static str,
}
impl ModelSource for Script {
    fn complete(&self, _s: &str, user: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if user.starts_with("Decide the next action for a position you already hold") {
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

fn mint() -> DomainMint {
    DomainMint::from_bytes(MINT)
}
fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8 + 1;
    w
}
fn cfg() -> Config {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    c.floor_fraction_bps = 2_500;
    c
}
fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}
thread_local! {
    /// Every FEED event this test thread delivered, in order (what a durable feed would be able to replay).
    static FEED_LOG: std::cell::RefCell<Vec<AppEvent>> = const { std::cell::RefCell::new(Vec::new()) };
}
fn tick_log(e: &mut Engine, ev: AppEvent) {
    FEED_LOG.with(|l| l.borrow_mut().push(ev));
    e.tick(ev);
}
fn feed_log() -> Vec<AppEvent> {
    FEED_LOG.with(|l| l.borrow().clone())
}
fn curve_obs(e: &mut Engine, ts: i64, slot: u64, dsol: u64) {
    tick_log(
        e,
        AppEvent::CurveObserved {
            mint: mint(),
            v_sol_lamports: VSOL + dsol,
            v_tokens: VTOK - 4_000_000_000_000,
            real_sol_lamports: 8_100_000_000,
            real_tokens: 565_000_000_000_000,
            recv_unix_ms: Some(ts),
            slot,
        },
    );
}
fn print(e: &mut Engine, i: u32, ts: i64, slot: u64, price: i128) {
    tick_log(
        e,
        AppEvent::MarketTrade {
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
        },
    );
}
fn warm_events(n: u32) -> Vec<AppEvent> {
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
    ev.push(AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem: false,
        slot: 0,
    });
    ev.push(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

#[allow(dead_code)] // `calls` is kept for debugging request counts
struct Rig {
    e: Engine,
    calls: Arc<AtomicUsize>,
    clock: i64,
    slot: u64,
    n: u32,
}

fn held_path(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pq_sell_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("held.json")
}

fn rig(answer: fn(i64) -> &'static str, held: &std::path::Path) -> Rig {
    rig_with(answer, held, None)
}

fn flow_prov() -> pump_quant_app::flow_checkpoint::Provenance {
    pump_quant_app::flow_checkpoint::Provenance {
        seed_source: "t".into(),
        seed_sha256: String::new(),
        seed_before_ms: 0,
        producer: "t".into(),
    }
}

fn rig_with(
    answer: fn(i64) -> &'static str,
    held: &std::path::Path,
    flow: Option<&std::path::Path>,
) -> Rig {
    FEED_LOG.with(|l| l.borrow_mut().clear());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        calls: Arc::clone(&calls),
        answer,
    });
    e.model_held_attach(held);
    if let Some(f) = flow {
        e.model_flow_attach(
            f,
            pump_quant_market_state::flow_reducer::FlowParams::default(),
            flow_prov(),
            0,
        );
    }
    for ev in &warm_events(40) {
        tick_log(&mut e, *ev);
    }
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve_obs(&mut e, t_last + 1_500, 2_100, 200_000_000);
    for _ in 0..200 {
        if e.model_position_open(&MINT) {
            break;
        }
        ticks(&mut e, 1);
    }
    assert!(
        e.model_position_open(&MINT),
        "setup: entry filled: {:?}",
        e.model_lane_report()
    );
    Rig {
        e,
        calls,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
    }
}

impl Rig {
    fn advance_to_order(&mut self, max_ms: i64) {
        let end = self.clock + max_ms;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.clock += 1_000;
            self.slot += 1;
            self.n += 1;
            curve_obs(&mut self.e, self.clock, self.slot, 200_000_000);
            print(
                &mut self.e,
                self.n,
                self.clock,
                self.slot,
                45_300 + i128::from(self.n % 7),
            );
            ticks(&mut self.e, 2);
        }
    }
}

fn fresh(held: &std::path::Path) -> Engine {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        calls: Arc::new(AtomicUsize::new(0)),
        answer: |_| HOLD,
    });
    e.model_held_attach(held);
    e
}

/// Book value of the money side that a restart must reproduce exactly.
fn books(e: &Engine) -> (i128, u64, u64, Option<u64>, Option<u64>) {
    let v = e.model_accounting_view(&MINT);
    (
        v.realized,
        v.committed,
        v.balance,
        v.remaining_cost_basis,
        e.model_inventory_tokens(&MINT),
    )
}

/// A world stopped with a REDUCE order created and not yet filled (crash after intent, before any fill).
fn reduce_pending_world(tag: &str) -> (std::path::PathBuf, u64, u64) {
    let hp = held_path(tag);
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    assert!(r.e.model_held_persist_now());
    (hp, id, intended)
}

#[test]
fn a_crash_after_intent_restores_the_order_unresolved_and_a_report_settles_it_exactly_once() {
    let (hp, id, intended) = reduce_pending_world("s_a");
    let mut e2 = fresh(&hp);
    let rep = e2.model_held_restore().unwrap().unwrap();
    assert_eq!(rep.pending_uncertain, 1);
    let b0 = books(&e2);
    let inv0 = b0.4.unwrap();
    // Identity and quantity restored; nothing booked; never resubmitted or auto-filled.
    assert_eq!(
        e2.model_mgmt_pending(&MINT).map(|p| (p.0, p.2, p.3)),
        Some((id, intended, 0))
    );
    ticks(&mut e2, 6);
    assert_eq!(
        books(&e2),
        b0,
        "an uncertain sell is never simulated-filled"
    );
    assert!(e2.model_mgmt_pending(&MINT).is_some(), "and never expired");
    // The first report settles it: exactly the reported quantity leaves inventory.
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    let b1 = books(&e2);
    assert_eq!(b1.4, Some(inv0 - intended));
    assert!(b1.0 != b0.0, "realized moved once");
    // Repeats and older partials change nothing.
    for cum in [intended, intended, intended / 2, 1] {
        assert_eq!(
            e2.model_mgmt_ingest_report(MINT, id, cum, 22_000),
            SellReportResult::Duplicate
        );
    }
    assert_eq!(books(&e2), b1);
    assert_eq!(
        e2.model_sell_rec(id).map(|r| (r.state, r.filled)),
        Some((SellState::Completed, intended))
    );
}

#[test]
fn a_crash_after_a_partial_fill_restores_the_remainder_and_the_cumulative_key_prevents_a_second_debit(
) {
    let hp = held_path("s_b");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let part = intended / 3;
    assert_eq!(
        r.e.model_mgmt_ingest_report(MINT, id, part, 22_000),
        SellReportResult::Applied { delta: part }
    );
    ticks(&mut r.e, 2);
    assert!(r.e.model_held_persist_now());
    let before = books(&r.e);
    assert_eq!(before.4, Some(inv0 - part));
    drop(r);

    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    // Restored: identity, filled and remaining quantity, inventory, basis, cash; the remainder is unresolved.
    assert_eq!(
        books(&e2),
        before,
        "money side restored exactly, not replayed"
    );
    assert_eq!(
        e2.model_mgmt_pending(&MINT).map(|p| (p.0, p.2, p.3)),
        Some((id, intended, part))
    );
    assert!(
        e2.model_position_open(&MINT),
        "the remainder keeps the position held and monitored"
    );
    // A late repeat of the SAME partial changes nothing (cumulative key), nor does an older one.
    for cum in [part, part, part - 1] {
        assert_eq!(
            e2.model_mgmt_ingest_report(MINT, id, cum, 22_000),
            SellReportResult::Duplicate
        );
    }
    assert_eq!(books(&e2), before, "no second debit");
    // The rest arrives: the delta over the restored cumulative is applied once.
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Applied {
            delta: intended - part
        }
    );
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 - intended));
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Duplicate
    );
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 - intended));
}

#[test]
fn a_fill_that_completed_before_publication_is_recovered_from_the_older_ledger_and_the_report_is_not_lost(
) {
    // The ledger on disk predates the fill (crash after the fill, before the durable publication). The fill is
    // reported again after restart: it is applied exactly once; nothing was double counted because the
    // restored books do not contain it.
    let (hp, id, intended) = reduce_pending_world("s_c");
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 - intended));
    let once = books(&e2);
    // A SECOND restart from the republished ledger followed by the same report: still once.
    assert!(e2.model_held_persist_now());
    let mut e3 = fresh(&hp);
    e3.model_held_restore().unwrap().unwrap();
    assert_eq!(books(&e3), once);
    assert_eq!(
        e3.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Duplicate
    );
    assert_eq!(
        books(&e3),
        once,
        "the late report after republication is a duplicate"
    );
}

fn hard_collapse(e: &mut Engine, clock: i64, slot: u64) {
    // A single-print collapse: the agreed hard safeguard (rug precursor) that closes a held position
    // independently of any model order.
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 20_000,
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 777,
        age_slots: 30,
        recv_unix_ms: Some(clock),
        trader_pubkey: Some(wallet(999)),
        slot: Some(slot),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
}

#[test]
fn conflicting_management_reports_preserve_evidence_block_new_exposure_and_survive_a_restart() {
    let (hp, id, intended) = reduce_pending_world("s_d");
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    // A report claiming MORE than the order ever asked for contradicts the order itself.
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id, intended + 1, 22_000),
        SellReportResult::Fault
    );
    let f = e2
        .model_sell_faults()
        .get(&id)
        .expect("fault recorded")
        .clone();
    assert_eq!(
        (f.source, f.books_filled, f.reported.clone()),
        ("report_exceeds_order", 0, vec![intended + 1])
    );
    assert!(
        e2.model_mint_is_blocked(&MINT),
        "new exposure on the mint is blocked"
    );
    assert!(e2.model_inventory_tokens(&MINT).is_some());
    // Nothing was applied: the order is still pending at 0 filled.
    assert_eq!(e2.model_mgmt_pending(&MINT).map(|p| p.3), Some(0));
    // The fault survives a restart, still blocking, still refusing a re-arm.
    assert!(e2.model_held_persist_now());
    let mut e3 = fresh(&hp);
    e3.model_held_restore().unwrap().unwrap();
    assert_eq!(e3.model_sell_faults().get(&id), Some(&f));
    assert!(e3.model_mint_is_blocked(&MINT));
    // Settle the order correctly, then contradict the SETTLED record with a larger cumulative.
    assert_eq!(
        e3.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert_eq!(
        e3.model_mgmt_ingest_report(MINT, id, intended + 5, 22_000),
        SellReportResult::Fault
    );
    assert_eq!(
        e3.model_sell_faults().get(&id).map(|f| f.reported.clone()),
        Some(vec![intended + 1, intended + 5]),
        "every contradicting report is kept verbatim"
    );
}

#[test]
fn an_unknown_or_compacted_management_report_is_named_and_never_applied() {
    let (hp, id, intended) = reduce_pending_world("s_e");
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let b0 = books(&e2);
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id + 40, 5, 22_000),
        SellReportResult::Rejected("unknown_order")
    );
    // Right id, wrong mint: refused by name, never matched by id alone.
    assert_eq!(
        e2.model_mgmt_ingest_report([0x11; 32], id, intended, 22_000),
        SellReportResult::Rejected("unknown_order")
    );
    assert_eq!(books(&e2), b0);
    assert!(
        e2.model_sell_faults().is_empty(),
        "unknown is unresolved, not a fault"
    );
}

#[test]
fn a_protective_trigger_over_an_uncertain_sell_sells_nothing_reserved_and_the_position_stays_protected(
) {
    let hp = held_path("s_f");
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(r.e.model_held_persist_now());
    drop(r);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let b0 = books(&e2);
    let inv0 = b0.4.unwrap();
    assert_eq!(inv0, intended, "the EXIT covers the whole inventory");
    // The agreed hard safeguard fires while the EXIT's acknowledgement is unknown.
    let clock = T0 + 1_000 + 40 * 2_000 + 400_000;
    hard_collapse(&mut e2, clock, 9_000);
    // The trigger alone removed no inventory and credited no cash: every token is reserved by the unresolved sell.
    assert!(
        e2.model_position_open(&MINT),
        "position stays open and monitored"
    );
    assert_eq!(
        books(&e2),
        b0,
        "no inventory removed, no cash credited, by a trigger"
    );
    assert!(
        e2.model_protection_deferred() >= 1,
        "the deferral is counted, not silent"
    );
    assert!(
        e2.model_mgmt_pending(&MINT).is_some(),
        "the unresolved EXIT is not dropped"
    );
    assert!(
        e2.model_sell_faults().get(&id).is_none(),
        "no fault: nothing was done under it"
    );
    // Evidence then settles it exactly once.
    assert_eq!(
        e2.ev_px(MINT, id, MgmtKind::Exit, intended, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert_eq!(e2.model_inventory_tokens(&MINT), None);
    let realized = e2.model_accounting_view(&MINT).realized;
    assert_eq!(
        e2.ev_px(MINT, id, MgmtKind::Exit, intended, intended, 22_000),
        SellReportResult::Duplicate
    );
    assert_eq!(
        e2.model_accounting_view(&MINT).realized,
        realized,
        "no second credit"
    );
}

#[test]
fn an_uncertain_partial_reduce_reserves_only_its_own_tokens_and_a_trigger_sells_only_the_free_remainder(
) {
    let hp = held_path("s_r");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(r.e.model_held_persist_now());
    drop(r);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    let clock = T0 + 1_000 + 40 * 2_000 + 400_000;
    hard_collapse(&mut e2, clock, 9_000);
    // The trigger created a protective ORDER for only the free part (inventory - reserved). It moved nothing yet.
    let (_pid, pq, pf, _code) = e2
        .model_protect_pending_order(&MINT)
        .expect("protective order");
    assert_eq!((pq, pf), (inv0 - intended, 0));
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        Some(inv0),
        "an intent removes no inventory"
    );
    // The paper executor's landing state reconciles the fill: exactly the free part leaves.
    let mut c = clock;
    for k in 0..3u64 {
        if e2.model_protect_pending_order(&MINT).is_none() {
            break;
        }
        c += 1_000;
        curve_obs(&mut e2, c, 9_001 + k, 200_000_000);
        ticks(&mut e2, 2);
    }
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(intended));
    assert!(
        e2.model_position_open(&MINT),
        "the reserved remainder is still held and protected"
    );
    // The executor then reports the REDUCE: its tokens leave once; total sold never exceeds the original inventory.
    assert_eq!(
        e2.ev_px(MINT, id, MgmtKind::Reduce, intended, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        None,
        "inventory ends at exactly zero, never negative or resurrected"
    );
}

#[test]
fn a_completed_exit_cannot_be_applied_again_after_a_restart_and_leaves_no_inventory() {
    let hp = held_path("s_g");
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    assert_eq!(
        r.e.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert!(!r.e.model_position_open(&MINT));
    ticks(&mut r.e, 2);
    assert!(r.e.model_held_persist_now());
    let done = books(&r.e);
    assert_eq!(done.4, None);
    drop(r);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    assert!(!e2.model_position_open(&MINT), "nothing resurrected");
    assert_eq!(books(&e2).0, done.0, "realized restored, not replayed");
    assert_eq!(
        e2.model_sell_rec(id).map(|r| (r.state, r.filled)),
        Some((SellState::Completed, intended))
    );
    for cum in [intended, intended - 1, 1] {
        assert_eq!(
            e2.model_mgmt_ingest_report(MINT, id, cum, 22_000),
            SellReportResult::Duplicate
        );
    }
    assert_eq!(e2.model_inventory_tokens(&MINT), None);
    assert_eq!(books(&e2).0, done.0);
}

#[test]
fn a_report_cannot_oversell_the_order_or_the_inventory_and_free_inventory_is_never_guessed() {
    let (hp, id, intended) = reduce_pending_world("s_h");
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    assert!(intended < inv0, "a REDUCE leaves inventory");
    // The order's own bound is enforced before inventory is touched.
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id, inv0, 22_000),
        SellReportResult::Fault
    );
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0));
}

#[test]
fn a_foreign_session_verdict_cannot_create_a_sell_or_add_order_nor_answer_a_live_management_request(
) {
    let hp = held_path("s_i");
    let mut r = rig(|_| HOLD, &hp);
    assert!(r.e.model_held_persist_now());
    let seq0 = r.e.model_held_ledger().mgmt_seq;
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    // Management ids live above 1<<40. Every kind of verdict text, from a session that is not this process.
    let foreign = r.e.model_session_id().wrapping_add(1);
    for (k, text) in [
        REDUCE,
        EXIT,
        "DECISION: ADD\nINVALIDATION: none\nEVIDENCE: x",
    ]
    .iter()
    .enumerate()
    {
        for id in [(1u64 << 40) + 1, (1u64 << 40) + 2 + k as u64, 1, 2] {
            r.e.model_offer_external_verdict(foreign, id, MINT, text);
        }
    }
    ticks(&mut r.e, 3);
    assert!(
        r.e.model_mgmt_pending(&MINT).is_none(),
        "no order from a foreign verdict"
    );
    assert_eq!(r.e.model_held_ledger().mgmt_seq, seq0, "no id consumed");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    assert!(r.e.model_position_open(&MINT));
    let discarded: u64 =
        r.e.model_lane_report()
            .iter()
            .filter(|(k, _)| k.starts_with("discard:foreign_session"))
            .map(|(_, v)| *v)
            .sum();
    assert_eq!(discarded, 12, "every foreign verdict was named and dropped");
}

/// Settlement totals for `cum` tokens all filled at `px` (lamports per raw token * 1e9), fee 1% of gross.
fn totals(cum: u64, px: u64) -> (u64, u64) {
    let g = u64::try_from((u128::from(cum) * u128::from(px)).div_ceil(1_000_000_000)).unwrap();
    (g, g / 100)
}

trait EvPx {
    fn ev_px(
        &mut self,
        mint: [u8; 32],
        id: u64,
        k: MgmtKind,
        iss: u64,
        cum: u64,
        px: u64,
    ) -> SellReportResult;
}
impl EvPx for Engine {
    fn ev_px(
        &mut self,
        mint: [u8; 32],
        id: u64,
        k: MgmtKind,
        iss: u64,
        cum: u64,
        px: u64,
    ) -> SellReportResult {
        let (g, f) = totals(cum, px);
        self.model_mgmt_ingest_evidence(mint, id, k, iss, cum, g, f)
    }
}

fn report(id: u64, action: u8, intended: u64, cum: u64, px: u64) -> AppEvent {
    AppEvent::ModelMgmtReport {
        mint: DomainMint::from_bytes(MINT),
        order_id: id,
        action,
        intended,
        cumulative_tokens: cum,
        cumulative_gross: totals(cum, px).0,
        cumulative_fees: totals(cum, px).1,
        terminal: false,
    }
}

#[test]
fn evidence_is_validated_against_the_issued_order_before_anything_changes() {
    let hp = held_path("s_v");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    let b0 = books(&r.e);
    let bad = [
        (
            MgmtKind::Exit,
            intended,
            intended,
            22_000,
            "action_mismatch",
        ),
        (
            MgmtKind::Reduce,
            intended + 1,
            intended,
            22_000,
            "intended_mismatch",
        ),
        (MgmtKind::Reduce, intended, intended, 0, "amounts_invalid"),
    ];
    for (k, iss, cum, px, why) in bad {
        assert_eq!(
            r.e.ev_px(MINT, id, k, iss, cum, px),
            SellReportResult::Rejected(why)
        );
        assert_eq!(books(&r.e), b0, "{why}: nothing changed");
    }
    // A wrong mint never reaches this order, and a never-issued id is named, not applied.
    assert!(matches!(
        r.e.ev_px([0x11; 32], id, MgmtKind::Reduce, intended, intended, 22_000),
        SellReportResult::Rejected(_)
    ));
    assert!(matches!(
        r.e.ev_px(MINT, id + 77, MgmtKind::Reduce, intended, intended, 22_000),
        SellReportResult::Rejected(_)
    ));
    assert_eq!(books(&r.e), b0);
    assert_eq!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
}

#[test]
fn an_equal_quantity_report_with_a_different_price_is_a_named_fault_not_a_duplicate() {
    let hp = held_path("s_p");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    assert!(matches!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, intended, 22_000),
        SellReportResult::Applied { .. }
    ));
    let b = books(&r.e);
    assert_eq!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, intended, 21_000),
        SellReportResult::Fault
    );
    assert_eq!(books(&r.e), b, "no money moved");
    assert_eq!(
        r.e.model_sell_faults().get(&id).map(|f| f.source),
        Some("report_contradicts_settled")
    );
    assert!(r.e.model_mint_is_blocked(&MINT), "new exposure blocked");
}

#[test]
fn an_older_report_is_ignored_only_when_a_booked_fill_proves_it_otherwise_it_is_named() {
    let hp = held_path("s_o");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    let part = intended / 2;
    assert!(matches!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, part, 22_000),
        SellReportResult::Applied { .. }
    ));
    assert!(matches!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, intended, 23_000),
        SellReportResult::Applied { .. }
    ));
    let b = books(&r.e);
    // Proven stale: the same quantity at the same price as a booked fill prefix.
    assert_eq!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, part, 22_000),
        SellReportResult::Duplicate
    );
    // Same older quantity as a booked checkpoint but DIFFERENT amounts: contradictory settlement evidence.
    assert_eq!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, part, 19_000),
        SellReportResult::Fault
    );
    assert!(r.e.model_sell_faults().contains_key(&id));
    // An older quantity that is not a booked prefix is also unproven.
    assert_eq!(
        r.e.ev_px(MINT, id, MgmtKind::Reduce, intended, 3, 22_000),
        SellReportResult::Rejected("stale_unproven")
    );
    assert_eq!(books(&r.e), b, "no report changed any money or inventory");
}

/// The whole inbox, in file order, replayed into an engine (what a restarted daemon does: offset is not persisted).
fn replay_inbox(e: &mut Engine, lines: &[AppEvent]) {
    for l in lines {
        e.tick(*l);
    }
}

#[test]
fn rereading_the_whole_inbox_after_a_crash_neither_loses_nor_double_books_any_report() {
    let hp = held_path("s_x");
    let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    let part = intended / 2;
    let inbox = vec![
        report(id, 0, intended, part, 22_000),
        report(id, 0, intended, intended, 23_000),
    ];
    // Uninterrupted reference.
    let reference = {
        let hp2 = held_path("s_x_ref");
        let mut r2 = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp2);
        r2.advance_to_order(120_000);
        replay_inbox(&mut r2.e, &inbox);
        books(&r2.e)
    };
    // Crash A: report 1 applied but NEVER published (ledger older), then restart + full re-read.
    assert!(r.e.model_held_persist_now());
    replay_inbox(&mut r.e, &inbox[..1]);
    drop(r);
    let mut a = fresh(&hp);
    a.model_held_restore().unwrap().unwrap();
    replay_inbox(&mut a, &inbox);
    assert_eq!(
        books(&a),
        reference,
        "unpublished report is re-applied once, nothing lost"
    );
    // Crash B: both applied AND published, ack lost, restart + full re-read.
    assert!(a.model_held_persist_now());
    drop(a);
    let mut b = fresh(&hp);
    b.model_held_restore().unwrap().unwrap();
    let after_restore = books(&b);
    assert_eq!(after_restore, reference);
    replay_inbox(&mut b, &inbox);
    replay_inbox(&mut b, &inbox);
    assert_eq!(
        books(&b),
        reference,
        "a settled report is never applied twice, however often it is re-read"
    );
}

#[test]
fn execution_evidence_for_an_order_restored_from_an_earlier_process_is_accepted_but_a_foreign_verdict_is_not(
) {
    let hp = held_path("s_y");
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(r.e.model_held_persist_now());
    drop(r);
    // A new process has a new model session; the restored order keeps its execution identity.
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    e2.tick(report(id, 1, intended, intended, 22_000));
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        None,
        "previous-session evidence settled the restored order"
    );
    assert!(e2
        .model_sell_rec(id)
        .is_some_and(|x| x.state == SellState::Completed));
}

/// Two partial fills at DIFFERENT prices and fees, reported as cumulative settlement totals. Each increment books
/// its own proceeds (gross and fee exactly as reported), not quantity times one price. Expected values are
/// computed here independently of the engine.
#[test]
fn two_partial_fills_at_different_prices_and_fees_book_their_own_increments_then_restart_and_replay(
) {
    let hp = held_path("s_x2");
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    assert_eq!(inv0, intended);
    let t1 = intended / 4;
    let v0 = r.e.model_accounting_view(&MINT);
    let (realized0, cost_basis0) = (v0.realized, v0.remaining_cost_basis.unwrap());
    // Fill 1: t1 tokens, gross 1_000_000 lamports, fee 10_000. Fill 2: t2 tokens, gross 3_300_000, fee 99_000.
    let (g1, f1) = (1_000_000_u64, 10_000_u64);
    let t2 = intended / 2;
    let (g2, f2) = (3_300_000_u64, 99_000_u64);
    let (c_g, c_f) = (g1 + g2, f1 + f2);
    assert!(matches!(
        r.e.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, t1, g1, f1),
        SellReportResult::Applied { delta } if delta == t1
    ));
    let realized1 = r.e.model_accounting_view(&MINT).realized;
    assert!(matches!(
        r.e.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, t1 + t2, c_g, c_f),
        SellReportResult::Applied { delta } if delta == t2
    ));
    let fills = r.e.model_mgmt_fills().to_vec();
    assert_eq!(fills.len(), 2);
    assert_eq!(
        (
            fills[0].tokens,
            fills[0].gross_lamports,
            fills[0].fee_lamports
        ),
        (t1, g1, f1)
    );
    assert_eq!(
        (
            fills[1].tokens,
            fills[1].gross_lamports,
            fills[1].fee_lamports
        ),
        (t2, g2, f2)
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - t1 - t2));
    // Independent expectation: each increment realises (its own gross - its own fee) less its pro-rata share of
    // the position's cost basis. frac_bps = floor(tokens * remaining_bps / inventory) (floor: the remainder carries
    // the rounding), with the inventory and remaining fraction as they stood BEFORE that increment.
    let cost0 = cost_basis0;
    let fr1 = u128::from(t1) * 10_000 / u128::from(inv0);
    let rem1 = 10_000 - fr1;
    let fr2 = u128::from(t2) * rem1 / u128::from(inv0 - t1);
    let exp1 = i128::from(g1) - i128::from(f1) - (u128::from(cost0) * fr1 / 10_000) as i128;
    let exp2 = i128::from(g2) - i128::from(f2) - (u128::from(cost0) * fr2 / 10_000) as i128;
    assert_eq!(
        realized1 - realized0,
        exp1,
        "increment 1 books its own gross and fee"
    );
    let realized2 = r.e.model_accounting_view(&MINT).realized;
    assert_eq!(
        realized2 - realized1,
        exp2,
        "increment 2 books ITS gross and fee, not quantity x one price"
    );
    assert!(r.e.model_held_persist_now());
    let b = books(&r.e);
    drop(r);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    assert_eq!(books(&e2), b, "restart restores the same books");
    // Replay the whole report history after the restart: nothing moves.
    for (cum, g, f) in [(t1, g1, f1), (t1 + t2, c_g, c_f), (t1, g1, f1)] {
        assert_eq!(
            e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, cum, g, f),
            SellReportResult::Duplicate
        );
    }
    assert_eq!(books(&e2), b);
    // A contradictory total at an already-booked quantity is a named fault; money still unchanged.
    assert_eq!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, t1 + t2, c_g, c_f + 1),
        SellReportResult::Fault
    );
    assert_eq!(books(&e2), b);
    // Non-monotonic: more tokens but LOWER gross than already applied.
    assert_eq!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, intended, c_g - 1, c_f),
        SellReportResult::Fault
    );
    assert_eq!(books(&e2), b);
}

// ===================== PROTECTIVE ORDER LIFECYCLE =====================
// A protective trigger creates an identifiable order (kind `Protect`) in its own slot; only reconciled fills move
// inventory, cash, cost basis and realized PnL; a management sell on the same mint keeps its own identity.

fn land(e: &mut Engine, from_clock: i64, slot0: u64, n: u64) -> i64 {
    let mut c = from_clock;
    for k in 0..n {
        c += 1_000;
        curve_obs(e, c, slot0 + k, 200_000_000);
        ticks(e, 2);
    }
    c
}

/// A restored world: an UNCERTAIN management sell of `intended` tokens on a held position, ready for a collapse.
fn restored_with_uncertain(kind: &'static str, tag: &str) -> (Engine, u64, u64, u64, i64) {
    let hp = held_path(tag);
    let mut r = rig(
        if kind == "reduce" {
            |s| if s == 0 { REDUCE } else { HOLD }
        } else {
            |s| if s == 0 { EXIT } else { HOLD }
        },
        &hp,
    );
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("order pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(r.e.model_held_persist_now());
    drop(r);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    (e2, id, intended, inv0, T0 + 1_000 + 40 * 2_000 + 400_000)
}

#[test]
fn protection_over_an_uncertain_reduce_sells_only_the_free_part_settles_once_and_a_late_reduce_report_stays_attributable(
) {
    let (mut e, rid, intended, inv0, clock) = restored_with_uncertain("reduce", "p_a");
    let b0 = books(&e);
    hard_collapse(&mut e, clock, 9_000);
    // Trigger -> ONE protective order for exactly the free inventory; an intent moves nothing.
    let (pid, pq, pf, code) = e
        .model_protect_pending_order(&MINT)
        .expect("protective order");
    assert_ne!(pid, rid, "the protective order has its own identity");
    assert_eq!((pq, pf, code), (inv0 - intended, 0, 1));
    assert_eq!(books(&e), b0, "an intent moves no inventory, cash or basis");
    // Repeated triggers while it works: no second order.
    hard_collapse(&mut e, clock + 500, 9_001);
    assert_eq!(e.model_protect_pending_order(&MINT).map(|p| p.0), Some(pid));
    // The paper executor lands it: the free part leaves, the reserved part stays, once.
    land(&mut e, clock + 500, 9_100, 3);
    assert!(
        e.model_protect_pending_order(&MINT).is_none(),
        "protective order completed"
    );
    assert_eq!(e.model_inventory_tokens(&MINT), Some(intended));
    let rec = e.model_sell_rec(pid).expect("settled record");
    assert_eq!(
        (rec.filled, rec.state),
        (inv0 - intended, SellState::Completed)
    );
    let after = books(&e);
    assert!(after.0 != b0.0, "realized moved once");
    // The protective record carries the settlement that moved realized PnL (not zero totals), labelled simulated.
    assert!(rec.simulated && rec.gross > 0 && rec.fees > 0, "{rec:?}");
    let pf = e
        .model_mgmt_fills()
        .iter()
        .rev()
        .find(|x| x.order_id == pid)
        .copied()
        .unwrap();
    assert_eq!((pf.gross_lamports, pf.fee_lamports), (rec.gross, rec.fees));
    // The still-reserved management order is untouched and still attributable: its late report settles it.
    assert_eq!(e.model_mgmt_pending(&MINT).map(|p| p.0), Some(rid));
    assert_eq!(
        e.ev_px(MINT, rid, MgmtKind::Reduce, intended, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert_eq!(
        e.model_inventory_tokens(&MINT),
        None,
        "ends at exactly zero"
    );
    // Duplicate delivery of that report changes nothing.
    let done = books(&e);
    assert_eq!(
        e.ev_px(MINT, rid, MgmtKind::Reduce, intended, intended, 22_000),
        SellReportResult::Duplicate
    );
    assert_eq!(books(&e), done);
}

#[test]
fn a_fully_reserved_position_defers_protection_by_name_without_selling_and_a_definitive_report_re_evaluates(
) {
    let (mut e, rid, intended, inv0, clock) = restored_with_uncertain("exit", "p_b");
    assert_eq!(intended, inv0, "the EXIT reserves everything");
    let b0 = books(&e);
    hard_collapse(&mut e, clock, 9_000);
    // No overlapping sell: no protective order exists, nothing moved, the deferral is named and measured.
    assert!(e.model_protect_pending_order(&MINT).is_none());
    assert_eq!(books(&e), b0);
    assert!(
        e.model_protect_trigger_pending(&MINT),
        "the trigger is remembered, not dropped"
    );
    assert!(e.model_protection_deferred() >= 1);
    let st = e.model_held_data_status();
    let s = st.iter().find(|s| s.mint == MINT).unwrap();
    assert_eq!(
        (s.sell_reserved_tokens, s.inventory_tokens),
        (inv0, Some(inv0))
    );
    assert!(
        pump_quant_app::engine::model_manage::sell_reservation_gap(s).is_some(),
        "the degraded state is measured from the books"
    );
    // Monitoring continues: more ticks keep it deferred, still no sell, still unresolved (never treated as cancelled).
    land(&mut e, clock, 9_100, 3);
    assert_eq!(books(&e), b0);
    assert_eq!(e.model_mgmt_pending(&MINT).map(|p| p.0), Some(rid));
    // The DEFINITIVE report consumes the reservation (the EXIT filled) -> nothing left to protect.
    assert_eq!(
        e.ev_px(MINT, rid, MgmtKind::Exit, intended, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert_eq!(e.model_inventory_tokens(&MINT), None);
    assert!(!e.model_position_open(&MINT));
    assert!(
        !e.model_protect_trigger_pending(&MINT),
        "no exposure, no pending trigger"
    );
    assert!(e.model_protect_pending_order(&MINT).is_none());
}

#[test]
fn releasing_a_reservation_re_evaluates_protection_and_a_still_breached_stop_then_sells() {
    let (mut e, rid, intended, inv0, clock) = restored_with_uncertain("exit", "p_c");
    hard_collapse(&mut e, clock, 9_000);
    assert!(e.model_protect_pending_order(&MINT).is_none());
    // Definitive evidence that the EXIT did NOT execute releases the reservation (it is not guessed or timed out).
    assert!(e.model_mgmt_resolve_uncertain_not_executed(&MINT, rid));
    // Protection is re-evaluated on the next event; the remembered trigger is served for the full free inventory.
    land(&mut e, clock, 9_100, 1);
    let (pid, pq, pf, _) = e.model_protect_pending_order(&MINT).expect("re-evaluated");
    assert_eq!((pq, pf), (inv0, 0));
    assert_ne!(pid, rid);
    assert_eq!(
        e.model_inventory_tokens(&MINT),
        Some(inv0),
        "still no inventory moved by the intent"
    );
    land(&mut e, clock + 1_000, 9_200, 3);
    assert_eq!(e.model_inventory_tokens(&MINT), None);
    let _ = intended;
}

#[test]
fn a_restart_with_a_pending_or_partly_filled_protective_order_neither_resubmits_nor_settles_twice()
{
    // A live (not uncertain) EXIT is the management order; the collapse then creates a protective order while the
    // EXIT is unsubmitted -> the EXIT is ended, the protective order is the only sell.
    let hp = held_path("p_d");
    let mut r = rig(|_| HOLD, &hp);
    for _ in 0..100 {
        r.clock += 1_000;
        r.slot += 1;
        r.n += 1;
        curve_obs(&mut r.e, r.clock, r.slot, 200_000_000);
        print(&mut r.e, r.n, r.clock, r.slot, 45_300 + i128::from(r.n % 7));
        ticks(&mut r.e, 2);
    }
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    hard_collapse(&mut r.e, r.clock + 1_000, r.slot + 5);
    let (pid, pq, _, code) =
        r.e.model_protect_pending_order(&MINT)
            .expect("protective order");
    assert_eq!((pq, code), (inv0, 1));
    assert!(r.e.model_held_persist_now());
    drop(r);
    // CRASH after intent / before any fill: restored UNRESOLVED, same identity and quantity, not resubmitted.
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0));
    let (rid, rq, rf, rcode) = e2.model_protect_pending_order(&MINT).expect("restored");
    assert_eq!((rid, rq, rf, rcode), (pid, inv0, 0, 1));
    let b0 = books(&e2);
    // The restored order is uncertain: the paper executor does NOT fill it, and a repeated trigger makes no 2nd order.
    land(&mut e2, T0 + 1_000 + 40 * 2_000 + 400_000, 9_100, 3);
    hard_collapse(&mut e2, T0 + 1_000 + 40 * 2_000 + 410_000, 9_050);
    assert_eq!(books(&e2), b0, "nothing simulated-filled, nothing invented");
    assert_eq!(
        e2.model_protect_pending_order(&MINT).map(|p| p.0),
        Some(pid)
    );
    // Partial fill reported by the executor (cumulative totals): its increment books once.
    let part = inv0 / 3;
    let (g1, f1) = (2_000_000_u64, 20_000_u64);
    assert!(matches!(
        e2.model_mgmt_ingest_evidence(MINT, pid, MgmtKind::Protect, inv0, part, g1, f1),
        SellReportResult::Applied { delta } if delta == part
    ));
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 - part));
    let mid = books(&e2);
    assert!(e2.model_held_persist_now());
    // CRASH after a partial fill: the remainder stays pending/monitored, the filled part is not re-applied.
    let mut e3 = fresh(&hp);
    e3.model_held_restore().unwrap().unwrap();
    assert_eq!(books(&e3), mid);
    assert_eq!(
        e3.model_protect_pending_order(&MINT)
            .map(|p| (p.0, p.1, p.2)),
        Some((pid, inv0, part))
    );
    // The same cumulative report again: a duplicate, no second credit.
    assert_eq!(
        e3.model_mgmt_ingest_evidence(MINT, pid, MgmtKind::Protect, inv0, part, g1, f1),
        SellReportResult::Duplicate
    );
    assert_eq!(books(&e3), mid);
    // Completion: the rest is reported; exposure ends at exactly zero and nothing resurrects.
    let (g2, f2) = (g1 + 5_000_000, f1 + 50_000);
    assert!(matches!(
        e3.model_mgmt_ingest_evidence(MINT, pid, MgmtKind::Protect, inv0, inv0, g2, f2),
        SellReportResult::Applied { delta } if delta == inv0 - part
    ));
    assert_eq!(e3.model_inventory_tokens(&MINT), None);
    assert!(!e3.model_position_open(&MINT));
    let done = books(&e3);
    assert_eq!(
        e3.model_mgmt_ingest_evidence(MINT, pid, MgmtKind::Protect, inv0, inv0, g2, f2),
        SellReportResult::Duplicate
    );
    assert_eq!(books(&e3), done);
}

// ===================== ADD ACCOUNTING =====================
// CONVENTION: an ADD report states CUMULATIVE tokens acquired, cumulative quote SPENT (fee-EXCLUSIVE notional) and
// cumulative all-in FEES paid ON TOP. All-in cash cost = spent + fees. Cost basis rises by exactly that.

/// A world stopped with an ADD pending (acknowledgement unknown): returns (ledger path, id, intended, max_spend, fee_bps).
fn add_pending_world(tag: &str) -> (std::path::PathBuf, u64, u64, u64, u32) {
    let hp = held_path(tag);
    let mut r = rig(|step| if step == 0 { ADD } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, k, intended, _) = r.e.model_mgmt_pending(&MINT).expect("ADD pending");
    assert_eq!(k, MgmtKind::Add);
    let (max_spend, fee_bps, ..) = r.e.model_mgmt_add_reservation(&MINT).unwrap();
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(r.e.model_held_persist_now());
    (hp, id, intended, max_spend, fee_bps)
}

#[test]
fn a_partial_add_restart_reconcile_books_exact_cash_basis_fees_and_releases_its_reservation() {
    let (hp, id, intended, max_spend, _fee_bps) = add_pending_world("a_p");
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    let v0 = e2.model_accounting_view(&MINT);
    let (c0, committed0) = (v0.remaining_cost_basis.unwrap(), v0.committed);
    // Restored uncertain: reserved, unbooked, never resubmitted.
    let (_, _, spent0, fees0, held_back0) = e2.model_mgmt_add_reservation(&MINT).unwrap();
    assert_eq!((spent0, fees0), (0, 0));
    assert!(
        held_back0 > max_spend,
        "reservation = remaining spend + fee + one fixed leg"
    );
    ticks(&mut e2, 4);
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0));
    // Fill 1: a third of the tokens for a third of the reserved notional; fee 1_234 all-in.
    let t1 = intended / 3;
    let s1 = max_spend / 3;
    let f1 = 1_234u64;
    assert!(matches!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Add, intended, t1, s1, f1),
        SellReportResult::Applied { delta } if delta == t1
    ));
    let v1 = e2.model_accounting_view(&MINT);
    // Independent expectations: tokens add exactly; cost basis and committed capital rise by spent + fees.
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 + t1));
    assert_eq!(v1.remaining_cost_basis.unwrap(), c0 + s1 + f1);
    assert_eq!(v1.committed, committed0 + s1 + f1);
    assert_eq!(v1.realized, v0.realized, "a buy realizes nothing");
    let (_, _, sp1, fe1, held_back1) = e2.model_mgmt_add_reservation(&MINT).unwrap();
    assert_eq!((sp1, fe1), (s1, f1));
    assert!(
        held_back1 < held_back0,
        "the reservation shrinks by what was spent"
    );
    // Restart between the fill and the next report (publication done): books restore exactly.
    assert!(e2.model_held_persist_now());
    let mut e3 = fresh(&hp);
    e3.model_held_restore().unwrap().unwrap();
    assert_eq!(e3.model_inventory_tokens(&MINT), Some(inv0 + t1));
    assert_eq!(
        e3.model_accounting_view(&MINT).remaining_cost_basis,
        v1.remaining_cost_basis
    );
    let (_, _, sp3, fe3, _) = e3.model_mgmt_add_reservation(&MINT).unwrap();
    assert_eq!(
        (sp3, fe3),
        (s1, f1),
        "settlement totals restore with the books"
    );
    // Duplicate and older reports change nothing.
    for (tk, sp, fe) in [(t1, s1, f1), (t1, s1, f1)] {
        assert_eq!(
            e3.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Add, intended, tk, sp, fe),
            SellReportResult::Duplicate
        );
    }
    // Same quantity with different spend or fees is contradictory evidence, named and blocking.
    assert_eq!(
        e3.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Add, intended, t1, s1, f1 + 1),
        SellReportResult::Fault
    );
    assert!(e3.model_mint_is_blocked(&MINT));
    assert_eq!(e3.model_inventory_tokens(&MINT), Some(inv0 + t1));
}

#[test]
fn an_add_that_already_executed_beyond_its_reservation_is_booked_as_executed_faulted_by_name_and_blocks_the_mint(
) {
    let (hp, id, intended, max_spend, fee_bps) = add_pending_world("a_q");
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    let b0 = e2.model_accounting_view(&MINT);
    // FEES above the reserved bound, reported by an executor for an order that already executed.
    let fee_cap = u64::try_from((u128::from(max_spend) * u128::from(fee_bps)).div_ceil(10_000))
        .unwrap()
        + pump_quant_app::exec_quote::landed_leg_cost(10_000);
    // PRE-submission an estimate above the reservation refuses the order (planner tests). POST-execution it is
    // evidence of what happened: the books must show it. Expected values below are computed independently.
    let (tk, sp, fee_hi) = (intended / 2, max_spend / 2, fee_cap + 5_000);
    let committed0 = b0.committed;
    let basis0 = b0.remaining_cost_basis.unwrap();
    assert!(matches!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Add, intended, tk, sp, fee_hi),
        SellReportResult::Applied { delta } if delta == tk
    ));
    let b1 = e2.model_accounting_view(&MINT);
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        Some(inv0 + tk),
        "tokens that were acquired are in inventory"
    );
    assert_eq!(
        b1.committed,
        committed0 + sp + fee_hi,
        "cash: spend plus the ACTUAL fee left the free cash, not the capped fee"
    );
    assert_eq!(
        b1.remaining_cost_basis.unwrap(),
        basis0 + sp + fee_hi,
        "cost basis carries the actual all-in cost"
    );
    let f = e2
        .model_sell_faults()
        .get(&id)
        .expect("overrun is a named fault");
    assert_eq!(f.source, "add_exceeds_reservation");
    assert!(
        e2.model_mint_is_blocked(&MINT),
        "no further exposure while the overrun is unresolved"
    );
    // Durable: the fault survives a restart and keeps blocking (clean books are not reported).
    assert!(e2.model_held_persist_now());
    drop(e2);
    let mut e3 = fresh(&hp);
    e3.model_held_restore().unwrap().unwrap();
    assert_eq!(
        e3.model_sell_faults().get(&id).map(|f| f.source),
        Some("add_exceeds_reservation")
    );
    assert!(e3.model_mint_is_blocked(&MINT));
    assert_eq!(e3.model_inventory_tokens(&MINT), Some(inv0 + tk));
    // Spend above the order's own bound is the same: booked as executed, faulted by name.
    let mut e4 = {
        let (hp4, id4, intended4, max_spend4, _) = add_pending_world("a_q4");
        let mut e = fresh(&hp4);
        e.model_held_restore().unwrap().unwrap();
        let inv = e.model_inventory_tokens(&MINT).unwrap();
        assert!(matches!(
            e.model_mgmt_ingest_evidence(
                MINT,
                id4,
                MgmtKind::Add,
                intended4,
                intended4 / 2,
                max_spend4 + 1,
                10
            ),
            SellReportResult::Applied { .. }
        ));
        assert_eq!(e.model_inventory_tokens(&MINT), Some(inv + intended4 / 2));
        assert_eq!(
            e.model_sell_faults().get(&id4).map(|f| f.source),
            Some("add_exceeds_reservation")
        );
        e
    };
    assert!(e4.model_mint_is_blocked(&MINT));
    ticks(&mut e4, 3);
}

#[test]
fn an_add_within_its_bounds_completes_releases_its_reservation_and_a_larger_duplicate_is_a_named_fault(
) {
    let (hp, id, intended, max_spend, _fee_bps) = add_pending_world("a_q2");
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let inv0 = e2.model_inventory_tokens(&MINT).unwrap();
    let b0 = e2.model_accounting_view(&MINT);
    // The full fill, within bounds, completes it: the order ends, its reservation is gone, the record is terminal.
    let spent = max_spend / 2;
    let fee = 777u64;
    assert!(matches!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Add, intended, intended, spent, fee),
        SellReportResult::Applied { .. }
    ));
    assert!(e2.model_mgmt_pending(&MINT).is_none(), "completed");
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0 + intended));
    let rec = e2.model_sell_rec(id).expect("terminal record");
    assert_eq!(
        (rec.state, rec.filled, rec.spent, rec.fees),
        (SellState::Completed, intended, spent, fee)
    );
    let b1 = e2.model_accounting_view(&MINT);
    assert_eq!(b1.committed, b0.committed + spent + fee);
    // A late duplicate of the completed order changes nothing; a larger one is a named fault.
    assert_eq!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Add, intended, intended, spent, fee),
        SellReportResult::Duplicate
    );
    assert_eq!(e2.model_accounting_view(&MINT), b1);
    assert_eq!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Add, intended, intended, spent + 10, fee),
        SellReportResult::Fault
    );
    assert_eq!(e2.model_accounting_view(&MINT), b1);
}

/// INVARIANT (what makes an inbox byte offset unnecessary): at EVERY durable publication point the persisted
/// settlement totals and the books describe the same state. Checked independently of the engine: tokens filled
/// by the order equal the inventory that left the position, and the order's cumulative gross/fees equal the sum of
/// the increments the books actually booked. Then a re-read from offset zero reaches the uninterrupted final books.
#[test]
fn persisted_settlement_totals_and_books_agree_at_every_publication_point_so_rereading_from_zero_is_exact(
) {
    let hp0 = held_path("s_inv_ref");
    let mut r0 = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp0);
    r0.advance_to_order(120_000);
    let (id, _, intended, _) = r0.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    let inv0 = r0.e.model_inventory_tokens(&MINT).unwrap();
    let (a, b, c) = (intended / 4, intended / 2, intended);
    let inbox = vec![
        report(id, 0, intended, a, 22_000),
        report(id, 0, intended, b, 23_500),
        report(id, 0, intended, c, 21_000),
    ];
    replay_inbox(&mut r0.e, &inbox);
    let reference = books(&r0.e);
    let cum = [a, b, c];
    for k in 0..=inbox.len() {
        let hp = held_path(&format!("s_inv_{k}"));
        let mut r = rig(|step| if step == 0 { REDUCE } else { HOLD }, &hp);
        r.advance_to_order(120_000);
        assert_eq!(r.e.model_mgmt_pending(&MINT).unwrap().0, id);
        replay_inbox(&mut r.e, &inbox[..k]);
        assert!(r.e.model_held_persist_now(), "publication point {k}");
        drop(r);
        let mut e = fresh(&hp);
        e.model_held_restore().unwrap().unwrap();
        // Agreement at the publication point, from the RESTORED state.
        let done = if k == 0 { 0 } else { cum[k - 1] };
        let (g_exp, f_exp) = if k == 0 {
            (0, 0)
        } else {
            totals(done, [22_000, 23_500, 21_000][k - 1])
        };
        // totals() above is for ALL tokens at one price; the report carries those cumulative totals verbatim:
        // The settlement totals as PERSISTED: an unresolved order is a pending record, an ended one a sell record.
        let led = pump_quant_app::held_state::HeldLedger::read(&hp).expect("ledger reads");
        let totals_of = |led: &pump_quant_app::held_state::HeldLedger| {
            led.pending
                .iter()
                .find(|p| p.id == id)
                .map(|p| (p.filled, p.gross, p.fees))
                .or_else(|| {
                    led.sells
                        .iter()
                        .find(|x| x.id == id)
                        .map(|x| (x.filled, x.gross, x.fees))
                })
        };
        if k == 0 {
            assert!(
                totals_of(&led).is_none_or(|t| t == (0, 0, 0)),
                "k=0: nothing settled yet"
            );
        } else {
            assert_eq!(
                totals_of(&led),
                Some((done, g_exp, f_exp)),
                "k={k}: persisted totals are the last report's, verbatim"
            );
        }
        // INVARIANT (stated generally): remaining inventory = prior inventory + reconciled acquisitions
        // - reconciled disposals. Here the order is a sell, so acquisitions = 0; disposals are summed from the fills
        // the engine actually BOOKED (not from the report), and the persisted totals must equal that booked sum.
        let booked = e.model_mgmt_fills().to_vec();
        let disposals: u64 = booked
            .iter()
            .filter(|f| f.order_id == id)
            .map(|f| f.tokens)
            .sum();
        let acquisitions = 0u64;
        // The restored engine has no in-process fills (they are archival); the booked effect is the inventory.
        let left = e.model_inventory_tokens(&MINT);
        let expected_left = (inv0 + acquisitions) - done;
        assert_eq!(
            left,
            if expected_left == 0 { None } else { Some(expected_left) },
            "k={k}: remaining = prior + acquisitions({acquisitions}) - disposals({done}); restored fills seen={}",
            disposals
        );
        // Independent increments: in the uninterrupted reference the booked per-fill gross/fee sum to the totals.
        let ref_fills: Vec<_> =
            r0.e.model_mgmt_fills()
                .iter()
                .filter(|f| f.order_id == id)
                .cloned()
                .collect();
        let n_applied = k;
        let (sg, sf): (u64, u64) = ref_fills.iter().take(n_applied).fold((0, 0), |a, f| {
            (a.0 + f.gross_lamports, a.1 + f.fee_lamports)
        });
        assert_eq!(
            (sg, sf),
            (g_exp, f_exp),
            "k={k}: persisted totals equal the sum of the increments the engine booked"
        );
        // Re-read from offset zero: lands on the uninterrupted books exactly.
        replay_inbox(&mut e, &inbox);
        assert_eq!(books(&e), reference, "k={k}: full re-read is exact");
        replay_inbox(&mut e, &inbox);
        assert_eq!(books(&e), reference, "k={k}: and idempotent");
    }
}

// ===================== HISTORY BEHIND BOOKS =====================
/// What a run exposes that a restart must reproduce: the flow history's aggregates and scope for the held mint at a
/// fixed decision time, the books, the unresolved order and its reservation.
#[derive(Debug, PartialEq)]
struct Obs {
    flow: String,
    scope: Option<(&'static str, i64, i64)>,
    books: (i128, u64, u64, Option<u64>, Option<u64>),
    pending: Option<(u64, u64)>,
    reserved: u64,
}
fn observe(e: &Engine, t_dec: i64) -> Obs {
    Obs {
        flow: format!("{:?}", e.model_flow_aggregates(&MINT, t_dec)),
        scope: e.model_flow_scope_refusal(&MINT, t_dec),
        books: books(e),
        pending: e.model_mgmt_pending(&MINT).map(|(id, _, i, f)| (id, i - f)),
        reserved: e
            .model_held_data_status()
            .iter()
            .find(|s| s.mint == MINT)
            .map_or(0, |s| s.sell_reserved_tokens),
    }
}
/// Quiet feed: more curve observations and prints on the held mint (no verdict needed).
fn feed_more(r: &mut Rig, secs: i64) {
    for _ in 0..secs {
        r.clock += 1_000;
        r.slot += 1;
        r.n += 1;
        curve_obs(&mut r.e, r.clock, r.slot, 200_000_000);
        print(&mut r.e, r.n, r.clock, r.slot, 45_300 + i128::from(r.n % 7));
        ticks(&mut r.e, 1);
    }
}

/// The scenario: history published at P0, then (feed + a PARTIAL REDUCE fill) happen and the BOOKS are published,
/// but the process dies before the next history snapshot. Restart: books restored, history older than the books.
/// Accepted ONLY if replaying the overlap of the feed rebuilds the missing history without touching the books.
fn behind_world(
    tag: &str,
    crash: bool,
) -> (
    Obs,
    Obs,
    Vec<AppEvent>,
    std::path::PathBuf,
    std::path::PathBuf,
    u64,
    i64,
    u64,
) {
    let (hp, fp) = (held_path(tag), held_path(&format!("{tag}_f")));
    let mut r = rig_with(|s| if s == 0 { REDUCE } else { HOLD }, &hp, Some(&fp));
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(r.e.model_held_persist_now());
    assert!(
        r.e.model_flow_flush(Duration::from_secs(5)),
        "P0: history published"
    );
    let t_p0 = r.clock;
    let at_p0 = observe(&r.e, t_p0);
    // Between the generations: more feed, then a partial fill, then the BOOKS are published (history is not).
    feed_more(&mut r, 6);
    let part = intended / 3;
    r.e.tick(report(id, 0, intended, part, 22_000));
    // The remainder's acknowledgement is still unknown: it stays unresolved, never simulated-filled.
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(
        r.e.model_held_persist_now(),
        "books published after the financial effect"
    );
    feed_more(&mut r, 4);
    let t_end = r.clock;
    let end_ref = observe(&r.e, t_end);
    let _ = (crash, t_end);
    let log = feed_log();
    (at_p0, end_ref, log, hp, fp, id, t_p0, part)
}

#[test]
fn history_behind_books_is_rebuilt_by_overlap_replay_without_reapplying_fills_or_changing_earlier_decisions(
) {
    // Uninterrupted reference.
    let (p0_ref, end_ref, log, hp, fp, id, t_p0, intended_part) = behind_world("hb_ref", false);
    let gen_books = pump_quant_app::held_state::HeldLedger::read(&hp)
        .unwrap()
        .generation;
    // The crash happened before the last 4 s of feed was ever snapshotted: the history file on disk is the P0 one.
    let seen = match pump_quant_app::flow_checkpoint::load(
        pump_quant_market_state::flow_reducer::FlowParams::default(),
        &fp,
    ) {
        pump_quant_app::flow_checkpoint::Load::Loaded(h) => h.meta.held_gen_seen.expect("bound"),
        _ => panic!("history on disk"),
    };
    assert!(
        seen < gen_books,
        "history is BEHIND the books: history saw generation {seen}, books are at {gen_books}"
    );
    // RESTART from exactly those two files.
    let mut e = fresh(&hp);
    e.model_held_restore().unwrap().unwrap();
    let attach = e.model_flow_attach(
        &fp,
        pump_quant_market_state::flow_reducer::FlowParams::default(),
        flow_prov(),
        0,
    );
    assert!(
        matches!(
            attach,
            pump_quant_app::engine::model_admit::FlowAttach::Restored { .. }
        ),
        "{attach:?}"
    );
    let t_end = log
        .iter()
        .filter_map(|ev| match ev {
            AppEvent::MarketTrade {
                recv_unix_ms: Some(t),
                ..
            } => Some(*t),
            _ => None,
        })
        .max()
        .unwrap();
    let before_replay = observe(&e, t_end);
    assert_ne!(
        before_replay.flow, end_ref.flow,
        "the restored history is genuinely missing the post-P0 interval (the test can fail)"
    );
    // Books restored exactly as published (the partial fill is in them); reservation intact.
    assert_eq!(before_replay.books, end_ref.books);
    // The reservation is derived from the order book on each tick: it must equal the unfilled remainder after one.
    ticks(&mut e, 1);
    assert_eq!(
        observe(&e, t_end).reserved,
        end_ref.reserved,
        "restored reservation = uninterrupted reservation, before any replay"
    );
    // OVERLAP REPLAY of the feed (observations only: no reports, no fills).
    for ev in &log {
        e.tick(*ev);
    }
    ticks(&mut e, 2);
    let after = observe(&e, t_end);
    assert_eq!(
        after.flow, end_ref.flow,
        "history rebuilt: same aggregates as uninterrupted"
    );
    assert_eq!(
        after.books, end_ref.books,
        "no fill re-applied, no cash or inventory moved"
    );
    assert_eq!(
        after.pending, end_ref.pending,
        "the remaining order is unchanged"
    );
    // The restored order is still pending with exactly the unfilled remainder: nothing re-applied the partial fill.
    let (pid, _, pint, pfill) = e.model_mgmt_pending(&MINT).expect("still pending");
    assert_eq!((pid, pfill), (id, intended_part));
    assert_eq!(end_ref.pending.unwrap().1, pint - pfill);
    assert_eq!(
        end_ref.reserved,
        pint - pfill,
        "reservation = unfilled remainder"
    );
    // EARLIER decisions are unchanged: the history read at P0's decision time is what it was uninterrupted
    // (replay re-delivered only events the history already held or newer ones; none was folded into the past).
    assert_eq!(
        observe(&e, t_p0).flow,
        p0_ref.flow,
        "an earlier decision reads the same history after the replay as it did before the crash"
    );
}

#[test]
fn if_the_overlap_cannot_be_replayed_the_history_stays_refused_by_name_and_the_books_are_untouched()
{
    let (_p0, end_ref, log, hp, fp, id, _t_p0, intended_part) = behind_world("hb_gap", false);
    let mut e = fresh(&hp);
    e.model_held_restore().unwrap().unwrap();
    // The process resumes far later than the snapshot (declared resume clock): a named gap, not a bridge.
    let resume = log
        .iter()
        .filter_map(|ev| match ev {
            AppEvent::MarketTrade {
                recv_unix_ms: Some(t),
                ..
            } => Some(*t),
            _ => None,
        })
        .max()
        .unwrap()
        + 600_000;
    let a = e.model_flow_attach(
        &fp,
        pump_quant_market_state::flow_reducer::FlowParams::default(),
        flow_prov(),
        resume,
    );
    assert!(
        matches!(
            a,
            pump_quant_app::engine::model_admit::FlowAttach::Restored {
                complete: false,
                ..
            }
        ),
        "{a:?}"
    );
    // Only the LATER part of the feed arrives (the interval right after the snapshot is not replayable).
    for ev in log.iter().rev().take(4).rev() {
        e.tick(*ev);
    }
    ticks(&mut e, 2);
    assert_eq!(
        e.model_flow_scope_refusal(&MINT, resume + 1).map(|x| x.0),
        Some("feed_gap"),
        "readiness stays refused by name while a needed interval is missing"
    );
    // Refusing readiness never touched the money or the order.
    assert_eq!(books(&e).4, end_ref.books.4);
    assert_eq!(
        e.model_mgmt_pending(&MINT).map(|(i, _, _, f)| (i, f)),
        Some((id, intended_part))
    );
}

#[test]
fn a_history_file_with_no_generation_field_cannot_be_tied_to_restored_books_and_is_refused() {
    let (_p0, _end, _log, hp, fp, _id, _t, _p) = behind_world("hb_g0", false);
    // Rewrite the history header as a pre-generation file: drop the field, keep everything else byte-valid.
    let raw = std::fs::read(&fp).unwrap();
    let nl = raw.iter().position(|b| *b == b'\n').unwrap();
    let mut hdr: serde_json::Value = serde_json::from_slice(&raw[..nl]).unwrap();
    assert!(hdr
        .as_object_mut()
        .unwrap()
        .remove("held_gen_seen")
        .is_some());
    let mut out = serde_json::to_vec(&hdr).unwrap();
    out.push(b'\n');
    out.extend_from_slice(&raw[nl + 1..]);
    std::fs::write(&fp, &out).unwrap();
    let mut e = fresh(&hp);
    e.model_held_restore().unwrap().unwrap();
    let a = e.model_flow_attach(
        &fp,
        pump_quant_market_state::flow_reducer::FlowParams::default(),
        flow_prov(),
        0,
    );
    assert!(
        matches!(
            a,
            pump_quant_app::engine::model_admit::FlowAttach::Untrusted("flow_generation_unbound")
        ),
        "{a:?}"
    );
    assert_eq!(
        std::fs::read(&fp).unwrap(),
        out,
        "the file is left as evidence"
    );
    // A genuinely clean start (no books file) with a legacy history is not blocked by this rule.
    let hp2 = held_path("hb_g0_clean");
    let mut e2 = fresh(&hp2);
    assert!(matches!(e2.model_held_restore(), Ok(None)));
    let a2 = e2.model_flow_attach(
        &fp,
        pump_quant_market_state::flow_reducer::FlowParams::default(),
        flow_prov(),
        0,
    );
    assert!(
        !matches!(
            a2,
            pump_quant_app::engine::model_admit::FlowAttach::Untrusted("flow_generation_unbound")
        ),
        "{a2:?}"
    );
}

// ===================== PAPER EXECUTOR SETTLEMENT =====================
/// Independent model of one simulated sell, from the DURABLE pre-fill position (test config: OptimisticCeiling,
/// so no impairment; curve_exact_fill off, so the fill price is the reserve-walk average and no extra impact).
/// Returns (fill price, gross, all-in fee, pro-rata cost, frac_bps).
fn expected_sim_sell(
    h: &pump_quant_app::held_state::HeldEntry,
    vsol: u64,
    vtok: u64,
    tokens: u64,
) -> (u64, u64, u64, u64, u32) {
    // M3 expectation, computed HERE (not via the engine's quote module): the pump.fun curve sell for exactly
    // `tokens` at the landing reserves - gross = floor(tokens * vsol / (vtok + tokens)) - minus protocol 95 bp and
    // creator 30 bp, each ceil-rounded (validated on independent mainnet sells), then one landed leg = measured
    // p50 network fee 10_000 + the configured exit tip 10_000. The fill price is venue-net per token.
    let inv = h.inventory_tokens.unwrap();
    let gross = u128::from(tokens) * u128::from(vsol) / (u128::from(vtok) + u128::from(tokens));
    let ceil = |bps: u128| (gross * bps).div_ceil(10_000);
    let venue = ceil(95) + ceil(30);
    let net = gross - venue;
    let px = u64::try_from((net * 1_000_000_000).div_ceil(u128::from(tokens))).unwrap();
    let frac = if tokens == inv {
        h.remaining_bps
    } else {
        u32::try_from(u128::from(tokens) * u128::from(h.remaining_bps) / u128::from(inv)).unwrap()
    };
    let fee = venue + 10_000 + 10_000;
    let cost = u128::from(h.cost_lamports) * u128::from(frac) / 10_000;
    (px, gross as u64, fee as u64, cost as u64, frac)
}

/// Drive one REDUCE order to its paper fill at a landing observation with `dsol`, from a persisted pre-fill
/// state. Returns (order id, tokens, expected (px, gross, fee, cost, frac)).
fn sim_reduce_fill(
    r: &mut Rig,
    hp: &std::path::Path,
    dsol: u64,
) -> (u64, u64, (u64, u64, u64, u64, u32)) {
    r.advance_to_order(120_000);
    let (id, k, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    assert_eq!((k, filled), (MgmtKind::Reduce, 0));
    assert!(r.e.model_held_persist_now());
    let led = pump_quant_app::held_state::HeldLedger::read(hp).unwrap();
    let h = led.held.iter().find(|h| h.mint == MINT).unwrap().clone();
    let (vsol, vtok) = (VSOL + dsol, VTOK - 4_000_000_000_000);
    let exp = expected_sim_sell(&h, vsol, vtok, intended);
    // The landing observation: 1 s after creation, a newer slot (meets the existing 400 ms / newer-slot rule).
    r.clock += 1_000;
    r.slot += 1;
    curve_obs(&mut r.e, r.clock, r.slot, dsol);
    ticks(&mut r.e, 3);
    assert!(
        r.e.model_mgmt_pending(&MINT).is_none(),
        "paper fill completed the order: {:?}",
        r.e.model_lane_report()
    );
    (id, intended, exp)
}

/// The PAPER EXECUTOR books through the cumulative-settlement path. Two simulated disposals at DIFFERENT prices
/// (two REDUCE orders, each filled whole by the paper executor: the position is partially sold each time), then a
/// restart and duplicate replay. Every number is computed here from the durable pre-fill position.
#[test]
fn paper_fills_record_the_cumulative_settlement_that_moved_cash_and_replay_settles_nothing_twice() {
    let hp = held_path("s_sim");
    let mut r = rig(|s| if s <= 1 { REDUCE } else { HOLD }, &hp);
    let mut prev = r.e.model_accounting_view(&MINT);
    let mut fills = Vec::new();
    for dsol in [200_000_000u64, 900_000_000] {
        let inv_before = r.e.model_inventory_tokens(&MINT).unwrap();
        let prev_bps = {
            assert!(r.e.model_held_persist_now());
            pump_quant_app::held_state::HeldLedger::read(&hp)
                .unwrap()
                .held
                .iter()
                .find(|h| h.mint == MINT)
                .unwrap()
                .remaining_bps
        };
        let entry_spend_before = prev.attribution_entry_spend.unwrap();
        let (id, tokens, (px, g, f, cost, _frac)) = sim_reduce_fill(&mut r, &hp, dsol);
        let v = r.e.model_accounting_view(&MINT);
        let rel = u64::try_from(
            u128::from(entry_spend_before) * u128::from(tokens) / u128::from(inv_before),
        )
        .unwrap();
        assert_eq!(
            v.realized - prev.realized,
            i128::from(g) - i128::from(f) - i128::from(cost),
            "realized = gross - fees - pro-rata cost"
        );
        assert_eq!(
            v.balance,
            prev.balance
                .wrapping_add_signed(i64::try_from(v.realized - prev.realized).unwrap()),
            "cash moves by exactly the booked net"
        );
        assert_eq!(
            prev.committed - v.committed,
            rel,
            "committed entry spend released pro rata"
        );
        assert_eq!(
            v.inventory_tokens,
            Some(inv_before - tokens),
            "remaining = prior - reconciled disposal"
        );
        // The view is floor(cost * remaining_bps / 1e4) of the DURABLE position after the fill (its own rounding);
        // the tranche cost charged to realized PnL is checked exactly above.
        let led_after = {
            assert!(r.e.model_held_persist_now());
            pump_quant_app::held_state::HeldLedger::read(&hp).unwrap()
        };
        let ha = led_after.held.iter().find(|h| h.mint == MINT).unwrap();
        assert_eq!(
            v.remaining_cost_basis.unwrap(),
            u64::try_from(u128::from(ha.cost_lamports) * u128::from(ha.remaining_bps) / 10_000)
                .unwrap()
        );
        assert_eq!(
            ha.remaining_bps,
            prev_bps - _frac,
            "remaining fraction falls by exactly the sold share"
        );
        // The RECORD is the evidence that moved cash: cumulative totals equal the booked amounts, labelled simulated.
        let rec = r.e.model_sell_rec(id).expect("terminal record");
        assert_eq!(
            (rec.filled, rec.gross, rec.fees, rec.simulated, rec.state),
            (tokens, g, f, true, SellState::Completed)
        );
        let fill =
            r.e.model_mgmt_fills()
                .iter()
                .rev()
                .find(|x| x.order_id == id)
                .copied()
                .unwrap();
        assert_eq!(
            (fill.tokens, fill.gross_lamports, fill.fee_lamports),
            (tokens, g, f)
        );
        assert!(f > 0, "modelled fee is never silently zero");
        fills.push((id, tokens, g, f, px));
        prev = v;
    }
    assert_ne!(fills[0].4, fills[1].4, "two different fill prices");
    assert!(
        r.e.model_assessable_fills().is_empty(),
        "simulated economics stay outside assessable PnL"
    );
    // Restart: the durable totals and books come back exactly; replaying each order's totals settles nothing.
    let end = books(&r.e);
    assert!(r.e.model_held_persist_now());
    drop(r);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    assert_eq!(books(&e2), end);
    for &(id, tokens, g, f, _) in &fills {
        let rec = e2.model_sell_rec(id).unwrap();
        assert_eq!(
            (rec.filled, rec.gross, rec.fees, rec.simulated),
            (tokens, g, f, true),
            "persisted verbatim"
        );
        for _ in 0..2 {
            assert_eq!(
                e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Reduce, tokens, tokens, g, f),
                SellReportResult::Duplicate
            );
        }
        // Same quantity, different amounts: contradictory evidence, named.
        assert_eq!(
            e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Reduce, tokens, tokens, g + 1, f),
            SellReportResult::Fault
        );
    }
    let after = books(&e2);
    assert_eq!(
        (after.0, after.1, after.2, after.3, after.4),
        (end.0, end.1, end.2, end.3, end.4),
        "no replay moved money or inventory"
    );
}

/// An order the external executor has PARTIALLY filled is still working there: its remainder stays reserved, the paper
/// executor never fills it, and a protective trigger sells only inventory outside it (never ending it as "unsubmitted").
#[test]
fn a_partially_reported_reduce_keeps_its_remainder_reserved_and_protection_never_overlaps_it() {
    let (mut e, rid, intended, inv0, clock) = restored_with_uncertain("reduce", "p_part");
    let part = intended / 3;
    assert!(matches!(
        e.ev_px(MINT, rid, MgmtKind::Reduce, intended, part, 22_000),
        SellReportResult::Applied { delta } if delta == part
    ));
    let inv1 = inv0 - part;
    assert_eq!(e.model_inventory_tokens(&MINT), Some(inv1));
    ticks(&mut e, 2);
    let st = e
        .model_held_data_status()
        .into_iter()
        .find(|s| s.mint == MINT)
        .unwrap();
    assert_eq!(
        st.sell_reserved_tokens,
        intended - part,
        "the unfilled remainder stays reserved"
    );
    // A landing observation must not paper-fill an externally working order.
    land(&mut e, clock + 500, 9_100, 3);
    assert_eq!(
        e.model_mgmt_pending(&MINT).map(|p| (p.0, p.3)),
        Some((rid, part)),
        "still working, not paper-filled"
    );
    assert_eq!(e.model_inventory_tokens(&MINT), Some(inv1));
    // Protection fires: only the free part is sold; the REDUCE survives.
    hard_collapse(&mut e, clock + 2_000, 9_200);
    let (pid, pq, _, _) = e
        .model_protect_pending_order(&MINT)
        .expect("protective order for the free part");
    assert_ne!(pid, rid);
    assert_eq!(
        pq,
        inv1 - (intended - part),
        "free = inventory - reserved remainder"
    );
    assert_eq!(
        e.model_mgmt_pending(&MINT).map(|p| p.0),
        Some(rid),
        "the working REDUCE is never ended as unsubmitted"
    );
}

/// Harness external-execution mode: a new REDUCE/EXIT is SUBMITTED unresolved (reserved, never paper-filled), and a
/// protective order created while the model's order is unresolved sells only the free part, also unresolved.
#[test]
fn external_execution_submits_unresolved_orders_that_only_reports_can_settle() {
    let hp = held_path("x_ext");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    r.e.model_set_external_execution(true);
    r.advance_to_order(120_000);
    let (id, k, intended, filled, unc) =
        r.e.model_mgmt_order_status(&MINT).expect("EXIT submitted");
    assert_eq!(
        (k, filled, unc),
        (MgmtKind::Exit, 0, true),
        "submitted, unresolved"
    );
    let b0 = books(&r.e);
    // Landing observations that WOULD fill a paper order: nothing fills, the whole order stays reserved.
    for _ in 0..8 {
        r.clock += 1_000;
        r.slot += 1;
        curve_obs(&mut r.e, r.clock, r.slot, 200_000_000);
        ticks(&mut r.e, 2);
    }
    assert_eq!(books(&r.e), b0, "never paper-filled");
    assert_eq!(r.e.model_mgmt_order_status(&MINT).map(|s| s.3), Some(0));
    let st =
        r.e.model_held_data_status()
            .into_iter()
            .find(|s| s.mint == MINT)
            .unwrap();
    assert_eq!(
        st.sell_reserved_tokens, intended,
        "the full EXIT reserves the whole remainder"
    );
    // The external report settles it, once.
    assert!(matches!(
        r.e.ev_px(MINT, id, MgmtKind::Exit, intended, intended, 22_000),
        SellReportResult::Applied { .. }
    ));
    assert!(!r.e.model_position_open(&MINT));
}

/// Two unresolved sells on one mint (a management REDUCE and a protective order) are SUMMED into the reservation:
/// neither may overwrite the other, and outstanding sell commitments never exceed reconciled inventory.
#[test]
fn an_unresolved_reduce_and_an_unresolved_protective_order_reserve_their_sum() {
    let hp = held_path("x_sum");
    let mut r = rig(|s| if s == 0 { REDUCE } else { HOLD }, &hp);
    r.e.model_set_external_execution(true);
    r.advance_to_order(120_000);
    let (rid, _, rint, _, unc) =
        r.e.model_mgmt_order_status(&MINT)
            .expect("REDUCE submitted");
    assert!(unc);
    // A partial external fill of the REDUCE makes the two remainders DIFFERENT sizes (the defect hid when equal).
    let part = rint / 4;
    assert!(matches!(
        r.e.ev_px(MINT, rid, MgmtKind::Reduce, rint, part, 22_000),
        SellReportResult::Applied { .. }
    ));
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    hard_collapse(&mut r.e, r.clock + 1_000, r.slot + 5);
    let (pid, pint, pfill, _) =
        r.e.model_protect_pending_order(&MINT)
            .expect("protective order");
    assert_ne!(pid, rid);
    let rrem = rint - part;
    assert_eq!(pint, inv - rrem, "protection sells only the free part");
    ticks(&mut r.e, 2);
    let st =
        r.e.model_held_data_status()
            .into_iter()
            .find(|s| s.mint == MINT)
            .unwrap();
    assert_eq!(
        st.sell_reserved_tokens,
        rrem + (pint - pfill),
        "reservation = REDUCE remainder + protective remainder"
    );
    assert!(
        st.sell_reserved_tokens <= inv,
        "outstanding commitments never exceed reconciled inventory"
    );
}

/// The reservation describes the book AFTER each event: a report that settles the management REDUCE leaves only the
/// protective remainder reserved, in that same tick (no later tick is needed to correct it).
#[test]
fn the_reservation_reflects_a_settling_report_in_the_same_tick() {
    let hp = held_path("x_same");
    let mut r = rig(|s| if s == 0 { REDUCE } else { HOLD }, &hp);
    r.e.model_set_external_execution(true);
    r.advance_to_order(120_000);
    let (rid, _, rint, _, _) =
        r.e.model_mgmt_order_status(&MINT)
            .expect("REDUCE submitted");
    let part = rint / 4;
    assert!(matches!(
        r.e.ev_px(MINT, rid, MgmtKind::Reduce, rint, part, 22_000),
        SellReportResult::Applied { .. }
    ));
    hard_collapse(&mut r.e, r.clock + 1_000, r.slot + 5);
    let (_, pint, pfill, _) =
        r.e.model_protect_pending_order(&MINT)
            .expect("protective order");
    // The REDUCE completes through the event path (one tick), nothing else happens afterwards.
    r.e.tick(report(rid, 0, rint, rint, 22_000));
    let st =
        r.e.model_held_data_status()
            .into_iter()
            .find(|s| s.mint == MINT)
            .unwrap();
    assert!(
        r.e.model_mgmt_order_status(&MINT).is_none(),
        "REDUCE settled"
    );
    assert_eq!(
        st.sell_reserved_tokens,
        pint - pfill,
        "only the protective remainder is reserved, immediately"
    );
}

// ===================== TERMINAL EXECUTION EVIDENCE =====================
fn term(id: u64, action: u8, intended: u64, cum: u64, px: u64) -> AppEvent {
    let (g, f) = if cum == 0 { (0, 0) } else { totals(cum, px) };
    AppEvent::ModelMgmtReport {
        mint: DomainMint::from_bytes(MINT),
        order_id: id,
        action,
        intended,
        cumulative_tokens: cum,
        cumulative_gross: g,
        cumulative_fees: f,
        terminal: true,
    }
}
fn reserved(e: &Engine) -> u64 {
    e.model_held_data_status()
        .into_iter()
        .find(|s| s.mint == MINT)
        .map_or(0, |s| s.sell_reserved_tokens)
}

/// DEFINITIVELY NO EXECUTION: a fully reserved EXIT with a deferred protective trigger. Only terminal evidence with
/// zero totals releases the reservation; protection re-evaluates in that same event and sells the now-free inventory.
/// A duplicate terminal report is a no-op; later evidence claiming execution is a durable fault; all survive restart.
#[test]
fn terminal_no_execution_releases_the_reservation_and_protection_re_evaluates_immediately() {
    let (mut e, rid, intended, inv0, clock) = restored_with_uncertain("exit", "t_none");
    hard_collapse(&mut e, clock, 9_000);
    assert!(e.model_protect_pending_order(&MINT).is_none(), "deferred");
    let b0 = books(&e);
    // A NON-terminal zero report proves nothing: still reserved, still deferred.
    e.tick(report(rid, 1, intended, 0, 22_000));
    assert_eq!(reserved(&e), intended);
    assert!(e.model_protect_pending_order(&MINT).is_none());
    // Wrong identity terminal evidence is refused by the existing checks; nothing released.
    e.tick(term(rid, 0, intended, 0, 22_000)); // action mismatch (reduce vs exit)
    e.tick(term(rid, 1, intended + 1, 0, 22_000)); // intended mismatch
    assert_eq!(reserved(&e), intended);
    assert_eq!(e.model_mgmt_pending(&MINT).map(|p| p.0), Some(rid));
    // Authoritative terminal: nothing executed.
    e.tick(term(rid, 1, intended, 0, 22_000));
    assert!(e.model_mgmt_pending(&MINT).is_none(), "EXIT ended");
    let rec = e.model_sell_rec(rid).expect("terminal record");
    assert_eq!(rec.state, SellState::EndedUnfilled);
    assert_eq!((rec.filled, rec.gross, rec.fees), (0, 0, 0));
    assert!(
        e.model_sell_faults().is_empty(),
        "definitive non-execution is not a fault"
    );
    assert_eq!(books(&e), b0, "no execution moves nothing");
    // Protection re-evaluated in the SAME event: a protective order for the whole (now free) inventory.
    let (pid, pq, pf, _) = e.model_protect_pending_order(&MINT).expect("re-evaluated");
    assert_ne!(pid, rid);
    assert_eq!((pq, pf), (inv0, 0));
    // EXIT reservation released. This paper rig holds the protective order in the simulator (not externally
    // submitted), so it reserves nothing; the externally-submitted case is exercised through the daemon.
    assert_eq!(reserved(&e), 0, "the EXIT reservation is released");
    // Duplicate terminal: no-op.
    let snap = (
        books(&e),
        reserved(&e),
        e.model_protect_pending_order(&MINT),
    );
    e.tick(term(rid, 1, intended, 0, 22_000));
    assert_eq!(
        (
            books(&e),
            reserved(&e),
            e.model_protect_pending_order(&MINT)
        ),
        snap
    );
    assert!(e.model_sell_faults().is_empty());
    // Contradictory later evidence (it DID execute): durable fault, nothing applied.
    e.tick(report(rid, 1, intended, intended / 2, 22_000));
    assert_eq!(books(&e), snap.0, "contradiction applies nothing");
    let f = e.model_sell_faults().get(&rid).expect("fault");
    assert_eq!(f.source, "report_contradicts_settled");
    // Restart: the terminal record, the fault and the protective order all restore.
    assert!(e.model_held_persist_now());
    // The SAME file (held_path would wipe the directory).
    let hp = std::env::temp_dir()
        .join(format!("pq_sell_t_none_{}", std::process::id()))
        .join("held.json");
    drop(e);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    assert_eq!(
        e2.model_sell_rec(rid).map(|r| r.state),
        Some(SellState::EndedUnfilled)
    );
    assert!(e2.model_sell_faults().contains_key(&rid), "fault persists");
    assert_eq!(
        e2.model_protect_pending_order(&MINT).map(|p| (p.0, p.1)),
        Some((pid, pq))
    );
    assert_eq!(books(&e2), snap.0);
    // Duplicate terminal after restart: still a no-op.
    e2.tick(term(rid, 1, intended, 0, 22_000));
    assert_eq!(books(&e2), snap.0);
}

/// PARTIAL EXECUTION + DEFINITIVELY CANCELLED REMAINDER, and EXECUTION STILL UNKNOWN, kept apart. A partial
/// non-terminal report keeps the remainder working (reserved). The terminal report at the SAME totals preserves the
/// prior partial settlement exactly and releases only the remainder; protection then sells the free part.
#[test]
fn terminal_partial_preserves_prior_settlement_and_releases_only_the_cancelled_remainder() {
    let (mut e, rid, intended, inv0, clock) = restored_with_uncertain("exit", "t_part");
    hard_collapse(&mut e, clock, 9_000);
    assert!(e.model_protect_pending_order(&MINT).is_none(), "deferred");
    let part = intended / 3;
    // Execution still unknown for the remainder: partial non-terminal report.
    e.tick(report(rid, 1, intended, part, 22_000));
    let after_partial = books(&e);
    assert_eq!(after_partial.4, Some(inv0 - part), "partial settled");
    assert_eq!(
        reserved(&e),
        intended - part,
        "remainder still reserved (unknown)"
    );
    assert!(
        e.model_protect_pending_order(&MINT).is_none(),
        "still fully reserved"
    );
    // Terminal at a DIFFERENT (larger) total is new execution: settles the increment, then ends. Use the same
    // totals here: the remainder is definitively cancelled; nothing else moves.
    e.tick(term(rid, 1, intended, part, 22_000));
    assert_eq!(
        books(&e),
        after_partial,
        "prior partial settlement preserved, nothing re-applied"
    );
    let rec = e.model_sell_rec(rid).expect("record");
    assert_eq!(rec.state, SellState::EndedPartial);
    assert_eq!(
        (rec.filled, rec.gross, rec.fees),
        (part, totals(part, 22_000).0, totals(part, 22_000).1)
    );
    assert!(e.model_sell_faults().is_empty());
    let (_, pq, _, _) = e.model_protect_pending_order(&MINT).expect("re-evaluated");
    assert_eq!(pq, inv0 - part, "protection sells the released inventory");
    // Duplicate terminal: no-op. Terminal with different totals after the end: fault.
    let snap = books(&e);
    e.tick(term(rid, 1, intended, part, 22_000));
    assert!(e.model_sell_faults().is_empty());
    e.tick(term(rid, 1, intended, part + 1, 22_000));
    assert_eq!(books(&e), snap);
    assert_eq!(
        e.model_sell_faults().get(&rid).map(|f| f.source),
        Some("report_contradicts_settled")
    );
}

/// A terminal report that ALSO carries new execution settles the increment through the normal path first, then
/// cancels the rest; terminal zero over an order the books already partly filled is a contradiction (fault).
#[test]
fn terminal_with_new_execution_settles_it_first_and_terminal_zero_over_a_fill_is_a_fault() {
    let (mut e, rid, intended, inv0, clock) = restored_with_uncertain("exit", "t_new");
    let _ = clock;
    let q = intended / 4;
    e.tick(term(rid, 1, intended, q, 22_000));
    assert_eq!(e.model_inventory_tokens(&MINT), Some(inv0 - q));
    assert_eq!(
        e.model_sell_rec(rid).map(|r| (r.state, r.filled)),
        Some((SellState::EndedPartial, q))
    );
    assert_eq!(reserved(&e), 0, "nothing reserved after the definitive end");
    // Second world: books filled q, then terminal ZERO -> contradiction.
    let (mut e2, rid2, int2, _inv, _c) = restored_with_uncertain("exit", "t_zero");
    e2.tick(report(rid2, 1, int2, q, 22_000));
    let b = books(&e2);
    e2.tick(term(rid2, 1, int2, 0, 22_000));
    assert_eq!(books(&e2), b);
    assert_eq!(
        e2.model_sell_faults().get(&rid2).map(|f| f.source),
        Some("report_contradicts_settled")
    );
    assert_eq!(reserved(&e2), int2 - q, "a fault releases nothing");
}

// ---- M1: end-of-run report() preserves model-owned exposure (ownership, not arming, decides) ----

/// Everything a report() must leave unchanged for a model-owned position.
type M1State = (
    bool,
    (i128, u64, u64, Option<u64>, Option<u64>),
    u64,
    usize,
    usize,
);

fn m1_state(e: &Engine) -> M1State {
    (
        e.model_position_open(&MINT),
        books(e),
        reserved(e),
        e.model_assessable_fills().len(),
        e.model_excluded_exits().len(),
    )
}

fn held_doc(p: &std::path::Path) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    serde_json::json!({"held": v["held"], "pending": v["pending"], "orders": v["orders"],
        "sells": v["sells"], "realized": v["realized_lamports"]})
}

#[test]
fn m1_report_twice_with_an_unresolved_sell_changes_no_books_and_the_ledger_restores_unchanged() {
    let (mut e, id, intended, inv0, _clock) = restored_with_uncertain("reduce", "m1_a");
    let hp = held_path("m1_a_out");
    e.model_held_attach(&hp);
    assert!(e.model_held_persist_now());
    let s0 = m1_state(&e);
    let doc0 = held_doc(&hp);
    let r1 = e.report();
    let r2 = e.report();
    assert_eq!(
        m1_state(&e),
        s0,
        "report() twice: no inventory/cash/basis/reservation/terminal change"
    );
    assert_eq!(
        (r1.net_lamports, r2.net_lamports),
        (0, 0),
        "no realized result for unsold exposure"
    );
    assert_eq!(
        e.model_mgmt_pending(&MINT).map(|p| (p.0, p.2)),
        Some((id, intended)),
        "sell stays unresolved"
    );
    assert!(e.model_held_persist_now());
    assert_eq!(
        held_doc(&hp),
        doc0,
        "durable exposure unchanged by report()"
    );
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv0));
    assert_eq!(books(&e2), s0.1, "restart restores the same books");
    let x = e.model_open_exposure();
    assert_eq!(x.len(), 1);
    assert_eq!(
        (x[0].inventory_tokens, x[0].sell_reserved_tokens),
        (Some(inv0), reserved(&e)),
        "the exposure view states the same reservation the held-data status does"
    );
}

#[test]
fn m1_safety_off_tripped_then_report_preserves_exposure() {
    let (mut e, _id, _intended, inv0, _clock) = restored_with_uncertain("exit", "m1_b");
    e.model_safety_trip("m1_test");
    assert!(e.model_safety_blocked());
    let s0 = m1_state(&e);
    let _ = e.report();
    let _ = e.report();
    assert_eq!(m1_state(&e), s0);
    assert_eq!(e.model_inventory_tokens(&MINT), Some(inv0));
}

#[test]
fn m1_inference_disarmed_engine_restoring_model_exposure_never_force_closes_it() {
    let hp = held_path("m1_c");
    let mut r = rig(|_| HOLD, &hp);
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    assert!(r.e.model_held_persist_now());
    let b0 = books(&r.e);
    drop(r);
    // A restart WITHOUT the model lane armed: ownership travels with the record, not with arming.
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.model_held_attach(&hp);
    e.model_held_restore().unwrap().unwrap();
    assert!(!e.paper_model_enabled());
    assert_eq!(books(&e), b0);
    let _ = e.report();
    let _ = e.report();
    assert_eq!(
        e.model_inventory_tokens(&MINT),
        Some(inv0),
        "disarming restores no legacy force-close authority"
    );
    assert_eq!(books(&e), b0);
    assert!(e.model_excluded_exits().is_empty());
}

#[test]
fn m1_unknown_or_stale_mark_is_unavailable_valuation_not_zero_and_not_a_loss() {
    let hp = held_path("m1_d");
    let mut r = rig(|_| HOLD, &hp);
    let fresh_x = r.e.model_open_exposure();
    assert_eq!(fresh_x.len(), 1);
    assert!(
        fresh_x[0].spot_estimate_lamports.is_some(),
        "fresh curve mark gives an estimate: {fresh_x:?}"
    );
    let b0 = books(&r.e);
    // Advance the lane clock with prints only (no reserve observation) past the pricing budget.
    let t = r.clock + pump_quant_app::curve_annotation::PRICING_BUDGET_MS + 5_000;
    print(&mut r.e, 999, t, r.slot + 50, 45_300);
    ticks(&mut r.e, 1);
    let x = r.e.model_open_exposure();
    assert_eq!(x[0].spot_estimate_lamports, None);
    assert_eq!(x[0].valuation_unavailable, Some("mark_stale"));
    let _ = r.e.report();
    assert_eq!(books(&r.e).4, b0.4, "stale mark manufactures no settlement");
    assert_eq!(books(&r.e).0, b0.0);
    // Restored with no reserve observation at all: unknown, not zero.
    assert!(r.e.model_held_persist_now());
    drop(r);
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    let y = e2.model_open_exposure();
    assert_eq!(
        (y[0].spot_estimate_lamports, y[0].valuation_unavailable),
        (None, Some("no_reserve_observation"))
    );
}

#[test]
fn m1_a_genuine_reconciled_fill_still_settles_normally_after_report() {
    let (mut e, id, intended, inv0, _clock) = restored_with_uncertain("reduce", "m1_e");
    let _ = e.report();
    assert_eq!(
        e.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
    assert_eq!(e.model_inventory_tokens(&MINT), Some(inv0 - intended));
}

// ---- STOP TABLE (stop_policy): one test per trigger row; every row keeps management/protection/reconciliation ----

use pump_quant_app::stop_policy::{
    self as sp, action_for, Continue, Gate, Latch, LiquidationEstimate, OpsInputs, RiskValuation,
    RunPhase, StopTrigger,
};

fn rep_sum(e: &Engine, k: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(key, _)| key.starts_with(k))
        .map(|(_, v)| *v)
        .sum()
}

/// Drive a pending REDUCE to its paper fill; returns tokens sold.
fn fill_pending_reduce(r: &mut Rig) -> u64 {
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let (_, k, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    assert_eq!(k, MgmtKind::Reduce);
    r.clock += 1_000;
    r.slot += 1;
    curve_obs(&mut r.e, r.clock, r.slot, 200_000_000);
    ticks(&mut r.e, 3);
    assert!(r.e.model_mgmt_pending(&MINT).is_none(), "REDUCE filled");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0 - intended));
    intended
}

#[test]
fn stop_table_every_trigger_has_one_named_row_and_no_row_disables_management_protection_or_reconciliation(
) {
    let mut names = std::collections::BTreeSet::new();
    for t in StopTrigger::ALL {
        let a = action_for(t);
        assert!(names.insert(a.name), "row names are unique: {t:?}");
        assert!(!a.alert.is_empty(), "{t:?} surfaces a named alert");
        assert_eq!(
            (a.entry, a.add),
            (Gate::Block, Gate::Block),
            "{t:?} blocks new BUY and ADD"
        );
        assert_eq!(
            a.manage,
            Continue::IfValid,
            "{t:?}: REDUCE/EXIT continue where valid"
        );
        assert_eq!(
            a.protect,
            Continue::Always,
            "{t:?}: protection always continues"
        );
        assert_eq!(
            a.reconcile,
            Continue::Always,
            "{t:?}: reconciliation always continues"
        );
        assert_eq!(a.handoff_after_drain, t == StopTrigger::RunDeadline);
    }
    let latch = |t| action_for(t).latch;
    for t in [
        StopTrigger::EndpointHung,
        StopTrigger::ReconciliationFault,
        StopTrigger::DurableWriteFailure,
        StopTrigger::ShadowDivergence,
        StopTrigger::PaperLossStop,
        StopTrigger::UnknownLiquidationValue,
    ] {
        assert!(
            matches!(latch(t), Latch::SafetyOff(_)),
            "{t:?} latches SAFETY_OFF (no auto re-arm)"
        );
    }
    for t in [
        StopTrigger::DiskHeadroomLow,
        StopTrigger::RamHeadroomLow,
        StopTrigger::FeedGap,
        StopTrigger::RpcBudgetLow,
    ] {
        assert_eq!(
            latch(t),
            Latch::WhileCondition,
            "{t:?} is an operational warning, not risk-off"
        );
    }
    assert_eq!(latch(StopTrigger::RunDeadline), Latch::RunPhase);
    assert_eq!(
        latch(StopTrigger::LiveCapabilityPresent),
        Latch::RefuseStart
    );
}

#[test]
fn stop_endpoint_hung_stops_entry_asks_but_a_valid_management_verdict_still_reduces() {
    let hp = held_path("st_hung");
    let mut r = rig(|s| if s == 0 { REDUCE } else { HOLD }, &hp);
    r.e.model_safety_trip(pump_quant_app::safety_off::REASON_ENDPOINT_HUNG);
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(ev
        .raised
        .iter()
        .any(|(t, _)| *t == StopTrigger::EndpointHung));
    assert!(r.e.model_entries_blocked(), "entry asks stopped");
    assert!(
        !r.e.model_mgmt_asks_blocked(),
        "management keeps its own request path"
    );
    r.advance_to_order(120_000);
    assert!(
        fill_pending_reduce(&mut r) > 0,
        "a valid REDUCE verdict executes under endpoint-hung"
    );
}

#[test]
fn stop_endpoint_hung_refuses_add_by_name() {
    let hp = held_path("st_hung_add");
    let mut r = rig(|s| if s == 0 { ADD } else { HOLD }, &hp);
    r.e.model_safety_trip(pump_quant_app::safety_off::REASON_ENDPOINT_HUNG);
    r.advance_to_order(90_000);
    assert!(r.e.model_mgmt_pending(&MINT).is_none(), "no ADD order");
    assert!(rep_sum(&r.e, "mgmt:refuse:add_blocked_safety_off") >= 1);
}

#[test]
fn stop_reconciliation_fault_latches_risk_off_and_reconciliation_still_settles() {
    let (hp, id, intended) = reduce_pending_world("st_recon");
    let mut e = fresh(&hp);
    e.model_held_restore().unwrap().unwrap();
    assert_eq!(
        e.model_mgmt_ingest_report(MINT, id, intended + 1, 22_000),
        SellReportResult::Fault
    );
    let ev = e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(ev.newly_raised.contains(&StopTrigger::ReconciliationFault));
    assert!(e.model_safety_blocked());
    assert_eq!(e.model_safety_reason(), "reconciliation_fault");
    assert!(e.model_entries_blocked());
    // Reconciliation continues: the correct report for the same order still settles exactly once.
    assert_eq!(
        e.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Applied { delta: intended }
    );
}

#[test]
fn stop_durable_write_failure_latches_risk_off_by_name() {
    let hp = held_path("st_dur");
    let mut r = rig(|_| HOLD, &hp);
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(
        ev.raised.is_empty(),
        "healthy: nothing raised {:?}",
        ev.raised
    );
    r.e.model_held_attach(std::path::Path::new("/proc/pq_no_such_dir/held.json"));
    assert!(!r.e.model_held_persist_now());
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(ev.newly_raised.contains(&StopTrigger::DurableWriteFailure));
    assert_eq!(r.e.model_safety_reason(), "durable_write_failure");
    assert!(
        r.e.model_inventory_tokens(&MINT).is_some(),
        "nothing closed"
    );
}

/// Disk / RAM / feed gap / RPC budget: entries + ADD restricted while the condition holds, REDUCE management and
/// protection continue, SAFETY_OFF is NOT tripped, and the restriction clears when the condition clears.
fn while_condition_row(tag: &str, ops: OpsInputs, t: StopTrigger) {
    let hp = held_path(tag);
    let mut r = rig(
        |s| {
            if s == 0 {
                REDUCE
            } else if s == 1 {
                ADD
            } else {
                HOLD
            }
        },
        &hp,
    );
    assert!(!r.e.model_entries_blocked());
    let ev = r.e.model_stop_evaluate(0, ops);
    assert_eq!(ev.raised.iter().map(|x| x.0).collect::<Vec<_>>(), vec![t]);
    assert!(
        ev.new_risk_blocked && r.e.model_entries_blocked(),
        "{t:?}: new entries blocked"
    );
    assert!(
        !r.e.model_safety_blocked(),
        "{t:?}: an operational warning is not a risk-off latch"
    );
    assert!(!r.e.model_mgmt_asks_blocked());
    r.advance_to_order(120_000);
    assert!(
        fill_pending_reduce(&mut r) > 0,
        "{t:?}: REDUCE management continues"
    );
    // Step 1 asks ADD: refused by the row's name.
    r.advance_to_order(60_000);
    assert!(
        r.e.model_mgmt_pending(&MINT).is_none(),
        "{t:?}: no ADD order"
    );
    let label = format!("mgmt:refuse:add_blocked_stop:{}", action_for(t).name);
    assert!(
        rep_sum(&r.e, &label) >= 1,
        "{t:?}: ADD refused by name {label}: {:?}",
        r.e.model_lane_report()
    );
    // Condition clears -> restriction clears (no latch).
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(
        ev.raised.is_empty() && !r.e.model_entries_blocked(),
        "{t:?}: clears with the condition"
    );
}

#[test]
fn stop_disk_headroom_low_restricts_entries_only_while_low() {
    while_condition_row(
        "st_disk",
        OpsInputs {
            disk_ok: Some(false),
            ..OpsInputs::healthy()
        },
        StopTrigger::DiskHeadroomLow,
    );
}

#[test]
fn stop_disk_headroom_unmeasurable_restricts_like_low() {
    while_condition_row(
        "st_disk_u",
        OpsInputs {
            disk_ok: None,
            ..OpsInputs::healthy()
        },
        StopTrigger::DiskHeadroomLow,
    );
}

#[test]
fn stop_ram_headroom_low_restricts_entries_only_while_low() {
    while_condition_row(
        "st_ram",
        OpsInputs {
            ram_ok: Some(false),
            ..OpsInputs::healthy()
        },
        StopTrigger::RamHeadroomLow,
    );
}

#[test]
fn stop_feed_gap_restricts_entries_only_while_gapped() {
    while_condition_row(
        "st_feed",
        OpsInputs {
            feed_ok: false,
            ..OpsInputs::healthy()
        },
        StopTrigger::FeedGap,
    );
}

#[test]
fn stop_rpc_budget_low_restricts_entries_only_while_low() {
    while_condition_row(
        "st_rpc",
        OpsInputs {
            rpc_budget_ok: false,
            ..OpsInputs::healthy()
        },
        StopTrigger::RpcBudgetLow,
    );
}

#[test]
fn stop_operational_warning_keeps_protection_serving() {
    let hp = held_path("st_prot");
    let mut r = rig(|_| HOLD, &hp);
    r.e.model_stop_evaluate(
        0,
        OpsInputs {
            feed_ok: false,
            disk_ok: Some(false),
            ..OpsInputs::healthy()
        },
    );
    for _ in 0..100 {
        r.clock += 1_000;
        r.slot += 1;
        r.n += 1;
        curve_obs(&mut r.e, r.clock, r.slot, 200_000_000);
        print(&mut r.e, r.n, r.clock, r.slot, 45_300 + i128::from(r.n % 7));
        ticks(&mut r.e, 2);
    }
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    hard_collapse(&mut r.e, r.clock + 1_000, r.slot + 5);
    let (_, q, _, _) =
        r.e.model_protect_pending_order(&MINT)
            .expect("protection serves under restriction");
    assert_eq!(q, inv0);
}

#[test]
fn stop_shadow_divergence_hook_latches_risk_off() {
    let hp = held_path("st_shadow");
    let mut r = rig(|_| HOLD, &hp);
    let ev = r.e.model_stop_evaluate(
        0,
        OpsInputs {
            shadow_divergence: true,
            ..OpsInputs::healthy()
        },
    );
    assert!(ev.newly_raised.contains(&StopTrigger::ShadowDivergence));
    assert_eq!(r.e.model_safety_reason(), "shadow_divergence");
}

/// Cash on hand as the stop table values it, computed here from the accounting view.
fn cash_of(e: &Engine) -> i128 {
    let v = e.model_accounting_view(&MINT);
    i128::from(v.seed) + v.realized - i128::from(v.committed)
}

#[test]
fn stop_paper_loss_stop_trips_at_exactly_half_a_sol_by_the_model_estimate_and_never_picks_the_better_valuation(
) {
    let hp = held_path("st_loss");
    let mut r = rig(|_| HOLD, &hp);
    let cash = cash_of(&r.e);
    // Default estimator: the size-specific executable quote, labelled; its value is net of the landed exit leg.
    let (m, x) = r.e.model_stop_valuations();
    let est = r.e.model_liquidation_estimate(&MINT).unwrap();
    assert_eq!(
        m,
        RiskValuation::Known {
            equity: cash + est,
            loss_from_start: 2_000_000_000 - cash - est
        }
    );
    let spot = r.e.model_open_exposure()[0].spot_estimate_lamports.unwrap();
    assert_eq!(
        x,
        RiskValuation::Known {
            equity: cash + i128::from(spot),
            loss_from_start: 2_000_000_000 - cash - i128::from(spot)
        }
    );
    assert!(
        est < i128::from(spot),
        "costs make the model estimate lower than zero-size spot"
    );
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert_eq!(ev.estimator, sp::ESTIMATOR_EXEC_QUOTE);
    assert!(ev.raised.is_empty());
    // Shadow estimate: loss = 0.5 SOL - 1 lamport -> no trip.
    let half = i128::from(sp::PAPER_LOSS_STOP_LAMPORTS);
    let below = [LiquidationEstimate {
        mint: MINT,
        value: Ok(2_000_000_000 - cash - half + 1),
    }];
    let ev =
        r.e.model_stop_evaluate_with(0, OpsInputs::healthy(), Some(&below));
    assert_eq!(ev.estimator, sp::ESTIMATOR_SHADOW);
    assert!(
        matches!(ev.model_valuation, RiskValuation::Known { loss_from_start, .. } if loss_from_start == half - 1)
    );
    assert!(!r.e.model_safety_blocked(), "below the stop: no trip");
    // Exactly 0.5 SOL by the model estimate -> trip, even though the external valuation shows a far smaller loss.
    let at = [LiquidationEstimate {
        mint: MINT,
        value: Ok(2_000_000_000 - cash - half),
    }];
    let ev =
        r.e.model_stop_evaluate_with(0, OpsInputs::healthy(), Some(&at));
    assert!(
        matches!(ev.external_valuation, RiskValuation::Known { loss_from_start, .. } if loss_from_start < half / 10)
    );
    assert!(ev.newly_raised.contains(&StopTrigger::PaperLossStop));
    assert_eq!(r.e.model_safety_reason(), "paper_loss_stop");
    assert!(
        r.e.model_inventory_tokens(&MINT).is_some(),
        "the loss stop closes nothing"
    );
}

#[test]
fn stop_shadow_estimate_missing_for_a_held_position_is_risk_unknown_not_zero() {
    let hp = held_path("st_shmiss");
    let mut r = rig(|_| HOLD, &hp);
    let ev =
        r.e.model_stop_evaluate_with(0, OpsInputs::healthy(), Some(&[]));
    assert_eq!(
        ev.model_valuation,
        RiskValuation::Unknown {
            mint: MINT,
            reason: "shadow_estimate_missing"
        }
    );
    assert!(ev
        .newly_raised
        .contains(&StopTrigger::UnknownLiquidationValue));
}

#[test]
fn stop_unknown_liquidation_value_is_risk_unknown_never_zero_or_cost_and_never_auto_rearms() {
    let hp = held_path("st_unk");
    let mut r = rig(|_| HOLD, &hp);
    r.e.model_safety_attach(&hp.with_file_name("safety.json"));
    let b0 = books(&r.e);
    let t = r.clock + pump_quant_app::curve_annotation::PRICING_BUDGET_MS + 5_000;
    print(&mut r.e, 999, t, r.slot + 50, 45_300);
    ticks(&mut r.e, 1);
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert_eq!(
        ev.model_valuation,
        RiskValuation::Unknown {
            mint: MINT,
            reason: "mark_stale"
        }
    );
    assert!(
        matches!(ev.external_valuation, RiskValuation::Unknown { .. }),
        "external also unknown, reported"
    );
    assert!(ev
        .newly_raised
        .contains(&StopTrigger::UnknownLiquidationValue));
    assert_eq!(r.e.model_safety_reason(), "risk_unknown_liquidation_value");
    assert!(
        rep_sum(&r.e, "stop:ALERT_RISK_UNKNOWN_LIQUIDATION_VALUE") >= 1,
        "named alert"
    );
    assert_eq!(books(&r.e), b0, "unknown value books nothing");
    // A fresh mark returns: still blocked (no auto re-arm).
    curve_obs(&mut r.e, t + 1_000, r.slot + 60, 200_000_000);
    ticks(&mut r.e, 1);
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(matches!(ev.model_valuation, RiskValuation::Known { .. }));
    assert!(
        r.e.model_safety_blocked() && r.e.model_entries_blocked(),
        "no auto re-arm"
    );
    // Only an explicit operator re-arm lifts it.
    r.e.model_safety_rearm("alon").unwrap();
    assert!(!r.e.model_entries_blocked());
}

#[test]
fn stop_value_book_never_substitutes_zero_or_cost_for_an_unknown_position() {
    let ok = LiquidationEstimate {
        mint: [1; 32],
        value: Ok(100),
    };
    let unk = LiquidationEstimate {
        mint: [2; 32],
        value: Err("mark_stale"),
    };
    assert_eq!(
        sp::value_book(1_000, 900, &[ok.clone()]),
        RiskValuation::Known {
            equity: 1_000,
            loss_from_start: 0
        }
    );
    assert_eq!(
        sp::value_book(1_000, 900, &[ok, unk]),
        RiskValuation::Unknown {
            mint: [2; 32],
            reason: "mark_stale"
        }
    );
    assert_eq!(
        sp::valuation_trigger(&RiskValuation::Unknown {
            mint: [2; 32],
            reason: "x"
        }),
        Some(StopTrigger::UnknownLiquidationValue)
    );
}

#[test]
fn stop_rearm_with_an_active_operational_restriction_keeps_entries_blocked() {
    let hp = held_path("st_rearm");
    let mut r = rig(|_| HOLD, &hp);
    r.e.model_safety_attach(&hp.with_file_name("safety.json"));
    r.e.model_safety_trip_operator();
    r.e.model_stop_evaluate(
        0,
        OpsInputs {
            disk_ok: Some(false),
            ..OpsInputs::healthy()
        },
    );
    r.e.model_safety_rearm("alon").unwrap();
    assert!(
        r.e.model_entries_blocked(),
        "disk restriction still holds after re-arm"
    );
    r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(!r.e.model_entries_blocked());
}

#[test]
fn stop_run_deadline_stops_entries_drains_with_management_and_protection_then_requests_handoff_without_closing(
) {
    let hp = held_path("st_deadline");
    let mut r = rig(|s| if s == 0 { REDUCE } else { HOLD }, &hp);
    r.e.model_stop_set_deadline(10_000, 5_000);
    let ev = r.e.model_stop_evaluate(9_999, OpsInputs::healthy());
    assert_eq!(
        (ev.phase, ev.handoff_due, r.e.model_entries_blocked()),
        (RunPhase::Running, false, false)
    );
    let ev = r.e.model_stop_evaluate(10_000, OpsInputs::healthy());
    assert_eq!(ev.phase, RunPhase::Drain { ends_at_ms: 15_000 });
    assert!(
        !ev.handoff_due && r.e.model_entries_blocked(),
        "deadline: entries stopped, drain begins"
    );
    assert!(
        !r.e.model_safety_blocked(),
        "the deadline is a run phase, not a risk-off latch"
    );
    // DRAIN: management continues.
    r.advance_to_order(120_000);
    assert!(
        fill_pending_reduce(&mut r) > 0,
        "REDUCE executes during the drain"
    );
    // Sticky for the run, even if a clock reading went backwards.
    let ev = r.e.model_stop_evaluate(0, OpsInputs::healthy());
    assert!(matches!(ev.phase, RunPhase::Drain { .. }) && r.e.model_entries_blocked());
    // Drain bound passed: handoff requested; the book is preserved, nothing force-closed (also across report()).
    let b = books(&r.e);
    let ev = r.e.model_stop_evaluate(15_000, OpsInputs::healthy());
    assert_eq!((ev.phase, ev.handoff_due), (RunPhase::HandoffDue, true));
    let _ = r.e.report();
    assert_eq!(books(&r.e), b, "deadline never force-closes the book");
    assert!(r.e.model_position_open(&MINT));
    // Protection still serves after the deadline.
    for _ in 0..40 {
        r.clock += 1_000;
        r.slot += 1;
        r.n += 1;
        curve_obs(&mut r.e, r.clock, r.slot, 200_000_000);
        print(&mut r.e, r.n, r.clock, r.slot, 45_300 + i128::from(r.n % 7));
        ticks(&mut r.e, 2);
    }
    hard_collapse(&mut r.e, r.clock + 1_000, r.slot + 5);
    assert!(
        r.e.model_protect_pending_order(&MINT).is_some(),
        "protection after the deadline"
    );
}

#[test]
fn stop_run_phase_boundaries_and_default_bounds() {
    assert_eq!(sp::RUN_DEADLINE_MS, 21_600_000);
    assert_eq!(sp::DRAIN_BOUND_MS, 1_800_000);
    assert_eq!(
        sp::run_phase(21_599_999, sp::RUN_DEADLINE_MS, sp::DRAIN_BOUND_MS),
        RunPhase::Running
    );
    assert_eq!(
        sp::run_phase(21_600_000, sp::RUN_DEADLINE_MS, sp::DRAIN_BOUND_MS),
        RunPhase::Drain {
            ends_at_ms: 23_400_000
        }
    );
    assert_eq!(
        sp::run_phase(23_399_999, sp::RUN_DEADLINE_MS, sp::DRAIN_BOUND_MS),
        RunPhase::Drain {
            ends_at_ms: 23_400_000
        }
    );
    assert_eq!(
        sp::run_phase(23_400_000, sp::RUN_DEADLINE_MS, sp::DRAIN_BOUND_MS),
        RunPhase::HandoffDue
    );
}

#[test]
fn stop_startup_refuses_any_live_signing_or_submission_capability() {
    let paper = sp::StartCapability {
        live_flag: false,
        keypair_loaded: false,
        submission_sink_installed: false,
        paper_mode: true,
    };
    assert_eq!(sp::require_paper_only(paper), Ok(()));
    assert_eq!(
        sp::require_paper_only(sp::StartCapability {
            live_flag: true,
            ..paper
        }),
        Err("live_flag_present")
    );
    assert_eq!(
        sp::require_paper_only(sp::StartCapability {
            keypair_loaded: true,
            ..paper
        }),
        Err("keypair_loaded")
    );
    assert_eq!(
        sp::require_paper_only(sp::StartCapability {
            submission_sink_installed: true,
            ..paper
        }),
        Err("submission_sink_not_paper")
    );
    assert_eq!(
        sp::require_paper_only(sp::StartCapability {
            paper_mode: false,
            ..paper
        }),
        Err("engine_not_paper_mode")
    );
}

#[test]
fn stop_rpc_budget_reserves_capacity_for_held_positions() {
    let rule = sp::HeldReserve {
        capacity: 100,
        per_held: 30,
    };
    let mut b = sp::WindowBudget::new(rule, 1_000);
    // 2 held positions reserve 60: discovery may spend only 40.
    assert!(b.try_discovery(0, 2, 40));
    assert!(
        !b.try_discovery(0, 2, 1),
        "discovery never eats the held reserve"
    );
    assert!(b.discovery_exhausted(2));
    assert!(b.try_held(0, 60), "held support may use the reserve");
    assert!(!b.try_held(0, 1), "but not beyond capacity");
    // New window.
    assert!(b.try_discovery(1_000, 2, 40) && !b.discovery_exhausted(0));
    // No held positions: discovery may use the whole capacity.
    let mut c = sp::WindowBudget::new(rule, 1_000);
    assert!(c.try_discovery(0, 0, 100));
}

#[test]
fn stop_ram_headroom_from_meminfo() {
    let t = "MemTotal:       263000000 kB\nMemFree: 1 kB\nMemAvailable:    31560000 kB\n";
    let (tot, av) = sp::parse_meminfo(t);
    assert_eq!((tot, av), (Some(263_000_000), Some(31_560_000)));
    assert_eq!(
        sp::ram_ok(tot, av, sp::RAM_FLOOR_BPS),
        Some(true),
        "12.0% exactly is ok"
    );
    assert_eq!(
        sp::ram_ok(tot, Some(31_559_999), sp::RAM_FLOOR_BPS),
        Some(false)
    );
    assert_eq!(
        sp::ram_ok(None, av, sp::RAM_FLOOR_BPS),
        None,
        "unmeasurable is not fine"
    );
}
