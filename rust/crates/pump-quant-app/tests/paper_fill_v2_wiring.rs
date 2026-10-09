//! `paper_fill_v2_shadow` + shared settlement WIRED into the real engine (`Engine::tick`).
//!
//! Synthetic events, real engine path: entry BUY -> fill -> 60 s -> management verdict (stub keyed on prompt content)
//! -> REDUCE / EXIT order -> landing-state paper fill. Curve snapshots keep `vsol - real_sol` and `vtok - real_tok`
//! constant (a canonical curve: other participants' trades are constant-product), except where a test deliberately
//! changes the virtual SOL offset (the Mayhem-mode pattern) to provoke the named divergence.
//! Proves wiring and accounting, NOT profitability.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_app::shadow_pool::{Divergence, LegKind, PaperFillVersion};
use pump_quant_app::stop_policy::{OpsInputs, StopTrigger};
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
/// Canonical curve offsets: vsol - real_sol = 30 SOL, vtok - real_tok = 280e12.
const VOFF: u64 = 30_000_000_000;
const TOFF: u64 = 280_000_000_000_000;
const RSOL0: u64 = 7_900_000_000;
const RTOK0: u64 = 569_000_000_000_000;

fn k() -> u128 {
    u128::from(VOFF + RSOL0) * u128::from(TOFF + RTOK0)
}

/// A constant-product curve state with real SOL `rsol` and the canonical offsets.
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

