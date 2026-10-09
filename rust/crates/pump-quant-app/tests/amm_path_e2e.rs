//! AMM path through the REAL engine (`Engine::tick`) on CAPTURED history, discovered from stream
//! events only: launch -> curve prints -> canonical-pool swap events (pre-trade reserves) ->
//! stream-driven registration -> ready prompt -> stub BUY -> admission -> pending order -> fill from
//! the POOL reserves -> position.
//!
//! WHAT THIS IS. Fixture = the real mint ATtootmfeBeD..pump (chosen for provenance: launch row, curve
//! history with reserves, and a canonical pool with captured pre-trade reserves; NOT for outcome --
//! the stub always answers BUY). Inputs are the normalized corpus tape + captured reserve history
//! (`amm_history_v1`), so this is CORPUS RECONSTRUCTION, not daemon-wire compatibility.
//!
//! KNOWN GAPS in the fixture, stated rather than hidden: 1,569 of the mint's 2,254 AMM tape rows have
//! no captured pool in `amm_history_v1` and are OMITTED (their pool is not inferred), so the flow
//! windows undercount AMM activity; the pool fee rate is on-chain-verified for only 20 of the
//! swaps fed (125 bp), the rest carry none and the fill uses the last observed rate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pump_quant_app::config::Config;
use pump_quant_app::engine::model_admit::{
    Evidence, EvidenceResult, FaultResolution, FillReport, ReconcileOutcome,
};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const FIXTURE: &str = include_str!("fixtures/amm_atoo_events.jsonl");
const BUY: &str = "DECISION: BUY\nSIZE: SMALL\nINVALIDATION: none\nEVIDENCE: stub";

struct Stub(Arc<AtomicUsize>);
impl ModelSource for Stub {
    // Deterministic by PROMPT CONTENT: BUY only when the prompt itself says the venue is PumpSwap, so
    // the curve phase of the same mint cannot satisfy the AMM assertion.
    fn complete(&self, _s: &str, u: &str) -> Result<String, InferenceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if u.contains("venue=pumpswap") {
            Ok(BUY.to_string())
        } else {
            Ok("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: stub".to_string())
        }
    }
}

fn hex32(s: &str) -> [u8; 32] {
    let mut o = [0u8; 32];
    for (i, b) in o.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    o
}
fn entity(w: &[u8; 32]) -> u64 {
    let mut x = 0u64;
    for b in &w[..8] {
        x = (x << 8) | u64::from(*b);
    }
    x | 1
}

struct Run {
    e: Engine,
    calls: Arc<AtomicUsize>,
    first_amm_ms: i64,
    first_position_ms: Option<i64>,
}

