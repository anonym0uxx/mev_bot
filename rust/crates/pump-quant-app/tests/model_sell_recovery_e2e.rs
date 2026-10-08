//! Management sells (REDUCE / EXIT / ADD) across a restart. A report states the order's CUMULATIVE filled quantity,
//! so applying it is idempotent. Expected values are computed independently from the fixture's own numbers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_manage::{SellReportResult, SellState};
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
fn curve_obs(e: &mut Engine, ts: i64, slot: u64, dsol: u64) {
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
fn print(e: &mut Engine, i: u32, ts: i64, slot: u64, price: i128) {
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
    ev.push(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

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
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        calls: Arc::clone(&calls),
        answer,
    });
    e.model_held_attach(held);
    for ev in &warm_events(40) {
        e.tick(*ev);
    }
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve_obs(&mut e, t_last + 1_500, 2_100, 200_000_000);
    ticks(&mut e, 6);
    assert!(e.model_position_open(&MINT), "setup: entry filled");
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
    assert_eq!(e2.model_inventory_tokens(&MINT).is_some(), true);
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
fn a_protective_close_over_an_uncertain_sell_raises_a_named_fault_and_a_late_report_cannot_resurrect_inventory(
) {
    let hp = held_path("s_f");
    let mut r = rig(|step| if step == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    assert!(r.e.model_held_persist_now());
    drop(r);
    // Restart: the unresolved EXIT is restored; then the agreed hard safeguard closes the position.
    let mut e2 = fresh(&hp);
    e2.model_held_restore().unwrap().unwrap();
    assert!(e2.model_position_open(&MINT));
    let clock = T0 + 1_000 + 40 * 2_000 + 400_000;
    hard_collapse(&mut e2, clock, 9_000);
    assert!(
        !e2.model_position_open(&MINT),
        "the protective path closed it"
    );
    // The uncertain management order may have executed too: it is NOT silently dropped.
    let rec = e2.model_sell_rec(id).expect("ended order recorded");
    assert_eq!((rec.state, rec.filled), (SellState::Preempted, 0));
    let f = e2.model_sell_faults().get(&id).expect("named fault");
    assert_eq!(f.source, "uncertain_sell_preempted");
    assert!(
        e2.model_mint_is_blocked(&MINT),
        "new exposure on the mint stays blocked"
    );
    assert!(e2.model_safety_rearm("alon").is_err());
    let realized = e2.model_accounting_view(&MINT).realized;
    // A late report that the EXIT filled: evidence preserved, nothing applied, no inventory resurrected.
    assert_eq!(
        e2.model_mgmt_ingest_report(MINT, id, intended, 22_000),
        SellReportResult::Fault
    );
    assert_eq!(
        e2.model_inventory_tokens(&MINT),
        None,
        "a closed position stays closed"
    );
    assert_eq!(
        e2.model_accounting_view(&MINT).realized,
        realized,
        "no second credit"
    );
    assert_eq!(
        e2.model_sell_faults().get(&id).map(|f| f.reported.clone()),
        Some(vec![intended])
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