fn curve_ev(ts: i64, slot: u64, rsol: u64, voff_shift: u64) -> AppEvent {
    let (vsol, vtok, rs, rt) = state(rsol);
    AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: vsol - voff_shift,
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
        signed_base: if i % 3 != 0 {
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

struct Rig {
    e: Engine,
    clock: i64,
    slot: u64,
    n: u32,
    rsol: u64,
}

fn engine(v: Option<PaperFillVersion>, answer: fn(i64) -> &'static str) -> Engine {
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Script {
        calls: Arc::new(AtomicUsize::new(0)),
        answer,
    });
    if let Some(v) = v {
        e.model_set_paper_fill(v);
    }
    e
}

fn rig(
    v: Option<PaperFillVersion>,
    answer: fn(i64) -> &'static str,
    held: Option<&std::path::Path>,
) -> Rig {
    let mut e = engine(v, answer);
    if let Some(h) = held {
        e.model_held_attach(h);
    }
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
    e.tick(curve_ev(t_last, 2_000, RSOL0, 0));
    e.tick(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VOFF + RSOL0,
        real_sol_lamports: RSOL0,
    });
    ticks(&mut e, 8);
    let rsol = RSOL0 + 200_000_000;
    e.tick(curve_ev(t_last + 1_500, 2_100, rsol, 0));
    ticks(&mut e, 6);
    assert!(
        e.model_position_open(&MINT),
        "setup: the entry must have filled: {:?}",
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
    fn step(&mut self, ms: i64) {
        self.clock += ms;
        self.slot += 1;
        self.n += 1;
        self.e.tick(curve_ev(self.clock, self.slot, self.rsol, 0));
        self.e.tick(trade(
            self.n,
            self.clock,
            self.slot,
            45_300 + i128::from(self.n % 7),
        ));
        ticks(&mut self.e, 2);
    }
    fn advance_to_order(&mut self, max_ms: i64) {
        let end = self.clock + max_ms;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.step(1_000);
        }
    }
    fn landing(&mut self) {
        self.clock += 1_000;
        self.slot += 5;
        self.rsol += 50_000_000;
        self.e.tick(curve_ev(self.clock, self.slot, self.rsol, 0));
        ticks(&mut self.e, 6);
    }
    fn rep(&self, k: &str) -> u64 {
        self.e
            .model_lane_report()
            .iter()
            .filter(|(key, _)| key.starts_with(k))
            .map(|(_, v)| *v)
            .sum()
    }
    /// Settlement holding == engine reconciled inventory, and the settlement invariant, at this instant.
    fn assert_books_agree(&self, when: &str) {
        let l = self
            .e
            .model_settlement()
            .expect("v2 has a settlement ledger");
        assert!(l.invariant_holds(), "{when}: cash+committed==seed+realized");
        let st = l.holdings.get(&MINT).map_or(0, |h| h.tokens);
        let inv = self.e.model_inventory_tokens(&MINT).unwrap_or(0);
        assert_eq!(st, inv, "{when}: settlement holding == engine inventory");
        assert!(
            self.e.model_settlement_faults().is_empty(),
            "{when}: no settlement fault: {:?}",
            self.e.model_settlement_faults()
        );
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pfv2_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("held.json")
}

#[test]
fn v1_default_runs_no_settlement_and_writes_no_paper_fill_section() {
    let h = tmp("v1");
    let mut r = rig(None, |_| HOLD, Some(&h));
    assert_eq!(r.e.model_paper_fill(), PaperFillVersion::V1Observed);
    assert!(r.e.model_settlement().is_none());
    assert!(r.e.model_shadow_book().markets.is_empty());
    assert_eq!(r.rep("settle:"), 0);
    assert_eq!(r.rep("shadow:"), 0);
    assert!(r.e.model_held_persist_now());
    let raw = std::fs::read_to_string(&h).unwrap();
    assert!(
        !raw.contains("paper_fill"),
        "a v1 ledger is unchanged: no paper_fill section"
    );
    r.step(1_000);
}

#[test]
fn v2_buy_reduce_exit_every_increment_settles_once_and_the_sell_is_priced_on_the_shadow() {
    let mut r = rig(
        Some(PaperFillVersion::V2Shadow),
        |s| if s == 0 { REDUCE } else { EXIT },
        None,
    );
    // Entry: settled through the one path, and the shadow carries our delta.
    assert_eq!(
        r.rep("settle:applied:entry"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
    r.assert_books_agree("after entry");
    let sm =
        r.e.model_shadow_book()
            .market(&MINT)
            .expect("shadow market opened by our fill")
            .clone();
    assert!(sm.sol_in > 0 && sm.tok_out > 0 && sm.diverged.is_none());
    let l0 = r.e.model_settlement().unwrap().clone();
    let entry = r.e.model_accounting_view(&MINT);
    assert_eq!(
        l0.committed,
        i128::from(entry.committed),
        "settlement committed == engine committed entry cost (clip + network + tip + ATA)"
    );
    // REDUCE.
    r.advance_to_order(120_000);
    let (_, kind, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE order pending");
    assert_eq!(format!("{kind:?}"), "Reduce");
    r.landing();
    assert_eq!(
        r.e.model_mgmt_fills().len(),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
    let f0 = r.e.model_mgmt_fills()[0];
    assert_eq!(f0.tokens, intended);
    assert_eq!(r.rep("settle:applied:sell"), 1);
    r.assert_books_agree("after REDUCE");
    // The external benchmark (observed state alone) is reported apart and differs from our shadow-priced gross:
    // our own buy's SOL is in the shadow reserves, so the same size sells for MORE than the observed state alone.
    let ext = r.rep("shadow:sell_external_benchmark_gross");
    assert!(
        ext > 0,
        "external benchmark reported: {:?}",
        r.e.model_lane_report()
    );
    assert!(
        f0.gross_lamports > ext,
        "shadow gross {} must exceed the observed-only benchmark {ext} (our delta is carried)",
        f0.gross_lamports
    );
    let sm1 = r.e.model_shadow_book().market(&MINT).unwrap();
    assert_eq!(
        sm1.sol_out,
        u128::from(f0.gross_lamports),
        "our sell gross left the shadow reserves"
    );
    assert_eq!(sm1.tok_in, u128::from(f0.tokens));
    // EXIT.
    r.advance_to_order(200_000);
    r.landing();
    assert!(
        !r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.rep("settle:applied:sell"), 2);
    r.assert_books_agree("after EXIT");
    let l = r.e.model_settlement().unwrap();
    assert!(l.holdings.is_empty() && l.committed == 0);
    let acct = r.e.model_accounting_view(&MINT);
    assert_eq!(
        l.realized, acct.realized,
        "flat: settlement realized == engine realized exactly (same gross, fees, cost)"
    );
    assert_eq!(l.cash, l.seed + l.realized);
    assert_eq!(
        l.network_estimate,
        3 * i128::from(pump_quant_app::exec_quote::NETWORK_FEE_P50_LAMPORTS),
        "one labelled network estimate per landed leg (BUY, REDUCE, EXIT)"
    );
    // A closed market's shadow is forgotten.
    assert!(r.e.model_shadow_book().market(&MINT).is_none());
}

#[test]
fn v2_virtual_offset_change_is_a_named_divergence_that_latches_safety_off_and_preserves_books() {
    let mut r = rig(Some(PaperFillVersion::V2Shadow), |_| HOLD, None);
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    // Healthy before: no divergence, the stop table raises nothing shadow-related, estimator is the shadow.
    let ev = r.e.model_stop_evaluate(1_000, OpsInputs::healthy());
    assert!(!ev
        .raised
        .iter()
        .any(|(t, _)| *t == StopTrigger::ShadowDivergence));
    assert_eq!(ev.estimator, pump_quant_app::stop_policy::ESTIMATOR_SHADOW);
    assert!(!r.e.model_safety_blocked());
    // Mayhem-pattern snapshot: virtual SOL re-parameterised (vsol - real_sol moves), token offset unchanged.
    r.clock += 500;
    r.slot += 1;
    r.e.tick(curve_ev(r.clock, r.slot, r.rsol, 2_000_000_000));
    assert_eq!(
        r.e.model_shadow_held_divergence(),
        Some((MINT, Divergence::CurveVirtualOffsetChanged))
    );
    assert_eq!(r.rep("shadow_divergence:curve_virtual_offset_changed"), 1);
    let ev = r.e.model_stop_evaluate(2_000, OpsInputs::healthy());
    assert!(
        ev.newly_raised.contains(&StopTrigger::ShadowDivergence),
        "{:?}",
        ev.raised
    );
    assert!(r.e.model_safety_blocked());
    assert_eq!(r.e.model_safety_reason(), "shadow_divergence");
    // Books preserved: nothing closed, inventory and settlement unchanged; the valuation is UNKNOWN by name.
    assert!(r.e.model_position_open(&MINT));
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv0));
    r.assert_books_agree("after divergence");
    let est = r.e.model_shadow_liquidation_estimates();
    assert_eq!(est.len(), 1);
    assert_eq!(
        est[0].value,
        Err("shadow_divergence:curve_virtual_offset_changed")
    );
    // Permanent per segment: a later consistent snapshot does not revive the delta.
    r.step(1_000);
    assert_eq!(
        r.e.model_shadow_book().divergence(&MINT),
        Some(Divergence::CurveVirtualOffsetChanged)
    );
}

#[test]
fn v2_restart_restores_shadow_and_settlement_exactly_and_refuses_mismatched_books() {
    let h = tmp("v2r");
    let mut r = rig(Some(PaperFillVersion::V2Shadow), |_| HOLD, Some(&h));
    r.step(1_000);
    assert!(r.e.model_held_persist_now());
    let book = r.e.model_shadow_book().clone();
    let settle = r.e.model_settlement().unwrap().clone();
    // Fresh v2 engine: exact restore.
    let mut e2 = engine(Some(PaperFillVersion::V2Shadow), |_| HOLD);
    e2.model_held_attach(&h);
    e2.model_held_restore()
        .expect("restore ok")
        .expect("ledger present");
    assert_eq!(e2.model_shadow_book(), &book);
    assert_eq!(e2.model_settlement(), Some(&settle));
    // The settlement idempotency survives: the same entry increment again is a duplicate, not a second buy.
    // A v1 engine refuses the v2 books (they were kept under a different fill model).
    let mut e1 = engine(None, |_| HOLD);
    e1.model_held_attach(&h);
    let err = e1.model_held_restore().unwrap_err();
    assert!(
        format!("{err:?}").contains("PaperFillVersionMismatch"),
        "{err:?}"
    );
    // Tampered settlement holding: refused by name, nothing applied.
    let raw = std::fs::read_to_string(&h).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let hold = &mut v["paper_fill"]["settlement"]["holdings"][0][1];
    *hold = serde_json::json!(hold.as_u64().unwrap() + 1);
    let h2 = h.with_file_name("held_tampered.json");
    std::fs::write(&h2, v.to_string()).unwrap();
    let mut e3 = engine(Some(PaperFillVersion::V2Shadow), |_| HOLD);
    e3.model_held_attach(&h2);
    let err = e3.model_held_restore().unwrap_err();
    assert!(
        format!("{err:?}").contains("SettlementBooksMismatch"),
        "{err:?}"
    );
    assert!(
        !e3.model_position_open(&MINT),
        "a refused restore applies nothing"
    );
    // A v2 engine restoring held exposure from a ledger with no paper_fill section refuses.
    v.as_object_mut().unwrap().remove("paper_fill");
    let h3 = h.with_file_name("held_nopf.json");
    std::fs::write(&h3, v.to_string()).unwrap();
    let mut e4 = engine(Some(PaperFillVersion::V2Shadow), |_| HOLD);
    e4.model_held_attach(&h3);
    let err = e4.model_held_restore().unwrap_err();
    assert!(
        format!("{err:?}").contains("PaperFillSectionMissing"),
        "{err:?}"
    );
}

#[test]
fn v2_price_only_reconciled_sell_cannot_be_settled_and_latches_settlement_fault() {
    let mut r = rig(
        Some(PaperFillVersion::V2Shadow),
        |s| if s == 0 { REDUCE } else { HOLD },
        None,
    );
    r.advance_to_order(120_000);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("REDUCE pending");
    // A reconciled report with a price but NO proceeds: the engine books it, settlement cannot (it would have to
    // invent proceeds) -> named fault, SAFETY_OFF, nothing closed.
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended, 45_000)
        .unwrap();
    assert!(
        r.e.model_settlement_faults()
            .iter()
            .any(|f| f.contains("price_only_report_has_no_proceeds")),
        "{:?}",
        r.e.model_settlement_faults()
    );
    assert!(r.e.model_safety_blocked());
    assert_eq!(r.e.model_safety_reason(), "settlement_fault");
    assert!(
        r.e.model_position_open(&MINT),
        "a REDUCE never closes; nothing force-closed"
    );
}

#[test]
fn leg_kind_codes_are_stable() {
    assert_eq!(LegKind::Entry.code(), "entry");
    assert_eq!(LegKind::Sell.code(), "sell");
}

/// SINGLE BOOKING PATH (runtime half; the source half is tests/books_single_path.rs). A v2 run through every
/// booking site the paper lane reaches (entry open, ADD commit, REDUCE release, EXIT realize + release) books
/// NOTHING outside a settlement scope, and the engine's books are the ledger's at every step.
#[test]
fn v2_every_booking_goes_through_the_ledger_add_reduce_exit_zero_bypasses() {
    let mut r = rig(
        Some(PaperFillVersion::V2Shadow),
        |s| match s {
            0 => ADD,
            1 => REDUCE,
            _ => EXIT,
        },
        None,
    );
    let check = |r: &Rig, when: &str| {
        assert_eq!(
            r.e.model_books_bypasses(),
            0,
            "{when}: a booking bypassed the ledger: {:?}",
            r.e.model_settlement_faults()
        );
        assert_eq!(r.e.model_books_source(), "settlement_ledger");
        let l = r.e.model_settlement().unwrap();
        let a = r.e.model_accounting_view(&MINT);
        assert_eq!(
            a.realized, l.realized,
            "{when}: engine realized IS the ledger's"
        );
        assert_eq!(
            i128::from(a.committed),
            l.committed,
            "{when}: engine committed IS the ledger's"
        );
        assert_eq!(
            i128::from(a.balance),
            l.seed + l.realized,
            "{when}: balance = seed + ledger realized"
        );
        r.assert_books_agree(when);
    };
    check(&r, "after entry");
    for (want, n) in [
        ("Add", "settle:applied:add"),
        ("Reduce", "settle:applied:sell"),
    ] {
        r.advance_to_order(120_000);
        let (_, kind, _, _) =
            r.e.model_mgmt_pending(&MINT)
                .unwrap_or_else(|| panic!("{want} pending: {:?}", r.e.model_lane_report()));
        assert_eq!(format!("{kind:?}"), want);
        r.landing();
        assert!(
            r.rep(n) >= 1,
            "{want} settled: {:?}",
            r.e.model_lane_report()
        );
        check(&r, want);
    }
    r.advance_to_order(200_000);
    r.landing();
    assert!(
        !r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    check(&r, "after EXIT");
    let l = r.e.model_settlement().unwrap();
    assert!(l.holdings.is_empty() && l.committed == 0 && l.cash == l.seed + l.realized);
    // The status block reports the books' source and that they equal the ledger.
    let st = r.e.model_paper_fill_status();
    assert_eq!(st["engine_books"]["source"], "settlement_ledger");
    assert_eq!(st["engine_books"]["realized"], st["settlement"]["realized"]);
    assert_eq!(
        st["engine_books"]["committed"],
        st["settlement"]["committed"]
    );
    assert_eq!(st["engine_books"]["bypasses"], 0);
}

/// The guard itself: a booking made OUTSIDE a settlement scope under v2 does not move the books, it latches the
/// named settlement fault `books_bypass:<site>` (SAFETY_OFF). Under v1 the same call writes the engine field.
#[test]
fn v2_an_unscoped_booking_is_a_named_bypass_fault_and_never_moves_the_books() {
    use pump_quant_app::engine::books::BookSite;
    let mut r = rig(Some(PaperFillVersion::V2Shadow), |_| HOLD, None);
    let before = r.e.model_accounting_view(&MINT);
    r.e.books_unscoped_commit_probe(BookSite::AddCommit, 12_345);
    assert_eq!(r.e.model_books_bypasses(), 1);
    assert!(
        r.e.model_settlement_faults()
            .iter()
            .any(|f| f == "books_bypass:add_commit"),
        "{:?}",
        r.e.model_settlement_faults()
    );
    assert!(r.e.model_safety_blocked());
    assert_eq!(r.e.model_safety_reason(), "settlement_fault");
    let after = r.e.model_accounting_view(&MINT);
    assert_eq!(after.committed, before.committed, "the books did not move");
    assert_eq!(after.realized, before.realized);
    // v1: the engine fields ARE the books; the same booking moves them and is not a fault.
    let mut v1 = rig(None, |_| HOLD, None);
    let b1 = v1.e.model_accounting_view(&MINT).committed;
    v1.e.books_unscoped_commit_probe(BookSite::AddCommit, 12_345);
    assert_eq!(v1.e.model_accounting_view(&MINT).committed, b1 + 12_345);
    assert_eq!(v1.e.model_books_bypasses(), 0);
    assert_eq!(v1.e.model_books_source(), "engine_fields_v1");
}

/// DOUBLE-BOOK PROOF. Under v2 the PREVIOUS booking path (the engine's own `bankroll_realized/committed`) must never
/// book a fill: through entry, ADD, REDUCE and EXIT the raw v1 fields stay exactly (0, 0) and every fill is booked
/// ONCE, in the ledger. If both paths ran, either the raw fields move (assert 1) or the ledger's books stop equalling
/// the independent sum of its own applied records (assert 2). Mutant m5_double_book (proc/wire_v4_mut_books.py)
/// makes the old path write too; this test must go red.
#[test]
fn v2_previous_booking_path_never_books_a_fill_no_double_booking() {
    let mut r = rig(
        Some(PaperFillVersion::V2Shadow),
        |s| match s {
            0 => ADD,
            1 => REDUCE,
            _ => EXIT,
        },
        None,
    );
    let once = |r: &Rig, when: &str| {
        assert_eq!(
            r.e.books_v1_fields_raw_probe(),
            (0, 0),
            "{when}: the previous (v1) booking path booked a fill under v2 -> double booking"
        );
        // Independent recomputation from the ledger's own durable fill records (each fill counted once).
        let l = r.e.model_settlement().unwrap();
        let j = l.to_json();
        let (mut bought, mut sold_net) = (0i128, 0i128);
        for a in j["applied"].as_array().unwrap() {
            let n = |i: usize| i128::from(a[i].as_u64().unwrap());
            if a[1] == "sell" {
                sold_net += n(4) - n(5) - n(6);
            } else {
                bought += n(4) + n(5) + n(6);
            }
        }
        assert_eq!(
            l.cash,
            l.seed - bought + sold_net,
            "{when}: cash == seed - Σbuys + Σsell nets (each fill booked once)"
        );
        let a = r.e.model_accounting_view(&MINT);
        assert_eq!(
            i128::from(a.balance) - i128::from(a.committed),
            l.cash,
            "{when}: engine free cash == ledger cash"
        );
    };
    once(&r, "after entry");
    for _ in 0..2 {
        r.advance_to_order(120_000);
        r.landing();
        once(&r, "after mgmt fill");
    }
    r.advance_to_order(200_000);
    r.landing();
    assert!(!r.e.model_position_open(&MINT));
    once(&r, "after EXIT");
}

/// Under v1 (unarmed) the previous path IS the book and nothing else books: there is no ledger to double into.
#[test]
fn v1_books_are_the_engine_fields_and_no_ledger_exists() {
    let r = rig(None, |_| HOLD, None);
    assert!(r.e.model_settlement().is_none());
    assert_eq!(r.e.model_books_source(), "engine_fields_v1");
    let (_, committed) = r.e.books_v1_fields_raw_probe();
    assert!(committed > 0, "v1 entry committed into the engine field");
    assert_eq!(
        u128::from(r.e.model_accounting_view(&MINT).committed),
        committed
    );
}

/// DIVERGENCE EVIDENCE IS DURABLE. A declared divergence leaves a bounded evidence record (mint, label, slot, clock,
/// the declaring observation, the base offsets, our dropped delta, the exposed inventory) that is persisted in the
/// held ledger, restored EXACTLY by a fresh engine, still present after the position is closed and its shadow
/// market forgotten, and the restored shadow stays diverged (a restart never clears a divergence).
#[test]
fn v2_divergence_evidence_survives_restart_and_close_and_is_never_cleared() {
    let h = tmp("v2div");
    let mut r = rig(
        Some(PaperFillVersion::V2Shadow),
        |s| if s == 0 { HOLD } else { EXIT },
        Some(&h),
    );
    let inv0 = r.e.model_inventory_tokens(&MINT).unwrap();
    r.clock += 500;
    r.slot += 1;
    let div_slot = r.slot;
    r.e.tick(curve_ev(r.clock, r.slot, r.rsol, 2_000_000_000));
    let log = r.e.model_divergence_log().to_vec();
    assert_eq!(log.len(), 1, "one record per declared divergence");
    let d = &log[0];
    assert_eq!(d.mint, MINT);
    assert_eq!(d.divergence, Divergence::CurveVirtualOffsetChanged);
    assert_eq!(d.slot, div_slot);
    assert_eq!(d.inventory_tokens, Some(inv0));
    let (vsol, _, rs, _) = state(r.rsol);
    assert_eq!(d.observed[0], vsol - 2_000_000_000);
    assert_eq!(d.observed[2], rs);
    assert_eq!(
        d.base_offsets.map(|o| o.0),
        Some(VOFF),
        "opened on the canonical offset"
    );
    assert!(
        d.dropped_delta[0] > 0,
        "our dropped SOL contribution is recorded"
    );
    // Persist + fresh engine: exact restore, still diverged.
    assert!(r.e.model_held_persist_now());
    let mut e2 = engine(Some(PaperFillVersion::V2Shadow), |_| HOLD);
    e2.model_held_attach(&h);
    e2.model_held_restore()
        .expect("restore ok")
        .expect("ledger");
    assert_eq!(e2.model_divergence_log(), &log[..]);
    assert_eq!(
        e2.model_shadow_book().divergence(&MINT),
        Some(Divergence::CurveVirtualOffsetChanged),
        "a restart never clears a divergence"
    );
    // Close the position (the model's EXIT, priced on the observed state alone): the market is forgotten, the
    // evidence stays, and it is still in the persisted ledger.
    r.advance_to_order(200_000);
    r.landing();
    assert!(
        !r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    assert!(r.e.model_shadow_book().market(&MINT).is_none());
    assert_eq!(r.e.model_divergence_log(), &log[..]);
    assert!(r.e.model_held_persist_now());
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&h).unwrap()).unwrap();
    assert_eq!(
        v["paper_fill"]["divergences"][0]["divergence"],
        "shadow_divergence:curve_virtual_offset_changed"
    );
    // Tampered evidence is refused by name, never silently dropped.
    let mut t = v.clone();
    t["paper_fill"]["divergences"][0]["divergence"] = serde_json::json!("cleared");
    let h2 = h.with_file_name("held_divtamper.json");
    std::fs::write(&h2, t.to_string()).unwrap();
    let mut e3 = engine(Some(PaperFillVersion::V2Shadow), |_| HOLD);
    e3.model_held_attach(&h2);
    let err = e3.model_held_restore().unwrap_err();
    assert!(format!("{err:?}").contains("PaperFillUntrusted"), "{err:?}");
}

/// RESTART PARITY of the closing sell: a held position restored from the ledger still owns its open token account, so
/// its closing sell credits the ATA deposit back (net of the close fee) exactly as an uninterrupted run does. Found by
/// daemon run wW5a3 (restart): before the fix the restored close booked NO refund (49,985,182 vs 52,019,462).
#[test]
fn v2_restart_then_close_books_the_same_ata_refund_as_an_uninterrupted_run() {
    let exit_all = |_: i64| EXIT;
    // Uninterrupted.
    let mut a = rig(Some(PaperFillVersion::V2Shadow), exit_all, None);
    a.advance_to_order(200_000);
    a.landing();
    assert!(!a.e.model_position_open(&MINT));
    assert_eq!(a.rep("settle:ata_rent_refund_in_closing_sell"), 1);
    // Same path, persisted + restored into a fresh engine before the EXIT is asked.
    let h = tmp("v2ata");
    let mut r = rig(Some(PaperFillVersion::V2Shadow), exit_all, Some(&h));
    assert!(r.e.model_held_persist_now());
    let mut b = Rig {
        e: engine(Some(PaperFillVersion::V2Shadow), exit_all),
        clock: r.clock,
        slot: r.slot,
        n: r.n,
        rsol: r.rsol,
    };
    b.e.model_held_attach(&h);
    b.e.model_held_restore().expect("restore").expect("ledger");
    // The restarted process re-observes the launch and the history (as the daemon's feeds do after a restart).
    b.e.tick(AppEvent::LaunchObserved {
        mint: mint(),
        creator: CREATOR,
        launch_unix_ms: T0,
    });
    for i in 0..40u32 {
        b.e.tick(trade(
            i,
            T0 + 1_000 + i64::from(i) * 2_000,
            1_000 + u64::from(i),
            22_000 + i128::from(i),
        ));
    }
    b.advance_to_order(200_000);
    b.landing();
    assert!(
        !b.e.model_position_open(&MINT),
        "{:?}",
        b.e.model_lane_report()
    );
    assert_eq!(
        b.rep("settle:ata_rent_refund_in_closing_sell"),
        1,
        "the restored close credits the ATA deposit back: {:?}",
        b.e.model_lane_report()
    );
    let (la, lb) = (
        a.e.model_settlement().unwrap(),
        b.e.model_settlement().unwrap(),
    );
    assert!(lb.invariant_holds() && lb.holdings.is_empty() && lb.committed == 0);
    // The SELL record carries the refund: venue-net of the restored close >= the deposit net of close fee.
    let sell = |l: &pump_quant_app::settlement::SettlementLedger| {
        l.to_json()["applied"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x[1] == "sell")
            .map(|x| x[4].as_u64().unwrap())
            .unwrap()
    };
    let refund = pump_quant_app::cost_model::ATA_RENT_LAMPORTS
        - pump_quant_app::cost_model::ATA_CLOSE_LAMPORTS;
    assert!(sell(lb) > refund, "restored close includes the refund");
    let _ = la;
}
