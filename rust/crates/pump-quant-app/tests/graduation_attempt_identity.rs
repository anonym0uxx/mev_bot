//! W1.2 GRADUATION ATTEMPT IDENTITY (SCOPE_WIRE_RECOVERY §3, WIRE_R5_OPERATOR_1115 §3) through the real engine.
//!
//! Rules under test:
//! * a provably UNSENT sell may switch to the verified canonical WSOL pool; the switch is part of the durable
//!   hand-off record (write failure: nothing switched, nothing handed off);
//! * once a curve attempt is handed off its venue is IMMUTABLE: it is never priced or settled with pool economics;
//! * timeout / missing ack / quote refusal / uncertainty do NOT establish non-execution: the attempt stays reserved;
//! * a pool REPLACEMENT needs the old attempt's definitive disposition (paper-owned, not uncertain, curve observed
//!   complete), keeps the order id, takes attempt n+1 and sells only the proven remainder;
//! * a route change alone moves no inventory, cash, cost basis or realized PnL;
//! * restart around submission and around publication; late / duplicate old-attempt reports.
//!
//! SYNTHETIC inputs (curve reserves, pool reserves, pool hex) as in m3_exec_economics; the pool fee parts / virtual
//! quote follow a captured non-cashback layout (2/93/30, vq 17_584_505_661). Expected values are computed here from
//! the program arithmetic, never read back from the engine. Engineering-behaviour evidence only.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_manage::{MgmtKind, SellReportResult, SellState, SubmitState};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_app::shadow_pool::PaperFillVersion;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;
use pump_quant_protocol::pumpswap_event::CashbackField;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;
const POOL: [u8; 32] = [0x5A; 32];
const VQ: u64 = 17_584_505_661;
const BRES: u64 = 200_000_000_000_000;
const QRES: u64 = 85_000_000_000;
const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const EXIT: &str = "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: x";

