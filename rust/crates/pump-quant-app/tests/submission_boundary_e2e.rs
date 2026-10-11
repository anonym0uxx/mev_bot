//! Durable SUBMISSION STATE of management / protective sells across a kill at every boundary
//! (operator scope SCOPE_WIRE_RECOVERY.md §1):
//!   a) durable intent written, submission not begun -> restores UNSENT -> submitted exactly once after restart;
//!   b) submission begun (durably marked), no acknowledgement -> UNCERTAIN -> never resubmitted; reconciliation decides;
//!   c) acknowledged (external executor), no fill -> UNCERTAIN/working -> never resubmitted;
//!   d) partial fill before AND after settlement publication -> no double settlement, exact inventory/reservation.
//! Plus: an order with NO submission record (an older ledger) restores UNCERTAIN (never inferred unsent); a late or
//! duplicate executor report cannot double-settle; the preserved wW5b1 kill ledger replays to the corrected outcome.
//! Expected values come from the fixture's own numbers, never from the code under test.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_manage::{MgmtKind, SellReportResult};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_app::shadow_pool::PaperFillVersion;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
const VOFF: u64 = 30_000_000_000;
const TOFF: u64 = 280_000_000_000_000;
const RSOL0: u64 = 7_900_000_000;
const RTOK0: u64 = 569_000_000_000_000;
const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const EXIT: &str = "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: x";

fn k() -> u128 {
    u128::from(VOFF + RSOL0) * u128::from(TOFF + RTOK0)
}
fn state(rsol: u64) -> (u64, u64, u64, u64) {
    let vsol = VOFF + rsol;
    let vtok = u64::try_from(k() / u128::from(vsol)).unwrap();
    (vsol, vtok, rsol, vtok - TOFF)
}
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
    c.floor_fraction_bps = 2_500;
    c
}

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

fn curve_ev(ts: i64, slot: u64, rsol: u64) -> AppEvent {
    let (vsol, vtok, rs, rt) = state(rsol);
    AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: vsol,
        v_tokens: vtok,
        real_sol_lamports: rs,
        real_tokens: rt,
        recv_unix_ms: Some(ts),
        slot,
    }
}
fn trade(i: u32, ts: i64, slot: u64, px: i128) -> AppEvent {
    AppEvent::MarketTrade {
        mint: mint(),
        price_fp: px,
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: VOFF + RSOL0,
        signed_base: if !i.is_multiple_of(3) {
            30_000_000_000
        } else {
            -30_000_000_000
        },
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
    }
}
fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("subm_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("held.json")
}
fn rep(e: &Engine, k: &str) -> u64 {
    e.model_lane_report().get(k).copied().unwrap_or(0)
}
fn reserved(e: &Engine) -> u64 {
    e.model_held_data_status()
        .into_iter()
        .find(|s| s.mint == MINT)
        .map_or(0, |s| s.sell_reserved_tokens)
}
fn pending_json(hp: &std::path::Path) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(hp).unwrap()).unwrap();
    v["pending"].clone()
}

struct Rig {
    e: Engine,
    clock: i64,
    slot: u64,
    n: u32,
    rsol: u64,
}

fn engine(answer: fn(i64) -> &'static str, hp: &std::path::Path) -> Engine {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        calls: Arc::new(AtomicUsize::new(0)),
        answer,
    });
    e.model_set_paper_fill(PaperFillVersion::V2Shadow);
    e.model_held_attach(hp);
    // Canonical (non-Mayhem) curve: pq_daemon pushes CurveModeObserved from the decoded account before its
    // reserves; the mode map is in-memory, so every engine here (incl. restarted ones) re-learns it (offset merge).
    e.tick(AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem: false,
        slot: 0,
    });
    e
}

fn rig(answer: fn(i64) -> &'static str, hp: &std::path::Path) -> Rig {
    let mut e = engine(answer, hp);
    e.tick(AppEvent::LaunchObserved {
        mint: mint(),
        creator: CREATOR,
        launch_unix_ms: T0,
    });
    for i in 0..40u32 {
        e.tick(trade(
            i,
            T0 + 1_000 + i64::from(i) * 2_000,
            1_000 + u64::from(i),
            22_000 + i128::from(i),
        ));
    }
    let t_last = T0 + 1_000 + 40 * 2_000;
    e.tick(curve_ev(t_last, 2_000, RSOL0));
    e.tick(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VOFF + RSOL0,
        real_sol_lamports: RSOL0,
    });
    ticks(&mut e, 8);
    let rsol = RSOL0 + 200_000_000;
    e.tick(curve_ev(t_last + 1_500, 2_100, rsol));
    ticks(&mut e, 6);
    assert!(
        e.model_position_open(&MINT),
        "setup: entry filled: {:?}",
        e.model_lane_report()
    );
    Rig {
        e,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
        rsol,
    }
}

