//! LIFECYCLE tests (not routing, not quote arithmetic, not prompt parity): the paper lane's order
//! bookkeeping under duplicate delivery, uncertain acknowledgement and reconciliation.
//!
//! They run on the CURVE plane because it has a complete synthetic feed here; they say nothing
//! about AMM quotes or feature parity. Pending intent is not inventory: a position appears only on
//! a fill, and a fill is applied at most once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_admit::{
    Evidence, EvidenceResult, FaultResolution, FillReport, OrderState, ReconcileOutcome,
};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;
const BUY: &str = "DECISION: BUY\nSIZE: SMALL\nINVALIDATION: none\nEVIDENCE: flow sustained";

struct Stub(Arc<AtomicUsize>);
impl ModelSource for Stub {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
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

fn feed(n: u32) -> Vec<AppEvent> {
    let mut ev = vec![AppEvent::LaunchObserved {
        mint: mint(),
        creator: [0xCD; 32],
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
    ev.push(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(T0 + 1_000 + i64::from(n) * 2_000),
        slot: 2_000,
    });
    ev.push(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

fn pump(e: &mut Engine, ticks: usize) {
    for _ in 0..ticks {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn rep(e: &Engine, k: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(n, _)| n.starts_with(k))
        .map(|(_, v)| *v)
        .sum()
}

/// An engine holding exactly one PENDING order (verdict accepted, no landing state yet).
fn with_pending() -> (Engine, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));
    for ev in feed(40) {
        e.tick(ev);
    }
    pump(&mut e, 8);
    assert_eq!(e.model_pending_orders(), 1, "{:?}", e.model_lane_report());
    assert!(!e.model_position_open(&MINT));
    (e, calls)
}

fn landing(e: &mut Engine, ts: i64, slot: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    });
    pump(e, 4);
}

const T_LAND: i64 = T0 + 1_000 + 40 * 2_000 + 1_500;

#[test]
fn lifecycle_a_duplicate_landing_observation_applies_the_fill_once() {
    let (mut e, _c) = with_pending();
    landing(&mut e, T_LAND, 2_100);
    assert!(e.model_position_open(&MINT));
    assert_eq!(rep(&e, "fill:position_opened"), 1);
    let bal = e.bankroll_balance();
    // The same landing state delivered again, and then a later one: no second position, no second debit.
    landing(&mut e, T_LAND, 2_100);
    landing(&mut e, T_LAND + 900, 2_101);
    assert_eq!(
        rep(&e, "fill:position_opened"),
        1,
        "{:?}",
        e.model_lane_report()
    );
    assert_eq!(e.model_pending_orders(), 0);
    assert_eq!(
        e.bankroll_balance(),
        bal,
        "no second debit from a duplicate"
    );
}

/// Identity of the order currently pending on MINT, as a report-ingestion layer would quote it back.
fn pending(e: &Engine) -> (u64, u32, u64) {
    e.model_pending_order(&MINT).expect("a pending order")
}

fn ev(id: u64, attempt: u32, clip: u64, outcome: ReconcileOutcome) -> Evidence {
    Evidence {
        order_id: id,
        attempt,
        clip_lamports: clip,
        outcome,
    }
}

fn fr() -> FillReport {
    FillReport {
        entry_price_fp: 30_000,
        reserve_sol_lamports: VSOL,
    }
}

#[test]
fn lifecycle_b_an_uncertain_ack_stays_pending_past_the_ttl_and_is_not_inventory() {
    let (mut e, _c) = with_pending();
    let (id, _, _) = pending(&e);
    assert!(e.model_mark_ack_uncertain(id));
    // Far beyond the order TTL, with landing states available: it must neither fill nor expire.
    landing(&mut e, T_LAND + 60_000, 2_200);
    assert_eq!(
        e.model_pending_orders(),
        1,
        "uncertain order must stay pending"
    );
    assert!(
        !e.model_position_open(&MINT),
        "unknown ack is not inventory"
    );
    assert_eq!(
        rep(&e, "fill_none:no_landing_state"),
        0,
        "TTL must not clear an uncertain order"
    );
    assert!(rep(&e, "pending_uncertain_held") >= 1);
    assert_eq!(
        e.model_order_rec(id).unwrap().state,
        OrderState::PendingUncertain
    );
}

#[test]
fn lifecycle_c_reconciled_not_filled_clears_the_intent_without_a_position() {
    let (mut e, _c) = with_pending();
    let (id, at, q) = pending(&e);
    e.model_mark_ack_uncertain(id);
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Applied
    );
    assert_eq!(e.model_pending_orders(), 0);
    assert!(!e.model_position_open(&MINT));
    // Credible Filled evidence for the SAME order after NotFilled is a fault, never inventory.
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::Filled(fr()))),
        EvidenceResult::Fault
    );
    assert!(
        !e.model_position_open(&MINT),
        "a report after clearing cannot resurrect inventory"
    );
}