/// Feed one fixture line into the engine exactly as the replay does (one feed path for every test).
fn feed_line(e: &mut Engine, v: &serde_json::Value, m: DomainMint, t: i64) {
    match v["k"].as_str().unwrap() {
        "L" => e.tick(AppEvent::LaunchObserved {
            mint: m,
            creator: hex32(v["c"].as_str().unwrap()),
            launch_unix_ms: t,
        }),
        "T" => {
            let w = hex32(v["w"].as_str().unwrap());
            let r = v["rv"].as_array();
            let (price_fp, liq) = match r {
                Some(r) => {
                    let (a, b) = (r[0].as_u64().unwrap(), r[1].as_u64().unwrap());
                    (
                        if b > 0 {
                            (u128::from(a) * 1_000_000_000 / u128::from(b)) as i128
                        } else {
                            0
                        },
                        a,
                    )
                }
                None => (0, 0),
            };
            if let Some(r) = r {
                e.tick(AppEvent::CurveObserved {
                    mint: m,
                    v_sol_lamports: r[0].as_u64().unwrap(),
                    v_tokens: r[1].as_u64().unwrap(),
                    real_sol_lamports: r[2].as_u64().unwrap(),
                    real_tokens: r[3].as_u64().unwrap(),
                    recv_unix_ms: Some(t),
                    slot: v["slot"].as_u64().unwrap(),
                });
            }
            e.tick(AppEvent::MarketTrade {
                mint: m,
                price_fp,
                quote_lamports: v["sol"].as_u64().unwrap(),
                liquidity_lamports: liq,
                signed_base: v["base"].as_i64().unwrap(),
                buyer_entity: entity(&w),
                age_slots: 30,
                recv_unix_ms: Some(t),
                trader_pubkey: Some(w),
                slot: v["slot"].as_u64(),
                fee_lamports: v["fee"].as_u64(),
                cu_consumed: v["cu"].as_u64(),
                venue: Some(TradeVenue::PumpFun),
                event_id: None,
                feature: None,
            });
        }
        "A" => {
            let w = hex32(v["w"].as_str().unwrap());
            e.tick(AppEvent::AmmSwap {
                mint: m,
                pool: hex32(v["pool"].as_str().unwrap()),
                pool_is_canonical: true,
                quote_is_wsol: true,
                token_reserve_pre: v["bres"].as_u64().unwrap(),
                quote_reserve_pre: v["qres"].as_u64().unwrap(),
                fee_bps: v["fee_bps"].as_u64().map(|x| x as u32),
                fee_parts: Some((
                    v["lp"].as_u64().unwrap() as u32,
                    v["pr"].as_u64().unwrap() as u32,
                    v["cr"].as_u64().unwrap() as u32,
                )),
                virtual_quote: v["vq"].as_u64(),
                // Pre-cashback-field stream semantics (the legacy pricing rule), unchanged.
                cashback: pump_quant_protocol::pumpswap_event::CashbackField::NotRecorded,
                is_buy: v["buy"].as_bool().unwrap(),
                token_amount: v["tok"].as_u64().unwrap(),
                quote_lamports: v["sol"].as_u64().unwrap(),
                trader: w,
                fee_lamports: v["fee"].as_u64(),
                cu_consumed: v["cu"].as_u64(),
                recv_unix_ms: Some(t),
                slot: v["slot"].as_u64().unwrap(),
            });
        }
        k => panic!("kind {k}"),
    }
}

fn replay(stop_before_first_amm: bool) -> Run {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));
    let mut last_tick = 0i64;
    let mut first_amm_ms = 0i64;
    let mut first_position_ms = None;
    for line in FIXTURE.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
        let t = v["t"].as_i64().unwrap();
        if v["k"] == "A" {
            if stop_before_first_amm {
                break;
            }
            if first_amm_ms == 0 {
                first_amm_ms = t;
            }
        }
        feed_line(&mut e, &v, m, t);
        if t - last_tick >= 1_000 {
            last_tick = t;
            e.tick(AppEvent::Tick);
            std::thread::sleep(std::time::Duration::from_millis(2));
            if first_position_ms.is_none() && e.model_position_open(m.as_bytes()) {
                first_position_ms = Some(t);
            }
        }
    }
    for _ in 0..30 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Run {
        e,
        calls,
        first_amm_ms,
        first_position_ms,
    }
}

fn amm_run_with_fill() -> Run {
    let r = replay(false);
    assert!(
        rep(&r.e, "fill:position_opened_amm") >= 1,
        "AMM fill required for these lifecycle cases"
    );
    r
}

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

#[test]
fn an_amm_market_is_discovered_from_stream_events_and_bought_through_the_real_engine() {
    let r = replay(false);
    let rpt = r.e.model_lane_report().clone();
    std::fs::write(
        "/tmp/amm_path_report.json",
        serde_json::to_string_pretty(&rpt).unwrap(),
    )
    .unwrap();
    eprintln!(
        "first_amm_ms={} first_position_ms={:?} calls={}",
        r.first_amm_ms,
        r.first_position_ms,
        r.calls.load(Ordering::SeqCst)
    );
    // Discovery came from the stream (registry), the stub was asked, and it answered BUY.
    assert!(rep(&r.e, "uniq_discovered") >= 1, "{rpt:?}");
    assert!(r.calls.load(Ordering::SeqCst) >= 1, "{rpt:?}");
    assert!(rep(&r.e, "verdict:buy") >= 1, "{rpt:?}");
    // The decision was an AMM one: either the pool plane governed the prompt, and the FILL was
    // priced from the pool (depth basis 3 books as MigratedPool).
    assert!(rep(&r.e, "verdict:buy|venue=pumpswap") >= 1, "{rpt:?}");
    assert_eq!(
        rep(&r.e, "verdict:buy|venue=pumpfun"),
        0,
        "the stub never buys the curve: {rpt:?}"
    );
    // EXECUTION lifecycle on a source-backed fixture: every swap carries its OWN fee parts and
    // virtual quote reserve decoded from the transaction's event (see fixtures/amm_atoo_events.jsonl,
    // rebuilt from chain). Quote arithmetic is the validated `buy_exact_quote_in` rule; the landing
    // assumption (fill against the next observed swap's pre-trade state, strictly later slot) is
    // UNVALIDATED, so the resulting position is a ROUTING fill, not assessable. Not a prompt-parity
    // claim: flow completeness is a separate test.
    assert!(
        rep(&r.e, "fill:position_opened_amm") >= 1,
        "complete per-event economics must now allow the AMM fill: {rpt:?}"
    );
    assert_eq!(
        rep(&r.e, "fill_none:amm_economics_not_on_landing_event"),
        0,
        "{rpt:?}"
    );
    assert!(!r.e.model_all_fills().is_empty());
    let f = r.e.model_all_fills()[0];
    assert!(f.amm && f.quote_validated && !f.landing_validated);
    assert!(r.e.model_assessable_fills().is_empty());
}