impl Rig {
    /// Advance with prints (no curve landing state) until a management order exists. The paper executor cannot
    /// land it without a newer curve observation, so the order stays at its first durable state.
    fn advance_to_order(&mut self, max_ms: i64) {
        let end = self.clock + max_ms;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.clock += 1_000;
            self.n += 1;
            self.e.tick(trade(
                self.n,
                self.clock,
                self.slot,
                45_300 + i128::from(self.n % 7),
            ));
            ticks(&mut self.e, 2);
        }
    }
}

/// A restarted engine on the same ledger. Fresh v2 engine; the model answers HOLD.
fn restart(hp: &std::path::Path) -> Engine {
    let mut e = engine(|_| HOLD, hp);
    e.model_held_restore()
        .expect("restore ok")
        .expect("ledger present");
    e
}

/// Landing states for the paper executor (newer slots, >= 400 ms after creation).
fn land(e: &mut Engine, from: i64, slot0: u64, rsol: u64, n: u64) {
    for k in 0..n {
        e.tick(curve_ev(from + 1_000 * (k as i64 + 1), slot0 + k, rsol));
        ticks(e, 2);
    }
}

// ---------- (a) durable intent written, submission not begun ----------
#[test]
fn a_intent_only_restores_unsent_and_is_submitted_exactly_once_after_restart() {
    let hp = tmp("a");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    // Pin the boundary: the durable record is written at order creation BEFORE any executor attempt, and the paper
    // executor only begins at a landing state. Advance with prints only (no landing state).
    r.advance_to_order(120_000);
    let (id, kind, intended, filled) = r.e.model_mgmt_pending(&MINT).expect("EXIT created");
    assert_eq!((kind, filled), (MgmtKind::Exit, 0));
    assert!(r.e.model_held_persist_now());
    let p = pending_json(&hp);
    let rec = p
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .expect("durable order record");
    assert_eq!(rec["exec"], "paper", "{rec}");
    assert_eq!(
        rec["submit"], "intent",
        "no executor attempt has begun: {rec}"
    );
    assert_eq!(rec["attempt"], 0);
    let (clock, slot, rsol) = (r.clock, r.slot, r.rsol);
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    drop(r); // kill

    let mut e2 = restart(&hp);
    assert_eq!(rep(&e2, "held_state:restored_pending_unsent"), 1);
    assert_eq!(rep(&e2, "held_state:restored_pending_uncertain"), 0);
    let (id2, k2, i2, f2, unc) = e2.model_mgmt_order_status(&MINT).expect("restored");
    assert_eq!(
        (id2, k2, i2, f2, unc),
        (id, MgmtKind::Exit, intended, 0, false),
        "UNSENT, same identity"
    );
    assert_eq!(
        reserved(&e2),
        0,
        "an unsent order reserves nothing at an executor"
    );
    // It is submitted exactly once (attempt 1) by the normal path and settles once.
    land(&mut e2, clock, slot + 10, rsol, 3);
    assert!(
        !e2.model_position_open(&MINT),
        "EXIT filled once: {:?}",
        e2.model_lane_report()
    );
    let rec = e2.model_sell_rec(id).expect("settled record");
    assert_eq!(rec.filled, inv0);
    assert_eq!(rep(&e2, "mgmt:submit:begun"), 1, "exactly one submission");
    let l = e2.model_settlement().unwrap();
    assert!(l.invariant_holds());
    assert_eq!(
        l.cumulative(&MINT, pump_quant_app::shadow_pool::LegKind::Sell, id)
            .0,
        inv0,
        "settled once"
    );
}