struct Script {
    answer: fn(i64) -> &'static str,
    calls: Arc<AtomicUsize>,
}
impl ModelSource for Script {
    fn complete(&self, _s: &str, user: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if user.starts_with("Decide the next action for a position you already hold") {
            let step: i64 = user
                .lines()
                .find_map(|l| l.strip_prefix("STEP: "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
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
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}
fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn trade_px(e: &mut Engine, i: u32, ts: i64, slot: u64, px: i128, buy: bool) {
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: px,
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: VSOL,
        signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(wallet(i)),
        slot: Some(slot),
        fee_lamports: Some(60_000),
        cu_consumed: Some(90_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
}
fn curve(e: &mut Engine, ts: i64, slot: u64, vsol: u64, vtok: u64, real_sol: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: vsol,
        v_tokens: vtok,
        real_sol_lamports: real_sol,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    });
}
fn pool_swap(e: &mut Engine, ts: i64, slot: u64) {
    e.tick(AppEvent::AmmSwap {
        mint: mint(),
        pool: POOL,
        pool_is_canonical: true,
        quote_is_wsol: true,
        token_reserve_pre: BRES,
        quote_reserve_pre: QRES,
        fee_bps: Some(2 + 93 + 30),
        fee_parts: Some((2, 93, 30)),
        virtual_quote: Some(VQ),
        cashback: CashbackField::NotRecorded,
        is_buy: true,
        token_amount: 1_000_000_000,
        quote_lamports: 50_000,
        trader: [7u8; 32],
        fee_lamports: Some(5_000),
        cu_consumed: Some(1),
        recv_unix_ms: Some(ts),
        slot,
    });
    ticks(e, 2);
}
/// Pool gross for selling exactly `tokens` on the synthetic pool (constant product on quote + virtual quote).
fn pool_gross(tokens: u64) -> u128 {
    (u128::from(QRES) + u128::from(VQ)) * u128::from(tokens)
        / (u128::from(BRES) + u128::from(tokens))
}
fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("gradid_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("held.json")
}

fn engine(answer: fn(i64) -> &'static str, v2: bool, hp: Option<&std::path::Path>) -> Engine {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Script {
        answer,
        calls: Arc::new(AtomicUsize::new(0)),
    });
    if v2 {
        e.model_set_paper_fill(PaperFillVersion::V2Shadow);
    }
    if let Some(p) = hp {
        e.model_held_attach(p);
    }
    e
}

struct Rig {
    e: Engine,
    clock: i64,
    slot: u64,
    n: u32,
}

fn rig(answer: fn(i64) -> &'static str, v2: bool, hp: Option<&std::path::Path>) -> Rig {
    let mut e = engine(answer, v2, hp);
    e.tick(AppEvent::LaunchObserved {
        mint: mint(),
        creator: CREATOR,
        launch_unix_ms: T0,
    });
    for i in 0..40u32 {
        trade_px(
            &mut e,
            i,
            T0 + 1_000 + i64::from(i) * 2_000,
            1_000 + u64::from(i),
            22_000 + i128::from(i),
            i % 3 != 0,
        );
    }
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last, 2_000, VSOL, VTOK, 7_900_000_000);
    e.tick(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ticks(&mut e, 8);
    curve(
        &mut e,
        t_last + 1_500,
        2_100,
        VSOL + 200_000_000,
        VTOK - 4_000_000_000_000,
        8_100_000_000,
    );
    ticks(&mut e, 6);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    Rig {
        e,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
    }
}

impl Rig {
    fn rep(&self, k: &str) -> u64 {
        self.e.model_lane_report().get(k).copied().unwrap_or(0)
    }
    /// Quiet market (no curve landing state) until a management order is pending: it stays a durable INTENT.
    fn to_order(&mut self) -> (u64, u64) {
        let end = self.clock + 180_000;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.clock += 1_000;
            self.n += 1;
            trade_px(
                &mut self.e,
                self.n,
                self.clock,
                self.slot,
                45_300 + i128::from(self.n % 7),
                true,
            );
            ticks(&mut self.e, 2);
        }
        let (id, _, intended, filled) = self
            .e
            .model_mgmt_pending(&MINT)
            .unwrap_or_else(|| panic!("{:?}", self.e.model_lane_report()));
        assert_eq!(filled, 0);
        assert_eq!(
            self.e.model_mgmt_attempt(&MINT, id),
            Some((0, false, SubmitState::Intent)),
            "setup: an unsent curve intent"
        );
        (id, intended)
    }
    /// A curve landing state on which the curve sell is REFUSED (real SOL far below the gross): the attempt is
    /// handed off on the curve (attempt 1) and stays unfilled.
    fn begin_on_curve_refused(&mut self, id: u64) {
        self.clock += 1_000;
        self.slot += 1;
        curve(
            &mut self.e,
            self.clock,
            self.slot,
            VSOL + 200_000_000,
            VTOK - 4_000_000_000_000,
            1_000,
        );
        ticks(&mut self.e, 2);
        assert_eq!(
            self.e.model_mgmt_attempt(&MINT, id),
            Some((1, false, SubmitState::Begun)),
            "{:?}",
            self.e.model_lane_report()
        );
    }
    /// The curve completes (program zeroes the virtual reserves) and the migration is observed.
    fn complete_curve(&mut self) {
        self.clock += 1_000;
        self.slot += 1;
        curve(&mut self.e, self.clock, self.slot, 0, 0, 0);
        self.migrate();
    }
    fn migrate(&mut self) {
        self.e.tick(AppEvent::Migration {
            mint: mint(),
            slot: self.slot,
        });
        ticks(&mut self.e, 2);
    }
    fn swap(&mut self) {
        self.clock += 1_000;
        self.slot += 5;
        pool_swap(&mut self.e, self.clock, self.slot);
    }
    fn reserved(&self) -> u64 {
        self.e
            .model_held_data_status()
            .into_iter()
            .find(|s| s.mint == MINT)
            .map_or(0, |s| s.sell_reserved_tokens)
    }
}

fn no_pool_fill_for(e: &Engine, id: u64) {
    assert!(
        e.model_mgmt_fills()
            .iter()
            .all(|f| !(f.order_id == id && f.pool && f.attempt <= 1)),
        "a curve attempt was settled with pool economics: {:?}",
        e.model_mgmt_fills()
    );
}