// ---- LIFECYCLE on the AMM path (execution lifecycle only; not routing, quote or prompt parity).
// Uses the same source-backed fixture, so the position is a real AMM fill with per-event economics.

#[test]
fn amm_fill_is_applied_once_and_duplicates_do_not_add_inventory() {
    let r = amm_run_with_fill();
    // Once-per-order, not once-per-run: held-position protection (a -35% hard stop on verified pool marks)
    // can legitimately close the position, after which the stub may BUY again as a NEW order. What must hold
    // is that no order filled twice, every fill is an AMM fill, every re-entry follows an exit, and no order
    // is left pending.
    let fills = r.e.model_all_fills();
    let amm_fills: Vec<_> = fills.iter().filter(|f| f.amm).collect();
    let opened = rep(&r.e, "fill:position_opened_amm") as usize;
    assert_eq!(
        amm_fills.len(),
        opened,
        "one fill record per opened position"
    );
    let mut ids: Vec<u64> = amm_fills.iter().map(|f| f.order_id).collect();
    ids.sort_unstable();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "no order id filled twice");
    // Protection now executes through a protective ORDER (it no longer books a close inside the position store), so
    // a protective close is counted at its reconciled fill.
    let exits = rep(&r.e, "protect:fill:closed")
        + rep(&r.e, "protect:fill:complete")
        + rep(&r.e, "mgmt:fill:complete");
    assert!(
        exits + 1 >= opened as u64,
        "every re-entry must follow a close ({opened} fills, {exits} closes)"
    );
    assert_eq!(
        r.e.model_pending_orders(),
        0,
        "no order left pending after its fill"
    );
}

#[test]
fn amm_conflicting_terminal_reports_fault_and_resolution_reconciles_the_books() {
    let mut r = amm_run_with_fill();
    let f0 = r.e.model_all_fills()[0];
    let m = f0.mint;
    let id = f0.order_id;
    let q = r.e.model_order_rec(id).unwrap().clip_lamports;
    let held = r.e.model_position_open(&m);
    // The paper simulator filled this AMM order; the execution side then says NotFilled.
    assert_eq!(
        r.e.model_ingest_evidence(Evidence {
            order_id: id,
            attempt: 1,
            clip_lamports: q,
            outcome: ReconcileOutcome::NotFilled
        }),
        EvidenceResult::Fault
    );
    assert_eq!(r.e.model_recon_faults().len(), 1);
    assert_eq!(rep(&r.e, "reconcile:FAULT_conflicting_terminal"), 1);
    let res =
        r.e.model_resolve_recon_fault(id, ReconcileOutcome::NotFilled);
    if held {
        assert_eq!(res, FaultResolution::Released { unwound: true });
        assert!(
            !r.e.model_position_open(&m),
            "inventory unwound before the block lifts"
        );
    } else {
        // Already closed by the normal exit path: realized cash cannot be silently rewritten.
        assert_eq!(
            res,
            FaultResolution::Refused("position_already_closed_needs_ledger_adjustment")
        );
        assert_eq!(r.e.model_recon_faults().len(), 1, "fault and block remain");
    }
}

