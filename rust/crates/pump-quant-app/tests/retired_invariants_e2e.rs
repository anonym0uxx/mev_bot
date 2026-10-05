//! Invariants that the 85 retired legacy-strategy tests asserted ALONGSIDE their strategy comparisons, re-expressed on the
//! MODEL path (the only entry authority). Nothing here restores legacy behaviour or pins a strategy outcome; each test names
//! the retired test it replaces in `docs/consolidation_assertion_audit.json`.
//!
//! Synthetic events: these prove plumbing and refusals. They are NOT evidence of profitability.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::journal_log::Decision;
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
const SKIP: &str = "DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: x";

struct Stub {
    text: &'static str,
    calls: Arc<AtomicUsize>,
}

impl ModelSource for Stub {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.text.to_string())
    }
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
    c
}

/// Launch + priced prints + a curve observation + an on-chain confirm carrying the given reserve pair.
fn events(confirm_virtual: u64, confirm_real: u64) -> Vec<AppEvent> {
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
        });
    }
    let t_last = T0 + 1_000 + 40 * 2_000;
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
        virtual_sol_lamports: confirm_virtual,
        real_sol_lamports: confirm_real,
    });
    ev
}

fn drive(e: &mut Engine, evs: &[AppEvent]) {
    for ev in evs {
        e.tick(*ev);
    }
    for _ in 0..8 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn landing(e: &mut Engine) {
    let t_last = T0 + 1_000 + 40 * 2_000;
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(t_last + 1_500),
        slot: 2_100,
    });
    for _ in 0..6 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn armed(text: &'static str) -> Engine {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Stub {
        text,
        calls: Arc::new(AtomicUsize::new(0)),
    });
    e
}

fn admitted_records(e: &Engine) -> Vec<Decision> {
    e.journal()
        .recent()
        .filter(|d| matches!(d, Decision::Admitted { .. }))
        .copied()
        .collect()
}

// ---- (1) decoder-boundary refusal -------------------------------------------------------------------
// Replaces phase_a_alignment::an_inflated_depth_claim_is_refused_not_clamped. A confirm whose two
// reserves contradict real_sol = virtual_sol - 30 SOL is a BROKEN DECODE: it is never recorded, so the
// market has no confirmation. On the model path the gate no longer exists, so the observable effect is
// the confirmed-market set, which feeds candidate ordering; the refusal itself is `confirm()`.

fn confirmed_after(confirm_virtual: u64, confirm_real: u64) -> bool {
    let mut e = armed(SKIP);
    drive(&mut e, &events(confirm_virtual, confirm_real));
    e.is_market_confirmed(&MINT)
}

#[test]
fn an_honest_reserve_pair_is_recorded_and_a_contradictory_one_is_refused_not_clamped() {
    const REAL_VSOL: u64 = 30_300_000_000;
    const ESCROWED: u64 = REAL_VSOL - 30_000_000_000;
    assert!(
        confirmed_after(REAL_VSOL, ESCROWED),
        "an honest pair must be recorded"
    );
    // the price reserve passed off as payout capacity
    assert!(
        !confirmed_after(REAL_VSOL, REAL_VSOL),
        "inflated claim must be refused, not clamped"
    );
    assert!(
        !confirmed_after(REAL_VSOL, REAL_VSOL * 100),
        "a claim 100x the curve must be refused"
    );
    let tol = pump_quant_app::curve_depth::cross_check_tolerance_lamports(ESCROWED);
    assert!(
        confirmed_after(REAL_VSOL, ESCROWED - tol),
        "drift inside tolerance is fee noise"
    );
    assert!(
        confirmed_after(REAL_VSOL, ESCROWED + tol),
        "drift inside tolerance is fee noise"
    );
    assert!(
        !confirmed_after(REAL_VSOL, ESCROWED + tol + 1),
        "one lamport past tolerance is refused"
    );
}

// ---- (2) the Admitted journal record on a MODEL entry -------------------------------------------------
// Replaces batch_2d_laws::admitted_record_carries_band_and_provenance. A model entry has no economic band
// (the brain decided; arbitration is bypassed), so the band fields are 0, not fabricated, and depth is
// provenance 2 (decoded) - never 0/unknown on an admitted trade.

#[test]
fn a_model_entry_writes_an_admitted_record_with_honest_zero_band_and_decoded_depth_provenance() {
    let mut e = armed(BUY);
    drive(&mut e, &events(VSOL, 7_900_000_000));
    landing(&mut e);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    let recs = admitted_records(&e);
    assert_eq!(
        recs.len(),
        1,
        "exactly one Admitted record per opened model position: {recs:?}"
    );
    let Decision::Admitted {
        mint,
        size_lamports,
        x_min,
        x_cost,
        x_max,
        rt_cost_bps,
        move_source,
        depth_basis,
        ..
    } = recs[0]
    else {
        unreachable!()
    };
    assert_eq!(mint, MINT);
    assert!(size_lamports > 0, "a positive clip was admitted");
    assert_eq!(
        (x_min, x_cost, x_max),
        (0, 0, 0),
        "no band exists for a model entry; none may be invented"
    );
    assert!(
        rt_cost_bps > 0,
        "a real round-trip cost was measured at the admitted size"
    );
    assert!(move_source <= 2, "move source is a known estimator code");
    assert!(
        depth_basis == 2 || depth_basis == 3,
        "never unknown(0) on an admitted trade: {depth_basis}"
    );
    let r = e.report();
    assert_eq!(r.admitted, 1);
}

// ---- (3) reject-code history + code 29 -----------------------------------------------------------------
// Replaces holder_concentration::veto_reject_code_17_actually_reaches_the_journal (the veto is gone; the
// numbering is not) and pins the new code. With no model armed, a promoted candidate is refused by code
// 29 and counted in exactly one slot, so sum(reject_counts) == rejected.