// ---------------------------------------------------------------------------------------------------------
// 1. UNSENT graduation: the switch is part of the hand-off; the order keeps its id and size; attempt 1 is a POOL
//    attempt from the start (never a relabelled curve attempt).
// ---------------------------------------------------------------------------------------------------------
#[test]
fn an_unsent_sell_switches_to_the_verified_pool_at_hand_off_and_settles_as_pool_attempt_one() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, false, None);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let basis = r.e.model_accounting_view(&MINT).remaining_cost_basis;
    let (id, intended) = r.to_order();
    assert_eq!(intended, inv);
    r.migrate();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    assert_eq!(r.e.model_accounting_view(&MINT).remaining_cost_basis, basis);
    r.swap();
    let f =
        *r.e.model_mgmt_fills()
            .last()
            .unwrap_or_else(|| panic!("{:?}", r.e.model_lane_report()));
    assert_eq!(
        (f.order_id, f.tokens, f.attempt, f.pool),
        (id, intended, 1, true)
    );
    assert_eq!(u128::from(f.gross_lamports), pool_gross(intended));
    assert_eq!(r.rep("mgmt:route:unsent_curve_to_pool"), 1);
    assert_eq!(r.rep("mgmt:attempt_replaced:curve_complete_to_pool"), 0);
    assert_eq!(r.rep("mgmt:submit:begun"), 1, "one hand-off");
    assert!(!r.e.model_position_open(&MINT));
}

/// The unsent switch is recorded with the hand-off: if that record cannot be written nothing switches and nothing
/// is handed off.
#[test]
fn an_unsent_switch_whose_hand_off_record_cannot_be_written_stays_an_unsent_curve_intent() {
    let hp = tmp("unsent_nowrite");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, true, Some(&hp));
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let (id, _) = r.to_order();
    r.migrate();
    let bad =
        std::env::temp_dir().join(format!("gradid_absent_{}/x/held.json", std::process::id()));
    r.e.model_held_attach(&bad);
    r.swap();
    assert!(r.rep("mgmt:submit_deferred:persist_failed") >= 1);
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((0, false, SubmitState::Intent)),
        "switch withdrawn with the hand-off"
    );
    assert!(r.e.model_mgmt_fills().is_empty());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
}

// ---------------------------------------------------------------------------------------------------------
// 2. A BEGUN curve attempt is never relabelled. Replacement only after definitive disposition.
// ---------------------------------------------------------------------------------------------------------

/// Begun curve attempt + verified pool state, but NO proof the curve completed (no complete-curve observation):
/// the old attempt could still execute on the curve, so it is never priced on the pool and keeps its reservation.
#[test]
fn a_begun_curve_attempt_without_proof_of_curve_completion_is_never_settled_on_the_pool() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, false, None);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let basis = r.e.model_accounting_view(&MINT).remaining_cost_basis;
    let realized = r.e.model_accounting_view(&MINT).realized;
    let (id, _) = r.to_order();
    r.begin_on_curve_refused(id);
    r.migrate();
    r.swap();
    r.swap();
    no_pool_fill_for(&r.e, id);
    assert!(
        r.e.model_mgmt_fills().is_empty(),
        "{:?}",
        r.e.model_mgmt_fills()
    );
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun)),
        "venue/attempt of the handed-off attempt unchanged"
    );
    assert_eq!(r.rep("mgmt:attempt_replaced:curve_complete_to_pool"), 0);
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    assert_eq!(r.e.model_accounting_view(&MINT).remaining_cost_basis, basis);
    assert_eq!(r.e.model_accounting_view(&MINT).realized, realized);
}