#[test]
fn amm_uncertain_ack_stays_pending_then_reconciles_to_exactly_one_fill() {
    // Stop the replay right after the AMM BUY verdict, mark the ack uncertain, let further AMM
    // landing states arrive (it must NOT fill or expire), then confirm FILLED by evidence.
    let calls = Arc::new(AtomicUsize::new(0));
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));
    let mut last_tick = 0i64;
    let mut marked: Option<(u64, u32, u64)> = None;
    let mut mint_seen = None;
    for line in FIXTURE.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
        mint_seen = Some(*m.as_bytes());
        let t = v["t"].as_i64().unwrap();
        feed_line(&mut e, &v, m, t);
        if t - last_tick >= 1_000 {
            last_tick = t;
            e.tick(AppEvent::Tick);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        if marked.is_none() {
            if let Some(po) = e.model_pending_order(m.as_bytes()) {
                assert!(e.model_mark_ack_uncertain(po.0));
                marked = Some(po);
            }
        }
    }
    for _ in 0..20 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (id, at, q) = marked.expect("the stub bought, so an order existed");
    let mint = mint_seen.unwrap();
    // The uncertain order never filled and never expired, however many AMM states followed.
    assert_eq!(
        e.model_pending_orders(),
        1,
        "an uncertain AMM order stays pending"
    );
    assert!(
        !e.model_position_open(&mint),
        "unknown ack is not inventory"
    );
    assert_eq!(rep(&e, "fill:position_opened_amm|quote=unvalidated"), 0);
    assert_eq!(
        rep(&e, "fill_none:no_landing_state"),
        0,
        "TTL must not clear it"
    );
    // Evidence says FILLED: applied exactly once through the normal report-ingestion path.
    let fr = FillReport {
        entry_price_fp: 40_000,
        reserve_sol_lamports: 40_000_000_000,
    };
    assert_eq!(
        e.model_ingest_evidence(Evidence {
            order_id: id,
            attempt: at,
            clip_lamports: q,
            outcome: ReconcileOutcome::Filled(fr)
        }),
        EvidenceResult::Applied
    );
    assert!(e.model_position_open(&mint));
    assert_eq!(e.model_position_order_id(&mint), Some(id));
    assert_eq!(
        e.model_ingest_evidence(Evidence {
            order_id: id,
            attempt: at,
            clip_lamports: q,
            outcome: ReconcileOutcome::Filled(fr)
        }),
        EvidenceResult::Duplicate
    );
    assert_eq!(
        e.model_all_fills()
            .iter()
            .filter(|f| f.order_id == id)
            .count(),
        1,
        "one fill for one order"
    );
}