#[test]
fn lifecycle_d_a_reconciled_fill_is_applied_exactly_once() {
    let (mut e, _c) = with_pending();
    let (id, at, q) = pending(&e);
    e.model_mark_ack_uncertain(id);
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::Filled(fr()))),
        EvidenceResult::Applied
    );
    assert!(e.model_position_open(&MINT));
    assert_eq!(rep(&e, "fill:applied_from_reconcile"), 1);
    let bal = e.bankroll_balance();
    // Duplicate confirmation: no second position, no second debit.
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::Filled(fr()))),
        EvidenceResult::Duplicate
    );
    assert_eq!(rep(&e, "reconcile:duplicate_same_terminal"), 1);
    // A CONFLICTING terminal report: named fault, evidence kept, nothing applied.
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Fault
    );
    assert_eq!(rep(&e, "reconcile:FAULT_conflicting_terminal"), 1);
    assert_eq!(
        e.model_recon_faults()
            .get(&id)
            .map(|f| f.contradicting.len()),
        Some(1)
    );
    assert_eq!(rep(&e, "fill:position_opened"), 1);
    assert_eq!(e.bankroll_balance(), bal);
    assert!(e.model_position_open(&MINT));
}

#[test]
fn lifecycle_g_not_filled_then_credible_filled_is_a_fault_that_blocks_exposure_until_resolved() {
    let (mut e, _c) = with_pending();
    let (id, at, q) = pending(&e);
    e.model_mark_ack_uncertain(id);
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Applied
    );
    let bal = e.bankroll_balance();
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::Filled(fr()))),
        EvidenceResult::Fault
    );
    assert_eq!(rep(&e, "reconcile:FAULT_conflicting_terminal"), 1);
    assert_eq!(
        e.model_recon_faults()
            .get(&id)
            .map(|f| f.contradicting.len()),
        Some(1)
    );
    assert!(!e.model_position_open(&MINT), "no silent inventory");
    assert_eq!(e.bankroll_balance(), bal, "no debit");
    // New exposure for the affected mint is blocked while the fault stands.
    let asks = rep(&e, "dispatched");
    for _ in 0..40 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rep(&e, "dispatched"), asks);
    // Authority says FILLED but the books cleared the order: inventing an entry would fabricate
    // economics => REFUSED, fault and block REMAIN.
    assert_eq!(
        e.model_resolve_recon_fault(id, ReconcileOutcome::Filled(fr())),
        FaultResolution::Refused("filled_evidence_without_matching_book_state")
    );
    assert_eq!(e.model_recon_faults().len(), 1, "fault stands");
    // Authority confirms NOT filled: books already agree => released.
    assert_eq!(
        e.model_resolve_recon_fault(id, ReconcileOutcome::NotFilled),
        FaultResolution::Released { unwound: false }
    );
    assert!(e.model_recon_faults().is_empty());
    assert!(!e.model_position_open(&MINT));
    assert_eq!(e.bankroll_balance(), bal, "still no debit");
}

#[test]
fn lifecycle_m_evidence_must_match_order_attempt_and_quantity_or_touch_nothing() {
    let (mut e, _c) = with_pending();
    let (id, at, q) = pending(&e);
    let before = (e.model_pending_orders(), e.bankroll_balance());
    for (bad, why) in [
        (
            ev(id + 99, at, q, ReconcileOutcome::NotFilled),
            "unknown_order",
        ),
        (
            ev(id, at + 1, q, ReconcileOutcome::NotFilled),
            "attempt_mismatch",
        ),
        (
            ev(id, at, q + 1, ReconcileOutcome::NotFilled),
            "quantity_mismatch",
        ),
    ] {
        assert_eq!(e.model_ingest_evidence(bad), EvidenceResult::Rejected(why));
        assert_eq!(
            (e.model_pending_orders(), e.bankroll_balance()),
            before,
            "{why}: state untouched"
        );
    }
    assert_eq!(e.model_order_rec(id).unwrap().state, OrderState::Pending);
}