// ---------- persist BEFORE the side effect ----------
/// The hand-off record is written BEFORE the paper executor may price or fill. If that write fails the order stays
/// at INTENT and the executor never sees it (no fill on a landing state); once the ledger is writable again the
/// order is handed off and settles exactly once.
#[test]
fn a_handoff_whose_durable_record_cannot_be_written_is_never_executed() {
    let hp = tmp("a_nowrite");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, _, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT created");
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    // The ledger path becomes unwritable (its parent does not exist).
    let bad = std::env::temp_dir().join(format!("pq_sb_absent_{}/x/held.json", std::process::id()));
    r.e.model_held_attach(&bad);
    let (clock, slot, rsol) = (r.clock, r.slot, r.rsol);
    land(&mut r.e, clock, slot + 10, rsol, 3);
    assert!(rep(&r.e, "mgmt:submit_deferred:persist_failed") >= 1);
    assert_eq!(rep(&r.e, "mgmt:submit:begun"), 0, "nothing handed off");
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv0),
        "never executed"
    );
    assert_eq!(
        r.e.model_mgmt_pending(&MINT).map(|p| p.0),
        Some(id),
        "still the same intent"
    );
    // Writable again: handed off once, settled once.
    r.e.model_held_attach(&hp);
    land(&mut r.e, clock + 3_000, slot + 20, rsol, 3);
    assert_eq!(rep(&r.e, "mgmt:submit:begun"), 1);
    // (The entry lane may re-enter afterwards; the EXIT itself closed the first position exactly once.)
    assert_eq!(
        rep(&r.e, "mgmt:fill:closed"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.e.model_sell_rec(id).map(|x| x.filled), Some(inv0));
}

// ---------- (b) submission begun (durably marked), no acknowledgement ----------
#[test]
fn b_begun_without_ack_restores_uncertain_and_the_paper_record_reconciles_it_without_resubmit() {
    let hp = tmp("b");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).unwrap();
    // A landing state: the paper executor durably records the hand-off FIRST, then fills. Capture the ledger exactly
    // as it was published at the hand-off (crash between the durable mark and the fill).
    let (clock, slot, rsol) = (r.clock, r.slot, r.rsol);
    let before = std::fs::read(&hp).unwrap();
    land(&mut r.e, clock, slot + 10, rsol, 1);
    // The sell may have completed in-process; the durable "begun" ledger is the one written before the fill.
    let marks = rep(&r.e, "mgmt:submit:begun");
    assert_eq!(marks, 1, "the hand-off was recorded once");
    drop(r);
    // Reconstruct the crash point: the begun ledger is the last write that precedes the fill. Rebuild it from the
    // pre-landing ledger by changing only the submission fields (what the hand-off persist wrote).
    let mut v: serde_json::Value = serde_json::from_slice(&before).unwrap();
    for p in v["pending"].as_array_mut().unwrap() {
        if p["id"] == id {
            assert_eq!(p["submit"], "intent");
            p["submit"] = "begun".into();
            p["attempt"] = 1.into();
        }
    }
    std::fs::write(&hp, v.to_string()).unwrap();
    let mut e2 = restart(&hp);
    assert_eq!(
        rep(&e2, "held_state:restored_pending_unsent"),
        0,
        "begun is never unsent"
    );
    // The paper executor's durable record (the settlement ledger in the same file) holds no fill for this order:
    // definitively not executed. Reconciliation ends it without resubmitting; protection/management re-evaluate.
    assert_eq!(rep(&e2, "mgmt:recon:paper_record_no_execution"), 1);
    assert!(
        e2.model_mgmt_pending(&MINT).is_none(),
        "ended by reconciliation, not resubmitted"
    );
    let rec = e2.model_sell_rec(id).expect("terminal record kept");
    assert_eq!((rec.filled, rec.intended), (0, intended));
    assert_eq!(reserved(&e2), 0);
    assert_eq!(
        rep(&e2, "mgmt:submit:begun"),
        0,
        "no resubmission of that attempt"
    );
    // A late report for the reconciled order cannot settle it (named fault, books unchanged).
    let inv = e2.model_inventory_tokens(&MINT);
    let r2 = e2.model_mgmt_ingest_evidence(
        MINT,
        id,
        MgmtKind::Exit,
        intended,
        intended,
        1_000_000,
        10_000,
    );
    assert!(matches!(r2, SellReportResult::Fault), "{r2:?}");
    assert_eq!(e2.model_inventory_tokens(&MINT), inv);
}

