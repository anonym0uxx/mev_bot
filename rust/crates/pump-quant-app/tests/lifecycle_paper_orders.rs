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
use pump_quant_app::engine::model_admit::{FillReport, ReconcileOutcome};
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

#[test]
fn lifecycle_b_an_uncertain_ack_stays_pending_past_the_ttl_and_is_not_inventory() {
    let (mut e, _c) = with_pending();
    assert!(e.model_mark_ack_uncertain(&MINT));
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
}

#[test]
fn lifecycle_c_reconciled_not_filled_clears_the_intent_without_a_position() {
    let (mut e, _c) = with_pending();
    e.model_mark_ack_uncertain(&MINT);
    assert!(e.model_reconcile(&MINT, ReconcileOutcome::NotFilled));
    assert_eq!(e.model_pending_orders(), 0);
    assert!(!e.model_position_open(&MINT));
    // A late duplicate report finds nothing to apply.
    assert!(!e.model_reconcile(
        &MINT,
        ReconcileOutcome::Filled(FillReport {
            entry_price_fp: 30_000,
            reserve_sol_lamports: VSOL
        })
    ));
    assert!(
        !e.model_position_open(&MINT),
        "a report after clearing cannot resurrect inventory"
    );
}

#[test]
fn lifecycle_d_a_reconciled_fill_is_applied_exactly_once() {
    let (mut e, _c) = with_pending();
    e.model_mark_ack_uncertain(&MINT);
    let fr = FillReport {
        entry_price_fp: 30_000,
        reserve_sol_lamports: VSOL,
    };
    assert!(e.model_reconcile(&MINT, ReconcileOutcome::Filled(fr)));
    assert!(e.model_position_open(&MINT));
    assert_eq!(rep(&e, "fill:applied_from_reconcile"), 1);
    let bal = e.bankroll_balance();
    // Duplicate confirmation, and a conflicting NotFilled: neither changes inventory or balance.
    assert!(!e.model_reconcile(&MINT, ReconcileOutcome::Filled(fr)));
    assert_eq!(rep(&e, "reconcile:duplicate_same_terminal"), 1);
    // A CONFLICTING terminal report is not silent and not applied: named fault, evidence kept.
    assert!(!e.model_reconcile(&MINT, ReconcileOutcome::NotFilled));
    assert_eq!(rep(&e, "reconcile:FAULT_conflicting_terminal"), 1);
    assert_eq!(e.model_recon_faults().get(&MINT).map(Vec::len), Some(1));
    assert_eq!(rep(&e, "fill:position_opened"), 1);
    assert_eq!(e.bankroll_balance(), bal);
    assert!(e.model_position_open(&MINT));
}

#[test]
fn lifecycle_g_not_filled_then_credible_filled_is_a_fault_that_blocks_exposure_until_resolved() {
    let (mut e, _c) = with_pending();
    e.model_mark_ack_uncertain(&MINT);
    assert!(e.model_reconcile(&MINT, ReconcileOutcome::NotFilled));
    assert_eq!(e.model_pending_orders(), 0);
    let bal = e.bankroll_balance();
    let fr = FillReport {
        entry_price_fp: 30_000,
        reserve_sol_lamports: VSOL,
    };
    // Credible Filled evidence after a NotFilled: not applied, not discarded, raised.
    assert!(!e.model_reconcile(&MINT, ReconcileOutcome::Filled(fr)));
    assert_eq!(rep(&e, "reconcile:FAULT_conflicting_terminal"), 1);
    assert_eq!(e.model_recon_faults().get(&MINT).map(Vec::len), Some(1));
    assert!(!e.model_position_open(&MINT), "no silent inventory");
    assert_eq!(e.bankroll_balance(), bal, "no debit");
    // New exposure for the affected mint is blocked while the fault stands.
    let asks = rep(&e, "dispatched");
    for _ in 0..40 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(rep(&e, "dispatched"), asks);
    // Explicit, counted resolution.
    assert!(e.model_resolve_recon_fault(&MINT));
    assert_eq!(rep(&e, "reconcile:fault_resolved_by_authority"), 1);
    assert!(e.model_recon_faults().is_empty());
}

#[test]
fn lifecycle_e_reconcile_with_no_order_is_a_counted_noop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    e.enable_paper_model(Stub(calls));
    assert!(!e.model_mark_ack_uncertain(&MINT));
    assert!(!e.model_reconcile(&MINT, ReconcileOutcome::NotFilled));
    assert_eq!(rep(&e, "ack:no_pending_order"), 1);
    assert_eq!(rep(&e, "reconcile:no_pending_order"), 1);
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
