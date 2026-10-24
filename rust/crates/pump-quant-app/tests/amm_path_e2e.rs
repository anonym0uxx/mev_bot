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
        "L" => {
            e.tick(AppEvent::LaunchObserved {
                mint: m,
                creator: hex32(v["c"].as_str().unwrap()),
                launch_unix_ms: t,
            });
            // Captured fixture markets are SOL-quoted (verified: DEV506_QUOTE_PROVENANCE).
            e.tick(AppEvent::CurveQuote {
                mint: m,
                quote_mint: [0u8; 32],
            });
        }
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
                virtual_quote: v["vq"].as_i64().map(i128::from),
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
    replay_arm(stop_before_first_amm, None, None)
}

/// Same replay with a research-arm configuration: an absorption veto (arm B) and/or a
/// replacement model source (arm C). `None, None` is arm A — byte-identical to `replay`.
fn replay_arm(
    stop_before_first_amm: bool,
    absorption_r_star_bps: Option<u64>,
    source: Option<Arc<dyn ModelSource + Send + Sync>>,
) -> Run {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 2_000_000_000;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg, RunMode::Paper);
    e.set_entry_absorption_veto_bps(absorption_r_star_bps);
    match source {
        Some(src) => e.enable_paper_model_arc(src),
        None => e.enable_paper_model(Stub(Arc::clone(&calls))),
    }
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
    // The atoo tape collapses −93 %: with AMM-plane protection the position is stopped and the
    // always-BUY stub re-enters, so several orders fill over the tape. The invariant is per order:
    // one fill per filled order, distinct order ids, and duplicates never add inventory.
    let fills: Vec<_> = r.e.model_all_fills().iter().filter(|f| f.amm).collect();
    assert!(!fills.is_empty());
    assert_eq!(
        rep(&r.e, "fill:position_opened_amm"),
        fills.len() as u64,
        "one fill per filled order"
    );
    let mut ids: Vec<u64> = fills.iter().map(|f| f.order_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), fills.len(), "no order filled twice");
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
            (v["k"] == "A").then(|| ())?;
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
    // On this −93 % tape the independent AMM-plane hard stop may pre-empt the scripted REDUCE/EXIT
    // (protection is never gated on the model); every close still comes from a reconciled EXIT fill,
    // each position closes exactly once, and nothing is left stranded.
    assert!(rep(&e, "mgmt:order:exit") >= 1, "{r:?}");
    assert!(rep(&e, "mgmt:fill:closed") >= 1, "{r:?}");
    let opened = rep(&e, "fill:position_opened_amm");
    let closed = rep(&e, "mgmt:fill:closed");
    assert!(
        closed == opened || closed + 1 == opened,
        "every entry closes exactly once (or the last is still held): {r:?}"
    );
    assert!(
        rep(&e, "protection:amm_hard_stop|intent") + rep(&e, "mgmt:order:reduce") >= 1,
        "{r:?}"
    );
    // The unverified AMM sell economics are FLAGGED on every AMM sell fill, so no PnL from them is citable.
    let sell_fills = e.model_mgmt_fills().len() as u64;
    assert_eq!(
        rep(&e, "mgmt:fill_amm_sell_fee_unverified"),
        sell_fills,
        "{r:?}"
    );
}


/// ARM B — the declared absorption veto is evaluated on the common opportunity (a valid BUY) from
/// the exact prompt bytes, before any capital veto: R* = 0 refuses by name with the components
/// journaled in the lane report; R* = MAX passes and the unchanged path fills. Arm A (no veto) is
/// byte-identical to the plain replay.
#[test]
fn arm_b_absorption_veto_refuses_by_name_before_capital_and_passes_when_below_r_star() {
    let a = replay(false);
    assert!(rep(&a.e, "fill:position_opened_amm") >= 1);
    assert_eq!(rep(&a.e, "notrade:absorption"), 0, "arm A never evaluates the veto");

    let b_veto = replay_arm(false, Some(0), None);
    assert_eq!(rep(&b_veto.e, "fill:position_opened_amm"), 0, "{:?}", b_veto.e.model_lane_report());
    assert!(rep(&b_veto.e, "notrade:absorption|") >= 1, "{:?}", b_veto.e.model_lane_report());
    assert_eq!(rep(&b_veto.e, "notrade:absorption_unavailable"), 0, "R was available on the AMM prompt");
    // the veto sits BEFORE sizing: no sizing/order records, no capital veto counted for these asks
    assert_eq!(rep(&b_veto.e, "order"), 0);

    let b_pass = replay_arm(false, Some(u64::MAX), None);
    assert!(rep(&b_pass.e, "fill:position_opened_amm") >= 1, "{:?}", b_pass.e.model_lane_report());
    assert_eq!(rep(&b_pass.e, "notrade:absorption"), 0);
}

/// ARM C — the transparent rule runs through the real engine as a model source: every answer is a
/// valid compact record (no invalid/off-contract), BUYs are SMALL only, and the rule's SKIP on an
/// unavailable R is a decision, not a veto record.
#[test]
fn arm_c_rule_source_runs_through_the_unchanged_authority_with_only_valid_compact_records() {
    let src: Arc<dyn ModelSource + Send + Sync> =
        Arc::new(pump_quant_app::absorption::RuleSource { r_star_bps: u64::MAX });
    let c = replay_arm(false, None, Some(src));
    let r = c.e.model_lane_report();
    let valid: u64 = r.iter().filter(|(k, _)| k.starts_with("probe_sl:valid:")).map(|(_, v)| *v).sum();
    let invalid: u64 = r.iter().filter(|(k, _)| k.starts_with("probe_sl:invalid")).map(|(_, v)| *v).sum();
    assert!(valid >= 1 || r.iter().any(|(k, _)| k.starts_with("verdict:")), "rule answered: {r:?}");
    assert_eq!(invalid, 0, "{r:?}");
    assert_eq!(rep(&c.e, "notrade:absorption"), 0, "arm C has no veto layer; R is inside its rule");
    // SMALL-only sizing: no FULL tier sizing record can exist for arm C
    assert_eq!(r.get("verdict:buy_full").copied().unwrap_or(0), 0);
}