// ---------- (b') an EXTERNAL begun hand-off with no acknowledgement stays uncertain ----------
#[test]
fn b_external_begun_without_ack_stays_uncertain_and_is_never_resubmitted() {
    let hp = tmp("b_ext");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    r.e.model_set_external_execution(true);
    r.advance_to_order(120_000);
    let (id, _, intended, _, unc) = r.e.model_mgmt_order_status(&MINT).unwrap();
    assert!(unc);
    let p = pending_json(&hp);
    let rec = p
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .expect("hand-off persisted before submit");
    assert_eq!(
        (rec["exec"].as_str(), rec["submit"].as_str()),
        (Some("external"), Some("begun"))
    );
    let (clock, slot, rsol) = (r.clock, r.slot, r.rsol);
    drop(r);
    let mut e2 = restart(&hp);
    assert_eq!(rep(&e2, "held_state:restored_pending_uncertain"), 1);
    land(&mut e2, clock, slot + 10, rsol, 4);
    let (id2, _, _, f2, unc2) = e2.model_mgmt_order_status(&MINT).unwrap();
    assert_eq!(
        (id2, f2, unc2),
        (id, 0, true),
        "never paper-filled, never resubmitted"
    );
    assert_eq!(
        reserved(&e2),
        intended,
        "the whole remainder stays reserved"
    );
    assert_eq!(
        rep(&e2, "mgmt:submit:begun") + rep(&e2, "mgmt:submitted_external"),
        0
    );
}

// ---------- (c) acknowledged (working at the external executor), no fill ----------
#[test]
fn c_acknowledged_without_fill_stays_working_and_only_evidence_settles_it_once() {
    let hp = tmp("c");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    r.e.model_set_external_execution(true);
    r.advance_to_order(120_000);
    let (id, _, intended, _, _) = r.e.model_mgmt_order_status(&MINT).unwrap();
    // A zero-fill acknowledgement is a duplicate of the books (nothing changes, still working).
    assert!(matches!(
        r.e.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, 0, 0, 0),
        SellReportResult::Duplicate
    ));
    assert!(r.e.model_held_persist_now());
    drop(r);
    let mut e2 = restart(&hp);
    let (_, _, _, _, unc) = e2.model_mgmt_order_status(&MINT).unwrap();
    assert!(unc, "working");
    let (g, f) = (900_000_000u64, 9_000_000u64);
    assert!(matches!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, intended, g, f),
        SellReportResult::Applied { .. }
    ));
    assert!(!e2.model_position_open(&MINT));
    // Duplicate report: no second settlement.
    let cash = e2.model_settlement().unwrap().cash;
    assert!(matches!(
        e2.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, intended, g, f),
        SellReportResult::Duplicate
    ));
    assert_eq!(e2.model_settlement().unwrap().cash, cash);
}

// ---------- (d) partial fill, before AND after settlement publication ----------
#[test]
fn d_partial_fill_before_and_after_publication_never_double_settles() {
    let hp = tmp("d");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    r.e.model_set_external_execution(true);
    r.advance_to_order(120_000);
    let (id, _, intended, _, _) = r.e.model_mgmt_order_status(&MINT).unwrap();
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    let before_pub = std::fs::read(&hp).unwrap(); // ledger published BEFORE the partial fill
    let part = intended / 3;
    let (g1, f1) = (300_000_000u64, 3_000_000u64);
    assert!(matches!(
        r.e.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, part, g1, f1),
        SellReportResult::Applied { .. }
    ));
    assert!(r.e.model_held_persist_now()); // published AFTER the partial fill
    let after_pub = std::fs::read(&hp).unwrap();
    drop(r);

    // Crash AFTER publication: the partial is in the books exactly once; the same report is a duplicate.
    std::fs::write(&hp, &after_pub).unwrap();
    let mut e_after = restart(&hp);
    e_after.tick(AppEvent::Tick); // reservations are derived per event
    assert_eq!(e_after.model_inventory_tokens(&MINT), Some(inv0 - part));
    assert_eq!(
        reserved(&e_after),
        intended - part,
        "reservation = exact remainder"
    );
    let cash_after = e_after.model_settlement().unwrap().cash;
    assert!(matches!(
        e_after.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, part, g1, f1),
        SellReportResult::Duplicate
    ));
    assert_eq!(e_after.model_settlement().unwrap().cash, cash_after);

    // Crash BEFORE publication: the books never saw the fill; the (re-delivered) report applies it ONCE.
    std::fs::write(&hp, &before_pub).unwrap();
    let mut e_before = restart(&hp);
    e_before.tick(AppEvent::Tick);
    assert_eq!(e_before.model_inventory_tokens(&MINT), Some(inv0));
    assert_eq!(reserved(&e_before), intended);
    assert!(matches!(
        e_before.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, part, g1, f1),
        SellReportResult::Applied { .. }
    ));
    assert_eq!(e_before.model_inventory_tokens(&MINT), Some(inv0 - part));
    assert_eq!(
        e_before.model_settlement().unwrap().cash,
        cash_after,
        "both paths settle the same cash once"
    );
    assert!(matches!(
        e_before.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, part, g1, f1),
        SellReportResult::Duplicate
    ));
    assert_eq!(e_before.model_settlement().unwrap().cash, cash_after);
}