/// Begun PAPER curve attempt, curve observed complete (definitive: the program refuses every further curve sell),
/// verified pool: a REPLACEMENT attempt (same id, attempt 2) sells exactly the proven remainder on the pool. The
/// route change alone moved nothing.
#[test]
fn a_begun_curve_attempt_on_a_completed_curve_is_replaced_by_pool_attempt_two_under_the_same_id() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, false, None);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let basis = r.e.model_accounting_view(&MINT).remaining_cost_basis;
    let (id, intended) = r.to_order();
    r.complete_curve();
    assert!(
        r.rep("mgmt:quote_unavailable:curve_complete") >= 1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun))
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    assert_eq!(r.e.model_accounting_view(&MINT).remaining_cost_basis, basis);
    assert!(r.e.model_mgmt_fills().is_empty());
    r.swap();
    let f =
        *r.e.model_mgmt_fills()
            .last()
            .unwrap_or_else(|| panic!("{:?}", r.e.model_lane_report()));
    assert_eq!(
        (f.order_id, f.tokens, f.attempt, f.pool),
        (id, intended, 2, true)
    );
    assert_eq!(u128::from(f.gross_lamports), pool_gross(intended));
    assert_eq!(r.rep("mgmt:attempt_replaced:curve_complete_to_pool"), 1);
    assert_eq!(r.rep("mgmt:route:unsent_curve_to_pool"), 0);
    assert_eq!(
        r.rep("mgmt:submit:begun"),
        2,
        "attempt 1 (curve) + attempt 2 (pool)"
    );
    no_pool_fill_for(&r.e, id);
    assert!(!r.e.model_position_open(&MINT));
}

/// The replacement is recorded durably before the new attempt may run: write failure -> the curve attempt stays
/// as it was (attempt 1, curve, begun) and nothing is priced on the pool.
#[test]
fn a_replacement_whose_record_cannot_be_written_leaves_the_curve_attempt_untouched() {
    let hp = tmp("repl_nowrite");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, true, Some(&hp));
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let (id, intended) = r.to_order();
    r.complete_curve();
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun))
    );
    let bad =
        std::env::temp_dir().join(format!("gradid_absent2_{}/x/held.json", std::process::id()));
    r.e.model_held_attach(&bad);
    r.swap();
    assert!(
        r.rep("mgmt:replace_deferred:persist_failed") >= 1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun))
    );
    assert!(
        r.e.model_mgmt_fills().is_empty(),
        "{:?}",
        r.e.model_mgmt_fills()
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    // Writable again: replaced (attempt 2, pool) and settled once on the pool.
    r.e.model_held_attach(&hp);
    r.swap();
    let f = *r.e.model_mgmt_fills().last().expect("replacement filled");
    assert_eq!(
        (f.order_id, f.tokens, f.attempt, f.pool),
        (id, intended, 2, true)
    );
    no_pool_fill_for(&r.e, id);
}

// ---------------------------------------------------------------------------------------------------------
// 3. SUBMITTED-UNCERTAIN graduation: missing ack does not establish non-execution.
// ---------------------------------------------------------------------------------------------------------
#[test]
fn an_uncertain_curve_attempt_survives_graduation_reserved_and_only_evidence_settles_it_on_the_curve(
) {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, false, None);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let (id, intended) = r.to_order();
    r.begin_on_curve_refused(id);
    assert!(r.e.model_mgmt_mark_ack_uncertain(&MINT, id));
    r.complete_curve();
    r.swap();
    r.swap();
    ticks(&mut r.e, 2);
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun))
    );
    assert_eq!(r.reserved(), intended, "the whole remainder stays reserved");
    assert!(r.e.model_mgmt_fills().is_empty());
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    assert_eq!(r.rep("mgmt:attempt_replaced:curve_complete_to_pool"), 0);
    // Authoritative evidence for the curve attempt settles it once, as a CURVE fill.
    let (g, fee) = (300_000_000u64, 3_000_000u64);
    assert!(matches!(
        r.e.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, intended, g, fee),
        SellReportResult::Applied { .. }
    ));
    let f = *r.e.model_mgmt_fills().last().unwrap();
    assert_eq!(
        (f.order_id, f.attempt, f.pool, f.gross_lamports),
        (id, 1, false, g)
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), None);
    // A duplicate of that report changes nothing.
    assert!(matches!(
        r.e.model_mgmt_ingest_evidence(MINT, id, MgmtKind::Exit, intended, intended, g, fee),
        SellReportResult::Duplicate | SellReportResult::Fault
    ));
    assert_eq!(r.e.model_mgmt_fills().len(), 1);
}