#[test]
fn with_no_model_armed_a_promoted_candidate_is_refused_by_code_29_and_counted_exactly_once() {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    for ev in events(VSOL, 7_900_000_000) {
        e.tick(ev);
    }
    for _ in 0..30 {
        e.tick(AppEvent::Tick);
    }
    let r = e.report();
    let ls = e.live_status();
    assert_eq!(r.admitted, 0, "no entry authority, no entry");
    assert!(
        ls.reject_counts[29] > 0,
        "the refusal is attributed to code 29: {:?}",
        ls.reject_counts
    );
    assert_eq!(
        ls.reject_counts.iter().sum::<u64>(),
        ls.rejected,
        "histogram sums to the rejected counter"
    );
    for code in 0..29usize {
        assert_eq!(
            ls.reject_counts[code], 0,
            "frozen historical code {code} is never emitted now"
        );
    }
    assert!(e
        .journal()
        .recent()
        .any(|d| matches!(d, Decision::Rejected { reason: 29, .. })));
    assert!(e
        .journal()
        .recent()
        .all(|d| !matches!(d, Decision::Admitted { .. })));
}

// ---- (4) wallet floor on the model entry fill ---------------------------------------------------------
// Replaces the floor/deployable assertions of engine_e2e::bankroll_sizing_scales_with_capital_and_respects_the_floor.
// The model fill is refused by name when the all-in cost would breach the survival floor; no silent resize.

fn floor_run(floor_bps: u32) -> (bool, std::collections::BTreeMap<String, u64>, u64) {
    let mut c = cfg();
    c.floor_fraction_bps = floor_bps;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Stub {
        text: BUY,
        calls: Arc::new(AtomicUsize::new(0)),
    });
    drive(&mut e, &events(VSOL, 7_900_000_000));
    landing(&mut e);
    (
        e.model_position_open(&MINT),
        e.model_lane_report().clone(),
        e.bankroll_balance(),
    )
}

#[test]
fn a_model_entry_with_no_deployable_capital_is_refused_by_name_and_the_wallet_is_untouched() {
    let (open_lo, _rep_lo, bal_lo) = floor_run(2_500);
    assert!(
        open_lo,
        "control: the same entry fills under the normal floor"
    );
    let (open_hi, rep_hi, bal_hi) = floor_run(9_800);
    assert!(
        !open_hi,
        "a floor that leaves no deployable capital must not open a position: {rep_hi:?}"
    );
    // Measured: with no deployable capital the refusal fires at ORDER creation as a named no-trade
    // (`notrade:unpayable_clip`). The second layer, `fill_none:below_wallet_floor` at the fill, is only
    // reachable when cash falls between order and fill; it has no test of its own (recorded as a gap).
    assert!(
        rep_hi.get("notrade:unpayable_clip").copied().unwrap_or(0) >= 1,
        "the refusal must be NAMED, not silent: {rep_hi:?}"
    );
    assert_eq!(
        bal_hi, 2_000_000_000,
        "a refused fill leaves cash exactly at the seed"
    );
    assert_eq!(
        bal_lo, bal_hi,
        "balance is the seed either way until a realized exit"
    );
}

// ---- (5) legacy fee/tip config fields cannot reach a model entry --------------------------------------
// Replaces one_authority_laws::the_retired_fee_fields_cannot_reach_a_decision. The four legacy fields
// survive only so an old operator config still parses; the venue fee is a function of the market.

fn entry_record_with(tweak: fn(&mut Config)) -> (Decision, u64) {
    let mut c = cfg();
    tweak(&mut c);
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Stub {
        text: BUY,
        calls: Arc::new(AtomicUsize::new(0)),
    });
    drive(&mut e, &events(VSOL, 7_900_000_000));
    landing(&mut e);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    (admitted_records(&e)[0], e.bankroll_balance())
}

#[test]
fn absurd_legacy_fee_and_tip_fields_do_not_change_a_model_entry() {
    let base = entry_record_with(|_| {});
    let absurd = entry_record_with(|c| {
        c.entry_fee_bps = 9_999;
        c.exit_fee_bps = 9_999;
        c.entry_tip_lamports = 500_000_000;
        c.exit_tip_lamports = 500_000_000;
    });
    assert_eq!(
        base, absurd,
        "a retired fee/tip field reached the model entry (size, cost, or cash moved)"
    );
}

// ---- (6) no config state restores legacy entry authority ---------------------------------------------
// With NO model armed, no combination of legacy knobs may open a position: the gate is deleted, so the only
// outcome is the named refusal (code 29). Extreme values are used on purpose.

#[test]
fn no_legacy_config_state_can_open_a_position_without_a_model() {
    let mut c = cfg();
    c.promote_k = 10_000;
    c.gate_fail_rate_bps = 0;
    c.min_trade_size_lamports = 1;
    c.entry_fee_bps = 0;
    c.exit_fee_bps = 0;
    c.entry_tip_lamports = 0;
    c.exit_tip_lamports = 0;
    c.expected_move_model_enable = true;
    c.brain_haircut_enable = true;
    c.money_proxy_enable = true;
    c.curve_exact_fill_enable = true;
    let mut e = Engine::new(c, RunMode::Paper);
    for ev in events(VSOL, 7_900_000_000) {
        e.tick(ev);
    }
    for _ in 0..40 {
        e.tick(AppEvent::Tick);
    }
    let r = e.report();
    assert_eq!(r.admitted, 0, "a legacy knob regained entry authority");
    assert!(!e.model_position_open(&MINT));
    assert_eq!(e.bankroll_balance(), 2_000_000_000, "cash untouched");
    assert!(
        admitted_records(&e).is_empty(),
        "no Admitted record may be written without a model"
    );
}