// ---------- an order with NO submission record is never inferred unsent ----------
#[test]
fn an_order_without_a_submission_record_restores_uncertain_never_unsent() {
    let hp = tmp("norec");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, &hp);
    r.advance_to_order(120_000);
    let (id, _, _, _) = r.e.model_mgmt_pending(&MINT).unwrap();
    assert!(r.e.model_held_persist_now());
    drop(r);
    // Strip the submission record (an older-format ledger): absence proves nothing.
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&hp).unwrap()).unwrap();
    for p in v["pending"].as_array_mut().unwrap() {
        p.as_object_mut().unwrap().remove("submit");
        p.as_object_mut().unwrap().remove("exec");
    }
    std::fs::write(&hp, v.to_string()).unwrap();
    let e2 = restart(&hp);
    assert_eq!(rep(&e2, "held_state:restored_pending_unsent"), 0);
    assert_eq!(rep(&e2, "held_state:restored_submission_unrecorded"), 1);
    let (id2, _, _, _, unc) = e2.model_mgmt_order_status(&MINT).unwrap();
    assert_eq!((id2, unc), (id, true), "conservative UNCERTAIN");
    // Unknown submit value is refused by name (never guessed).
    let mut v2 = v.clone();
    v2["pending"][0]["submit"] = "maybe".into();
    std::fs::write(&hp, v2.to_string()).unwrap();
    let mut e3 = engine(|_| HOLD, &hp);
    assert!(e3.model_held_restore().is_err());
}

// ---------- preserved regression fixture: the wW5b1 kill ledger ----------
const FIXTURE_SHA: &str = "cc861b5e368c883893672a1c4829d10fc28264d29c4d9713207a297dbe24be5e";
const M804: &str = "804a86fe22765107b0af553e16e14ab3c7f554da960307fc0ef3adf00de230ff";

fn unhex(s: &str) -> [u8; 32] {
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    o
}