/// An EXTERNAL curve attempt (owned by an executor this process cannot see) is never replaced, even after a
/// reconciled partial fill cleared its uncertainty: only that executor's terminal evidence disposes of it.
#[test]
fn an_external_curve_attempt_is_never_replaced_by_a_paper_pool_attempt() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, false, None);
    r.e.model_set_external_execution(true);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let end = r.clock + 180_000;
    while r.clock < end && r.e.model_mgmt_pending(&MINT).is_none() {
        r.clock += 1_000;
        r.n += 1;
        let (n, c, s) = (r.n, r.clock, r.slot);
        trade_px(&mut r.e, n, c, s, 45_300 + i128::from(n % 7), true);
        ticks(&mut r.e, 2);
    }
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT pending");
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun))
    );
    let part = intended / 4;
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000_000)
        .unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv - part));
    r.complete_curve();
    r.swap();
    r.swap();
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun))
    );
    assert_eq!(r.rep("mgmt:attempt_replaced:curve_complete_to_pool"), 0);
    no_pool_fill_for(&r.e, id);
    assert!(r.e.model_mgmt_fills().iter().all(|f| !f.pool));
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv - part));
}

// ---------------------------------------------------------------------------------------------------------
// 4. PARTIAL fill followed by migration: the replacement sells ONLY the proven remainder, never overlapping.
// ---------------------------------------------------------------------------------------------------------

/// A PAPER-route curve attempt whose partial fill arrived as executor EVIDENCE: the remainder is working at that
/// executor (uncertain, route record still paper). Curve completion + verified pool do not dispose of it: it is
/// never replaced, stays reserved, and nothing is priced on the pool.
#[test]
fn a_paper_curve_attempt_with_an_evidenced_working_remainder_is_never_replaced() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, false, None);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let (id, intended) = r.to_order();
    r.begin_on_curve_refused(id);
    let part = intended / 5;
    assert!(matches!(
        r.e.model_mgmt_ingest_evidence(
            MINT,
            id,
            MgmtKind::Exit,
            intended,
            part,
            50_000_000,
            500_000
        ),
        SellReportResult::Applied { .. }
    ));
    assert_eq!(
        r.e.model_mgmt_submit_state(&MINT, id).map(|s| (s.0, s.3)),
        Some((pump_quant_app::engine::model_manage::ExecRoute::Paper, true)),
        "setup: paper route record, remainder working (uncertain)"
    );
    r.complete_curve();
    r.swap();
    r.swap();
    assert_eq!(
        r.e.model_mgmt_attempt(&MINT, id),
        Some((1, false, SubmitState::Begun))
    );
    assert_eq!(r.rep("mgmt:attempt_replaced:curve_complete_to_pool"), 0);
    assert!(r.e.model_mgmt_fills().iter().all(|f| !f.pool));
    assert_eq!(r.reserved(), intended - part, "remainder stays reserved");
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv - part));
}
#[test]
fn a_partial_curve_fill_then_migration_replaces_only_the_proven_remainder_on_the_pool() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, false, None);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let (id, intended) = r.to_order();
    r.begin_on_curve_refused(id);
    let part = intended / 3;
    // Reconciled partial fill of the CURVE attempt (paper-owned, definitive for those tokens).
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, part, 22_000_000)
        .unwrap();
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv - part));
    let f1 = *r.e.model_mgmt_fills().last().unwrap();
    assert_eq!(
        (f1.order_id, f1.tokens, f1.attempt, f1.pool),
        (id, part, 1, false)
    );
    r.complete_curve();
    r.swap();
    let f2 =
        *r.e.model_mgmt_fills()
            .last()
            .unwrap_or_else(|| panic!("{:?}", r.e.model_lane_report()));
    let rem = intended - part;
    assert_eq!(
        (f2.order_id, f2.tokens, f2.attempt, f2.pool),
        (id, rem, 2, true)
    );
    assert_eq!(
        u128::from(f2.gross_lamports),
        pool_gross(rem),
        "priced for the remainder only"
    );
    let total: u64 =
        r.e.model_mgmt_fills()
            .iter()
            .filter(|f| f.order_id == id)
            .map(|f| f.tokens)
            .sum();
    assert_eq!(total, intended, "no overlapping disposal");
    assert_eq!(r.e.model_inventory_tokens(&MINT), None);
    let rec = r.e.model_sell_rec(id).expect("terminal record");
    assert_eq!((rec.filled, rec.state), (intended, SellState::Completed));
}