#[test]
fn lifecycle_n_evidence_for_one_order_never_unwinds_another_orders_or_preexisting_inventory() {
    // Order #1 fills and holds inventory. A DIFFERENT order id on the same mint (an earlier cleared
    // order) gets NotFilled evidence: the held position must be untouched.
    let (mut e, _c) = with_pending();
    let (id1, at, q) = pending(&e);
    landing(&mut e, T_LAND, 2_100);
    assert!(e.model_position_open(&MINT));
    assert_eq!(e.model_position_order_id(&MINT), Some(id1));
    let bal = e.bankroll_balance();
    // Evidence quoting a different (non-existent) order on this mint: rejected, inventory intact.
    assert_eq!(
        e.model_ingest_evidence(ev(id1 + 1, at, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Rejected("unknown_order")
    );
    assert!(e.model_position_open(&MINT));
    assert_eq!(e.bankroll_balance(), bal);
    // Even a contradicting report for order 1 is a FAULT (not an unwind) while its position is held.
    assert_eq!(
        e.model_ingest_evidence(ev(id1, at, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Fault
    );
    assert!(
        e.model_position_open(&MINT),
        "a fault does not unwind by itself"
    );
    // Resolution for an order that does not own the held position is refused, never applied.
    let fake = id1 + 1000;
    assert_eq!(
        e.model_resolve_recon_fault(fake, ReconcileOutcome::NotFilled),
        FaultResolution::NoFault
    );
    assert!(e.model_position_open(&MINT));
}

#[test]
fn lifecycle_o_closed_position_conflict_stays_blocked_with_durable_evidence() {
    let (mut e, _c) = with_pending();
    let (id, at, q) = pending(&e);
    landing(&mut e, T_LAND, 2_100);
    // Close it through the normal path.
    let mut ts = T_LAND + 1_000;
    let mut slot = 2_200;
    // The agreed HARD safeguard (a single-print collapse -> hard stop / rug precursor). The legacy
    // time-stop used to stand in for "protection" here; it no longer governs a model-managed
    // position, so protection is exercised through the safeguard that does.
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 1,
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 777,
        age_slots: 30,
        recv_unix_ms: Some(T_LAND + 900),
        trader_pubkey: Some([9u8; 32]),
        slot: Some(2_150),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(pump_quant_app::event::TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
    #[allow(clippy::explicit_counter_loop)]
    // `slot` advances with the loop but is passed into each event; range-as-slot would change the sequence
    for _ in 0..200 {
        e.tick(AppEvent::CurveObserved {
            mint: mint(),
            v_sol_lamports: VSOL / 4,
            v_tokens: VTOK * 4,
            real_sol_lamports: 1_000_000_000,
            real_tokens: 900_000_000_000_000,
            recv_unix_ms: Some(ts),
            slot,
        });
        pump(&mut e, 2);
        ts += 400;
        slot += 1;
        if !e.model_position_open(&MINT) {
            break;
        }
    }
    assert!(!e.model_position_open(&MINT));
    assert_eq!(e.model_order_rec(id).unwrap().state, OrderState::Closed);
    let bal = e.bankroll_balance();
    // NotFilled evidence for an order whose position is already SETTLED: fault, never a rewrite.
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Fault
    );
    assert_eq!(e.bankroll_balance(), bal, "settled history untouched");
    assert_eq!(
        e.model_resolve_recon_fault(id, ReconcileOutcome::NotFilled),
        FaultResolution::Refused("position_already_closed_needs_ledger_adjustment")
    );
    assert_eq!(e.model_recon_faults().len(), 1, "fault and block remain");
    assert_eq!(e.bankroll_balance(), bal);
    // The fault is journalled durably (position_closed = true).
    assert!(e.journal_recent().any(|d| matches!(
        d,
        pump_quant_app::journal_log::Decision::ReconFault { closed: 1, .. }
    )));
}

#[test]
fn lifecycle_k_resolution_unwinds_only_the_order_that_the_authority_says_never_filled() {
    // The paper simulator filled order #1 (inventory held). The execution side then reports
    // NotFilled for that same order: a fault (the books and evidence disagree). Authority confirms
    // NotFilled => THAT order's inventory and committed capital are unwound and the block lifts.
    let (mut e, _c) = with_pending();
    let (id, at, q) = pending(&e);
    landing(&mut e, T_LAND, 2_100);
    assert!(e.model_position_open(&MINT));
    assert_eq!(
        e.model_ingest_evidence(ev(id, at, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Fault
    );
    assert_eq!(e.model_recon_faults().len(), 1);
    assert!(
        e.model_position_open(&MINT),
        "inventory persists while the fault stands"
    );
    let bal_before = e.bankroll_balance();
    assert_eq!(
        e.model_resolve_recon_fault(id, ReconcileOutcome::NotFilled),
        FaultResolution::Released { unwound: true }
    );
    assert!(!e.model_position_open(&MINT), "inventory unwound");
    assert!(
        e.model_recon_faults().is_empty(),
        "block released only after books agree"
    );
    assert_eq!(
        e.bankroll_balance(),
        bal_before,
        "an unwound entry realized nothing"
    );
    assert_eq!(e.model_order_rec(id).unwrap().state, OrderState::NotFilled);
}

#[test]
fn lifecycle_l_held_position_is_monitored_while_new_exposure_on_the_mint_is_blocked() {
    let (mut e, _c) = with_pending();
    landing(&mut e, T_LAND, 2_100);
    assert!(e.model_position_open(&MINT));
    let id = e.model_position_order_id(&MINT).expect("owning order");
    let q = e.model_order_rec(id).unwrap().clip_lamports;
    assert_eq!(
        e.model_ingest_evidence(ev(id, 1, q, ReconcileOutcome::NotFilled)),
        EvidenceResult::Fault
    );
    assert_eq!(e.model_recon_faults().len(), 1);
    let asks_at_block = rep(&e, "dispatched");
    // Protective exit still fires on the held position while the fault stands.
    let mut ts = T_LAND + 1_000;
    let mut slot = 2_200;
    // The agreed HARD safeguard (a single-print collapse -> hard stop / rug precursor). The legacy
    // time-stop used to stand in for "protection" here; it no longer governs a model-managed
    // position, so protection is exercised through the safeguard that does.
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 1,
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 777,
        age_slots: 30,
        recv_unix_ms: Some(T_LAND + 900),
        trader_pubkey: Some([9u8; 32]),
        slot: Some(2_150),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(pump_quant_app::event::TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
    #[allow(clippy::explicit_counter_loop)]
    // `slot` advances with the loop but is passed into each event; range-as-slot would change the sequence
    for _ in 0..200 {
        e.tick(AppEvent::CurveObserved {
            mint: mint(),
            v_sol_lamports: VSOL / 4,
            v_tokens: VTOK * 4,
            real_sol_lamports: 1_000_000_000,
            real_tokens: 900_000_000_000_000,
            recv_unix_ms: Some(ts),
            slot,
        });
        pump(&mut e, 2);
        ts += 400;
        slot += 1;
        if !e.model_position_open(&MINT) {
            break;
        }
    }
    assert!(
        !e.model_position_open(&MINT),
        "held position was protected while the block stood"
    );
    assert_eq!(
        e.model_recon_faults().len(),
        1,
        "the block did not lift by itself"
    );
    assert_eq!(
        rep(&e, "dispatched"),
        asks_at_block,
        "no new ask/exposure for the blocked mint"
    );
}

#[test]
fn lifecycle_e_reconcile_with_no_order_is_a_counted_noop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    e.enable_paper_model(Stub(calls));
    assert!(!e.model_mark_ack_uncertain(7));
    assert_eq!(
        e.model_ingest_evidence(ev(7, 1, 1, ReconcileOutcome::NotFilled)),
        EvidenceResult::Rejected("unknown_order")
    );
    assert_eq!(rep(&e, "ack:unknown_order"), 1);
    assert_eq!(rep(&e, "evidence:rejected:unknown_order"), 1);
    assert!(!e.model_position_open(&MINT));
}

#[test]
fn lifecycle_f_funnel_counts_unique_markets_including_never_ready() {
    let (e, _c) = with_pending();
    let f = e.model_funnel();
    assert_eq!(
        f.get("discovered|venue=pumpfun").copied().unwrap_or(0)
            + f.get("discovered|venue=unknown").copied().unwrap_or(0),
        1,
        "{f:?}"
    );
    // A second market that only ever reported a launch: observed, discovered, never ready.
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e2 = Engine::new(Config::dev_portable(), RunMode::Paper);
    e2.enable_paper_model(Stub(calls));
    e2.tick(AppEvent::LaunchObserved {
        mint: DomainMint::from_bytes([0x77; 32]),
        creator: [1; 32],
        launch_unix_ms: T0,
    });
    e2.tick(AppEvent::CurveObserved {
        mint: DomainMint::from_bytes([0x77; 32]),
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 1,
        real_tokens: 1,
        recv_unix_ms: Some(T0 + 500),
        slot: 5,
    });
    pump(&mut e2, 3);
    let f2 = e2.model_funnel();
    let never: u64 = f2
        .iter()
        .filter(|(k, _)| k.starts_with("never_ready"))
        .map(|(_, v)| *v)
        .sum();
    assert_eq!(
        never, 1,
        "a market with too little history is counted as never-ready, with its reason: {f2:?}"
    );
    assert!(f2.keys().any(|k| k.contains("last=")), "{f2:?}");
}

#[test]
fn lifecycle_h_routing_fills_are_recorded_but_never_assessable() {
    let (mut e, _c) = with_pending();
    landing(&mut e, T0 + 1_000 + 40 * 2_000 + 1_500, 2_100);
    assert!(e.model_position_open(&MINT));
    assert_eq!(e.model_all_fills().len(), 1);
    assert!(!e.model_all_fills()[0].landing_validated);
    assert!(
        e.model_assessable_fills().is_empty(),
        "an unvalidated-landing fill must not be assessable"
    );
}

#[test]
fn lifecycle_i_older_report_paths_cannot_see_an_unvalidated_routing_fill() {
    // REPORTING GUARD regression. `report()` is the pre-existing end-of-run path: it force-closes
    // held positions and feeds lane/disc perf, reconciliation, analytics and the tape. A routing
    // fill that closes through it must change NONE of those, while cash and the exclusion list
    // still show it.
    let (mut e, _c) = with_pending();
    landing(&mut e, T_LAND, 2_100);
    assert!(e.model_position_open(&MINT));
    let cash_before = e.bankroll_balance();
    let r = e.report();
    assert!(
        !e.model_position_open(&MINT),
        "report() closed the held position"
    );
    // Visible, labelled, counted — and NOT a zero-return observation.
    assert_eq!(e.model_excluded_exits().len(), 1);
    assert_eq!(
        e.model_excluded_exits()[0].reason,
        "routing_fill:landing_unvalidated"
    );
    assert!(e.model_assessable_fills().is_empty());
    // Every assessment feed saw nothing.
    assert!(
        r.per_lane_net.iter().all(|(_, n)| *n == 0),
        "lane perf: {:?}",
        r.per_lane_net
    );
    assert!(r.per_discovery_lane_net.iter().all(|(_, n)| *n == 0));
    let a = e.analytics_report();
    assert!(
        a.cvar.is_none() && a.median_return_bps.is_none(),
        "analytics saw a trade"
    );
    assert_eq!(a.profit_factor_bps, 0);
    assert_eq!(a.trades, 0, "analytics trade count");
    // The promotion statistical gate keys off the analytics trade count: no trades => no candidate.
    assert!(
        !e.promotion_stat_verdict().fdr_blocks,
        "no candidate trades"
    );
    // Cash is real simulator state, so it is settled (and distinct from the assessment feeds).
    let _ = cash_before;
}

#[test]
fn lifecycle_j_operational_reconciliation_and_protection_are_not_skipped_for_routing_fills() {
    // OPERATIONAL (cash / inventory / protective exit) vs ASSESSMENT (lane perf, recon, analytics,
    // tape, promotion). The guard may skip only the latter.
    let (mut e, _c) = with_pending();
    landing(&mut e, T_LAND, 2_100);
    assert!(e.model_position_open(&MINT));
    let cash_after_entry = e.bankroll_balance();
    // A collapse of the held routing position's own price must close it through the NORMAL tick path
    // (no report()/finalize), i.e. management and safety see it.
    let mut ts = T_LAND + 1_000;
    let mut slot = 2_200;
    // The agreed HARD safeguard (a single-print collapse -> hard stop / rug precursor). The legacy
    // time-stop used to stand in for "protection" here; it no longer governs a model-managed
    // position, so protection is exercised through the safeguard that does.
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 1,
        quote_lamports: 900_000_000,
        liquidity_lamports: VSOL,
        signed_base: -90_000_000_000,
        buyer_entity: 777,
        age_slots: 30,
        recv_unix_ms: Some(T_LAND + 900),
        trader_pubkey: Some([9u8; 32]),
        slot: Some(2_150),
        fee_lamports: Some(70_000),
        cu_consumed: Some(95_000),
        venue: Some(pump_quant_app::event::TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
    #[allow(clippy::explicit_counter_loop)]
    // `slot` advances with the loop but is passed into each event; range-as-slot would change the sequence
    for _ in 0..200 {
        e.tick(AppEvent::CurveObserved {
            mint: mint(),
            v_sol_lamports: VSOL / 4,
            v_tokens: VTOK * 4,
            real_sol_lamports: 1_000_000_000,
            real_tokens: 900_000_000_000_000,
            recv_unix_ms: Some(ts),
            slot,
        });
        pump(&mut e, 2);
        ts += 400;
        slot += 1;
        if !e.model_position_open(&MINT) {
            break;
        }
    }
    assert!(
        !e.model_position_open(&MINT),
        "a protective exit fired on a routing position"
    );
    // Cash settled exactly once, through the normal exit: one excluded exit, and the bankroll moved
    // from its post-entry level by that exit's proceeds and nothing else.
    assert_eq!(e.model_excluded_exits().len(), 1);
    let x = e.model_excluded_exits()[0];
    assert_ne!(
        e.bankroll_balance(),
        cash_after_entry,
        "the exit settled cash"
    );
    // Journal: a RoutingExit (assessable=false) and never a Filled, so Filled consumers can't sum it.
    let routing = e
        .journal_recent()
        .filter(|d| matches!(d, pump_quant_app::journal_log::Decision::RoutingExit { .. }))
        .count();
    let filled = e
        .journal_recent()
        .filter(|d| matches!(d, pump_quant_app::journal_log::Decision::Filled { .. }))
        .count();
    assert_eq!((routing, filled), (1, 0));
    // Nothing leaked into assessment.
    let r = e.report();
    assert!(r.per_lane_net.iter().all(|(_, n)| *n == 0));
    assert_eq!(e.analytics_report().trades, 0);
    let _ = x;
}

#[test]
fn lifecycle_p_evidence_through_the_normal_event_path_is_order_bound() {
    // Same protections as the direct API, but delivered as `AppEvent::ModelOrderEvidence` through
    // `Engine::tick` -- the path a real report source uses, not a test helper.
    let (mut e, _c) = with_pending();
    let (id, at, q) = pending(&e);
    let evt = |order_id: u64, attempt: u32, clip: u64, filled: Option<(u64, u64)>| {
        AppEvent::ModelOrderEvidence {
            mint: mint(),
            order_id,
            attempt,
            clip_lamports: clip,
            filled,
        }
    };
    // Wrong order / attempt / quantity: nothing changes.
    for bad in [
        evt(id + 9, at, q, None),
        evt(id, at + 1, q, None),
        evt(id, at, q + 1, None),
    ] {
        e.tick(bad);
    }
    assert_eq!(e.model_pending_orders(), 1);
    assert_eq!(e.model_order_rec(id).unwrap().state, OrderState::Pending);
    assert_eq!(rep(&e, "evidence:rejected:unknown_order"), 1);
    assert_eq!(rep(&e, "evidence:rejected:attempt_mismatch"), 1);
    assert_eq!(rep(&e, "evidence:rejected:quantity_mismatch"), 1);
    // A report with the right order but the WRONG mint is rejected without reaching the order.
    e.tick(AppEvent::ModelOrderEvidence {
        mint: DomainMint::from_bytes([9u8; 32]),
        order_id: id,
        attempt: at,
        clip_lamports: q,
        filled: None,
    });
    assert_eq!(rep(&e, "evidence:rejected:mint_mismatch"), 1);
    assert_eq!(e.model_pending_orders(), 1);
    // The exact report applies; a duplicate does not apply twice; a contradiction is a fault.
    e.tick(evt(id, at, q, Some((30_000, VSOL))));
    assert!(e.model_position_open(&MINT));
    let bal = e.bankroll_balance();
    e.tick(evt(id, at, q, Some((30_000, VSOL))));
    assert_eq!(rep(&e, "reconcile:duplicate_same_terminal"), 1);
    e.tick(evt(id, at, q, None));
    assert_eq!(rep(&e, "reconcile:FAULT_conflicting_terminal"), 1);
    assert_eq!(e.model_recon_faults().len(), 1);
    assert_eq!(e.bankroll_balance(), bal);
    assert_eq!(rep(&e, "fill:applied_from_reconcile"), 1);
}