/// What the wW5b1 durable bytes PROVE about the boundary the kill hit: the protective order record carries NO route
/// and NO submission state (that binary persisted neither), attempt 0, nothing filled, and the settlement ledger in
/// the same file holds no sell leg. Those bytes cannot prove "never submitted", so UNCERTAIN on restore was the
/// CORRECT label and stays the result without further evidence. The defect was that no reconciliation path ever
/// resolved an uncertain PAPER order. With operator evidence that the writer (this exact lineage) ran only the
/// in-process paper executor (wW5b1/err.log has no external-executor line), the order is reconciled against the
/// paper executor's durable record (same atomic file): definitively not executed -> ended by evidence, reservation
/// released, the trigger re-armed so protection re-evaluates; books and divergence evidence unchanged.
#[test]
fn ww5b1_kill_ledger_replays_to_the_corrected_outcome() {
    let bytes = include_bytes!("fixtures/wW5b1_held_at_kill.json");
    let sha = pump_quant_protocol::sha256::to_hex(&pump_quant_protocol::sha256::sha256(bytes));
    assert_eq!(
        sha, FIXTURE_SHA,
        "fixture is the preserved byte-identical ledger"
    );
    let v: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let p = &v["pending"][0];
    assert_eq!(
        (p["kind"].as_str(), p["mint"].as_str()),
        (Some("protect"), Some(M804))
    );
    assert!(
        p.get("submit").is_none() && p.get("exec").is_none(),
        "no submission record in those bytes"
    );
    assert_eq!(
        (p["attempt"].as_u64(), p["filled"].as_u64()),
        (Some(0), Some(0))
    );
    let applied = v["paper_fill"]["settlement"]["applied"].as_array().unwrap();
    assert!(
        applied.iter().all(|a| a[1] == "entry"),
        "no sell leg was ever settled"
    );
    let pid = p["id"].as_u64().unwrap();
    let intended = p["intended"].as_u64().unwrap();
    let lineage = v["lineage"].as_str().unwrap().to_string();
    let m = unhex(M804);
    let open = |attest: Option<&str>, tag: &str| -> Engine {
        let d = tmp(tag);
        std::fs::write(&d, bytes).unwrap();
        let mut c = cfg();
        c.bankroll_initial_lamports = v["seed_lamports"].as_u64().unwrap();
        let mut e = Engine::new(c, RunMode::Paper);
        e.enable_paper_model(Script {
            calls: Arc::new(AtomicUsize::new(0)),
            answer: |_| HOLD,
        });
        e.model_set_paper_fill(PaperFillVersion::V2Shadow);
        e.model_held_attach(&d);
        if let Some(l) = attest {
            e.model_attest_paper_route(l);
        }
        let r = e.model_held_restore().expect("restore").expect("present");
        assert_eq!(r.positions, 2);
        assert_eq!(
            r.pending_unsent, 0,
            "never inferred unsent from an absent record"
        );
        assert_eq!(rep(&e, "held_state:restored_submission_unrecorded"), 1);
        e
    };
    // 1) No evidence beyond the bytes: conservative UNCERTAIN, the whole inventory stays reserved (as wW5b2 showed).
    let mut e0 = open(None, "ww5b1_none");
    e0.tick(AppEvent::Tick);
    assert_eq!(
        rep(&e0, "held_state:paper_reconcile_skipped:route_unrecorded"),
        1
    );
    assert_eq!(e0.model_protect_pending_order(&m).map(|x| x.0), Some(pid));
    let st0 = e0
        .model_held_data_status()
        .into_iter()
        .find(|s| s.mint == m)
        .unwrap();
    assert_eq!(st0.sell_reserved_tokens, intended);
    // Evidence for a DIFFERENT lineage is not evidence for this ledger.
    let e_wrong = open(Some("00000000000000000000000000000000"), "ww5b1_wrong");
    assert_eq!(
        e_wrong.model_protect_pending_order(&m).map(|x| x.0),
        Some(pid)
    );
    // 2) With operator evidence for this lineage: reconciled against the paper record, definitively not executed.
    let mut e = open(Some(&lineage), "ww5b1_attest");
    assert_eq!(rep(&e, "held_state:paper_route_attested"), 1);
    assert_eq!(
        rep(&e, "protect:recon:paper_record_no_execution"),
        1,
        "{:?}",
        e.model_lane_report()
    );
    let rec = e.model_sell_rec(pid).expect("terminal record kept");
    assert_eq!((rec.filled, rec.intended), (0, intended));
    assert!(
        e.model_sell_faults().is_empty(),
        "ended by evidence, not preempted: no fault"
    );
    let l = e.model_settlement().unwrap();
    assert_eq!(
        l.cash.to_string(),
        v["paper_fill"]["settlement"]["cash"].as_str().unwrap()
    );
    assert!(l.invariant_holds());
    assert_eq!(
        e.model_inventory_tokens(&m),
        Some(intended),
        "inventory unchanged"
    );
    assert_eq!(
        e.model_divergence_log().len(),
        v["paper_fill"]["divergences"].as_array().unwrap().len()
    );
    // (The SAFETY_OFF latch lives in safety.json, a separate file not part of this fixture.)
    assert_eq!(
        e.model_divergence_log()[0].mint,
        m,
        "the 804a86fe divergence record is intact"
    );
    // The trigger is re-armed: protection re-evaluates on the next event with a NEW order identity (never the old
    // attempt relabelled). Without a fresh mark the level stop is served again.
    e.tick(AppEvent::Tick);
    let (nid, nint, nfill, _) = e
        .model_protect_pending_order(&m)
        .expect("protection re-evaluated");
    assert!(nid > pid, "new identity {nid} after reconciled {pid}");
    assert_eq!((nint, nfill), (intended, 0));
    assert_eq!(
        e.model_mgmt_submit_state(&m, nid).map(|s| s.2),
        Some(0),
        "new order starts at attempt 0 (intent)"
    );
}