// ---------------------------------------------------------------------------------------------------------
// 5. RESTART around submission and publication (v2 settlement, durable held ledger); late old-attempt reports.
// ---------------------------------------------------------------------------------------------------------
fn restart(hp: &std::path::Path) -> Engine {
    let mut e = engine(|_| HOLD, true, Some(hp));
    e.model_held_restore()
        .expect("restore ok")
        .expect("ledger present");
    e
}

/// Crash after the curve hand-off (durable `begun`, curve attempt 1): the restored attempt is UNCERTAIN, the
/// paper record proves nothing executed, reconciliation ends it; it is never re-run as a pool attempt.
#[test]
fn a_restart_after_the_curve_hand_off_never_relabels_the_attempt_as_a_pool_attempt() {
    let hp = tmp("restart_sub");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, true, Some(&hp));
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let (id, intended) = r.to_order();
    r.complete_curve();
    assert!(r.e.model_held_persist_now());
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&hp).unwrap()).unwrap();
    let p = v["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .cloned()
        .expect("begun attempt in the ledger");
    assert_eq!(
        (
            p["submit"].as_str(),
            p["attempt"].as_u64(),
            p["amm"].as_bool()
        ),
        (Some("begun"), Some(1), Some(false))
    );
    let (clock, slot) = (r.clock, r.slot);
    drop(r);
    let mut e2 = restart(&hp);
    assert_eq!(
        e2.model_lane_report()
            .get("mgmt:recon:paper_record_no_execution")
            .copied(),
        Some(1)
    );
    let rec = e2.model_sell_rec(id).expect("terminal record");
    assert_eq!(
        (rec.filled, rec.intended, rec.state),
        (0, intended, SellState::EndedUnfilled)
    );
    pool_swap(&mut e2, clock + 1_000, slot + 5);
    pool_swap(&mut e2, clock + 2_000, slot + 10);
    assert!(
        e2.model_mgmt_fills().iter().all(|f| f.order_id != id),
        "{:?}",
        e2.model_mgmt_fills()
    );
    assert_eq!(e2.model_inventory_tokens(&MINT), Some(inv));
}

/// Crash after the replacement settled and was published: the restarted books hold it exactly once; a late or
/// duplicate report for the order (old curve attempt) cannot settle again.
#[test]
fn a_restart_after_publication_keeps_one_settlement_and_late_old_attempt_reports_change_nothing() {
    let hp = tmp("restart_pub");
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD }, true, Some(&hp));
    let (id, intended) = r.to_order();
    r.complete_curve();
    r.swap();
    let f =
        *r.e.model_mgmt_fills()
            .last()
            .unwrap_or_else(|| panic!("{:?}", r.e.model_lane_report()));
    assert_eq!(
        (f.order_id, f.tokens, f.attempt, f.pool),
        (id, intended, 2, true)
    );
    assert!(r.e.model_held_persist_now());
    let cash = r.e.model_settlement().unwrap().cash;
    drop(r);
    let mut e2 = restart(&hp);
    assert!(e2.model_mgmt_pending(&MINT).is_none());
    assert_eq!(e2.model_inventory_tokens(&MINT), None);
    let rec = e2.model_sell_rec(id).expect("settled record restored");
    assert_eq!((rec.filled, rec.state), (intended, SellState::Completed));
    assert_eq!(e2.model_settlement().unwrap().cash, cash);
    // Same totals: duplicate. Different totals (a late curve-attempt report): named fault. Books unchanged.
    let dup = e2.model_mgmt_ingest_evidence(
        MINT,
        id,
        MgmtKind::Exit,
        intended,
        intended,
        rec.gross,
        rec.fees,
    );
    assert!(matches!(dup, SellReportResult::Duplicate), "{dup:?}");
    let late = e2.model_mgmt_ingest_evidence(
        MINT,
        id,
        MgmtKind::Exit,
        intended,
        intended,
        1_000_000,
        10_000,
    );
    assert!(matches!(late, SellReportResult::Fault), "{late:?}");
    assert_eq!(e2.model_settlement().unwrap().cash, cash);
    assert_eq!(e2.model_inventory_tokens(&MINT), None);
}