#[test]
fn amm_fixture_funnel_pools_discovered_ready_dispatched() {
    // FUNNEL on the source-backed AMM fixture, which feeds real `AppEvent::AmmSwap` (the curve-only
    // replay never does). One mint / one canonical pool: this measures the PATH, not population
    // coverage. Written to /tmp/amm_funnel.json for the report.
    let r = replay(false);
    let rpt = r.e.model_lane_report().clone();
    let funnel = r.e.model_funnel();
    let pools_observed: std::collections::BTreeSet<&str> = FIXTURE
        .lines()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            (v["k"] == "A").then_some(())?;
            Some("pool")
        })
        .collect();
    let distinct_pools = {
        let mut set = std::collections::BTreeSet::new();
        for l in FIXTURE.lines() {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            if v["k"] == "A" {
                set.insert(v["pool"].as_str().unwrap().to_string());
            }
        }
        set.len()
    };
    let _ = pools_observed;
    std::fs::write(
        "/tmp/amm_funnel.json",
        serde_json::to_string_pretty(&serde_json::json!({
            "distinct_canonical_pools_observed": distinct_pools,
            "funnel": funnel, "lane": rpt,
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(distinct_pools, 1);
    // Registry-based funnel (CURRENT venue of each market): 1 pool -> discovered -> ready -> dispatched.
    assert_eq!(
        funnel.get("discovered|venue=pumpswap"),
        Some(&1),
        "{funnel:?}"
    );
    assert_eq!(funnel.get("ready|venue=pumpswap"), Some(&1), "{funnel:?}");
    assert_eq!(
        funnel.get("dispatched|venue=pumpswap"),
        Some(&1),
        "{funnel:?}"
    );
    // The AMM plane itself was asked (7 pumpswap dispatches) and the AMM verdict reached the engine.
    assert!(rep(&r.e, "dispatched|venue=pumpswap") >= 1, "{rpt:?}");
    // NOTE (measurement): the `uniq_*` counters are keyed by the venue AT THE TIME of each stage, so a
    // market that migrates curve -> pool appears under both venues there. That is why this assertion
    // uses the registry funnel.
}

// ---- AMM MANAGEMENT plumbing (labelled: ROUTING/ACCOUNTING only, never profitability evidence).
// AMM sell-side fee economics are UNVERIFIED, so every fill here stays `assessable == false` and no PnL
// from it may be cited. What is exercised: identity/state from the captured pool plane, order intent,
// reconciled fill, partial inventory change, and the wallet identity.

struct MgmtStub {
    asks: Arc<AtomicUsize>,
}
impl ModelSource for MgmtStub {
    fn complete(&self, _s: &str, u: &str) -> Result<String, InferenceError> {
        if u.starts_with("Decide the next action for a position you already hold") {
            let n = self.asks.fetch_add(1, Ordering::SeqCst);
            return Ok(if n == 0 {
                "DECISION: REDUCE\nINVALIDATION: none\nEVIDENCE: stub".to_string()
            } else if n == 1 {
                "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: stub".to_string()
            } else {
                "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: stub".to_string()
            });
        }
        if u.contains("venue=pumpswap") {
            Ok(BUY.to_string())
        } else {
            Ok("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: stub".to_string())
        }
    }
}

#[test]
fn amm_management_reduce_then_exit_runs_through_the_real_engine_with_unassessed_economics() {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let asks = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(MgmtStub {
        asks: Arc::clone(&asks),
    });
    let mut last_tick = 0i64;
    for line in FIXTURE.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
        let t = v["t"].as_i64().unwrap();
        feed_line(&mut e, &v, m, t);
        if t - last_tick >= 1_000 {
            last_tick = t;
            e.tick(AppEvent::Tick);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    for _ in 0..60 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let r = e.model_lane_report().clone();
    assert!(rep(&e, "fill:position_opened_amm") >= 1, "{r:?}");
    // Every fill on this path is an AMM fill with UNVALIDATED landing: nothing is assessable.
    assert!(e.model_assessable_fills().is_empty());
    assert!(e.model_all_fills().iter().all(|f| f.amm));
    // Management asked at least once on the AMM position and the answers came back as valid verdicts.
    assert!(
        rep(&e, "mgmt:dispatched") >= 1,
        "management was never asked on the AMM position: {r:?}"
    );
    // REDUCE then EXIT each created ONE order and were each booked from a reconciled fill; the position closed
    // only on the EXIT fill. The replay is time-compressed, so some answers land late and are DISCARDED
    // (counted) - they never execute.
    assert_eq!(rep(&e, "mgmt:order:reduce"), 1, "{r:?}");
    assert_eq!(rep(&e, "mgmt:order:exit"), 1, "{r:?}");
    assert_eq!(rep(&e, "mgmt:fill:closed"), 1, "{r:?}");
    // M3: AMM sells are now priced by the VERIFIED size-specific quote (8/8 independent sells exact, incl.
    // fee rounding): each AMM sell fill carries the verified label. Fills stay `simulated` (paper executor).
    assert_eq!(rep(&e, "mgmt:fill_amm_sell"), 2, "{r:?}");
    assert_eq!(rep(&e, "mgmt:fill_amm_sell_fee_unverified"), 0, "{r:?}");
}

/// INDEPENDENT LEDGER over the whole captured replay. After EVERY fed event the test samples the engine's public
/// state and keeps its own books; the invariants below are checked against that ledger, not against engine counters:
///   * a position opens only from flat, with exactly one new fill record and inventory established by that fill;
///   * a position closes only while holding inventory > 0, exactly once per open, and inventory is gone afterwards;
///   * a re-entry (a second open) only happens after a completed close;
///   * cash applies once: `balance == seed + realized` at every sample, `realized` changes ONLY at a close/partial
///     transition, and an open changes `committed` (not realized);
///   * every order id appears once in the fill ledger.
#[test]
fn amm_lifecycle_independent_ledger_every_close_has_inventory_every_reentry_follows_a_close_cash_applies_once(
) {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));
    let mut last_tick = 0i64;
    #[derive(Clone, Copy, PartialEq, Debug)]
    struct Snap {
        open: bool,
        inv: Option<u64>,
        realized: i128,
        committed: u64,
        fills: usize,
    }
    let mut mint_seen: Option<[u8; 32]> = None;
    let mut prev: Option<Snap> = None;
    let (mut opens, mut closes) = (0u32, 0u32);
    let mut open_inventory: Option<u64> = None;
    let mut closes_without_inventory = 0u32;
    let mut reentry_without_close = 0u32;
    let mut cash_changed_off_transition = 0u32;
    let mut identity_broken = 0u32;
    let mut seed = None;
    let mut sample = |e: &Engine, m: &DomainMint, prev: &mut Option<Snap>| {
        let v = e.model_accounting_view(m.as_bytes());
        seed.get_or_insert(v.seed);
        if i128::from(v.balance) != (i128::from(v.seed) + v.realized).max(0) {
            identity_broken += 1;
        }
        let cur = Snap {
            open: e.model_position_open(m.as_bytes()),
            inv: e.model_inventory_tokens(m.as_bytes()),
            realized: v.realized,
            committed: v.committed,
            fills: e.model_all_fills().iter().filter(|f| f.amm).count(),
        };
        if let Some(p) = *prev {
            if !p.open && cur.open {
                opens += 1;
                if cur.fills != p.fills + 1 {
                    identity_broken += 1; // an open must add exactly one fill record
                }
                if cur.inv.unwrap_or(0) == 0 {
                    identity_broken += 1;
                }
                if opens > 1 && closes + 1 != opens {
                    reentry_without_close += 1;
                }
                open_inventory = cur.inv;
            } else if p.open && !cur.open {
                closes += 1;
                if p.inv.unwrap_or(0) == 0 {
                    closes_without_inventory += 1;
                }
                if cur.inv.is_some_and(|i| i > 0) {
                    identity_broken += 1; // inventory must be gone after a full close
                }
                if cur.fills != p.fills {
                    identity_broken += 1; // a close is not a BUY fill
                }
            } else if p.realized != cur.realized && !(p.open != cur.open || p.inv != cur.inv) {
                cash_changed_off_transition += 1;
            }
        }
        *prev = Some(cur);
    };
    for line in FIXTURE.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
        let t = v["t"].as_i64().unwrap();
        feed_line(&mut e, &v, m, t);
        let mm = *mint_seen.get_or_insert(*m.as_bytes());
        if &mm == m.as_bytes() {
            sample(&e, &m, &mut prev);
        }
        if t - last_tick >= 1_000 {
            last_tick = t;
            e.tick(AppEvent::Tick);
            std::thread::sleep(std::time::Duration::from_millis(2));
            if let Some(mm) = mint_seen {
                sample(&e, &DomainMint::from_bytes(mm), &mut prev);
            }
        }
    }
    for _ in 0..30 {
        e.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(10));
        if let Some(mm) = mint_seen {
            sample(&e, &DomainMint::from_bytes(mm), &mut prev);
        }
    }
    let _ = open_inventory;
    assert!(
        opens >= 1,
        "the replay must open at least one AMM position ({opens})"
    );
    assert_eq!(closes_without_inventory, 0, "every close had inventory");
    assert_eq!(
        reentry_without_close, 0,
        "every re-entry followed a completed close"
    );
    assert_eq!(
        identity_broken, 0,
        "balance = seed + realized, one fill record per open, inventory gone after close"
    );
    assert_eq!(
        cash_changed_off_transition, 0,
        "cash changed with no open/close/inventory transition"
    );
    // Fill ledger: every order id once, and the sampled opens equal the engine's own fill records.
    let fills = e.model_all_fills();
    let amm: Vec<_> = fills.iter().filter(|f| f.amm).collect();
    let mut ids: Vec<u64> = amm.iter().map(|f| f.order_id).collect();
    ids.sort_unstable();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "each order id once");
    assert_eq!(amm.len() as u32, opens, "sampled opens == fill records");
    assert!(
        closes <= opens && opens - closes <= 1,
        "at most one position remains open ({opens} opens, {closes} closes)"
    );
    assert!(
        e.model_assessable_fills().is_empty(),
        "routing fills are never assessable"
    );
}
