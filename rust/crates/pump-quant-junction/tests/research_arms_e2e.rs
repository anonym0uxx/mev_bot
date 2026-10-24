//! EXPERIMENT_SPEC_ABSORPTION — deterministic isolation scenario for research arms in ONE process:
//! the primary engine (arm A) and research arms share every feed event and one memoised model source,
//! but each book has its own cash, orders, positions, protection and durable ledger.
//!
//! Checks (all in one scenario, no network):
//!   1. A is capital-vetoed (cash below the survival floor) while B fills the same opportunity.
//!   2. One arm (B) exits on its own protection while another (B2, veto-blocked) never held — and A's
//!      ENTRY block does not disable B's protection.
//!   3. Restart: B's ledger is restored exactly once from `data/arms/B/MODEL_HELD.json`; B2/C stay clean;
//!      a duplicate delivery of the fill-bearing event adds no inventory.
//!   4. Conservation per book: balance = seed + realized; committed == remaining basis.
//!   5. Live order authority is impossible: `Arms::build` refuses in live mode.
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::{Completion, InferenceError};
use pump_quant_junction::research_arms::{parse_spec, ArmKind, ArmTemplate, Arms};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const FIXTURE: &str = include_str!("../../pump-quant-app/tests/fixtures/amm_atoo_events.jsonl");

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

/// Deterministic compact-contract Qwen stand-in shared by A and B: BUY SMALL on PumpSwap prompts.
struct Stub(Arc<AtomicUsize>);
impl ModelSource for Stub {
    fn complete(&self, s: &str, u: &str) -> Result<String, InferenceError> {
        self.complete_meta(s, u).map(|c| c.text)
    }
    fn complete_meta(&self, _s: &str, u: &str) -> Result<Completion, InferenceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let text = if u.contains("venue=pumpswap") { "DECISION: BUY\nSIZE: SMALL" } else { "DECISION: SKIP" };
        Ok(Completion {
            text: text.into(),
            finish_reason: Some("stop".into()),
            completion_tokens: Some(4),
            stop_reason: Some(pump_quant_inference::probe_sl::PROBE_SL_V1_COMPACT_STOP.into()),
        })
    }
}

fn event_of(v: &serde_json::Value) -> Vec<AppEvent> {
    let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
    let t = v["t"].as_i64().unwrap();
    match v["k"].as_str().unwrap() {
        "L" => vec![
            AppEvent::LaunchObserved { mint: m, creator: hex32(v["c"].as_str().unwrap()), launch_unix_ms: t },
            AppEvent::CurveQuote { mint: m, quote_mint: [0u8; 32] },
        ],
        "T" => {
            let w = hex32(v["w"].as_str().unwrap());
            let r = v["rv"].as_array();
            let (price_fp, liq) = match r {
                Some(r) => {
                    let (a, b) = (r[0].as_u64().unwrap(), r[1].as_u64().unwrap());
                    (if b > 0 { (u128::from(a) * 1_000_000_000 / u128::from(b)) as i128 } else { 0 }, a)
                }
                None => (0, 0),
            };
            let mut out = Vec::new();
            if let Some(r) = r {
                out.push(AppEvent::CurveObserved {
                    mint: m,
                    v_sol_lamports: r[0].as_u64().unwrap(),
                    v_tokens: r[1].as_u64().unwrap(),
                    real_sol_lamports: r[2].as_u64().unwrap(),
                    real_tokens: r[3].as_u64().unwrap(),
                    recv_unix_ms: Some(t),
                    slot: v["slot"].as_u64().unwrap(),
                });
            }
            out.push(AppEvent::MarketTrade {
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
            out
        }
        "A" => vec![amm_swap(v, None)],
        k => panic!("kind {k}"),
    }
}

fn amm_swap(v: &serde_json::Value, price_scale_bps: Option<u64>) -> AppEvent {
    let m = DomainMint::from_bytes(hex32(v["m"].as_str().unwrap()));
    let t = v["t"].as_i64().unwrap();
    let w = hex32(v["w"].as_str().unwrap());
    let mut qres = v["qres"].as_u64().unwrap();
    if let Some(s) = price_scale_bps {
        qres = (u128::from(qres) * u128::from(s) / 10_000) as u64;
    }
    AppEvent::AmmSwap {
        mint: m,
        pool: hex32(v["pool"].as_str().unwrap()),
        pool_is_canonical: true,
        quote_is_wsol: true,
        token_reserve_pre: v["bres"].as_u64().unwrap(),
        quote_reserve_pre: qres,
        fee_bps: v["fee_bps"].as_u64().map(|x| x as u32),
        fee_parts: Some((v["lp"].as_u64().unwrap() as u32, v["pr"].as_u64().unwrap() as u32, v["cr"].as_u64().unwrap() as u32)),
        virtual_quote: v["vq"].as_i64().map(i128::from),
        is_buy: v["buy"].as_bool().unwrap(),
        token_amount: v["tok"].as_u64().unwrap(),
        quote_lamports: v["sol"].as_u64().unwrap(),
        trader: w,
        fee_lamports: v["fee"].as_u64(),
        cu_consumed: v["cu"].as_u64(),
        recv_unix_ms: Some(t),
        slot: v["slot"].as_u64().unwrap(),
    }
}

fn build(root: &std::path::Path, src: Arc<dyn ModelSource + Send + Sync>) -> (Engine, Arms) {
    let mut cfg = Config::dev_portable();
    // Arm A: cash BELOW the survival floor (max(0.5 SOL, 25 % seed)) → every BUY is a capital veto.
    cfg.bankroll_initial_lamports = 400_000_000;
    let mut a = Engine::new(cfg, RunMode::Paper);
    a.set_entry_contract(pump_quant_app::engine::EntryContract::ProbeSlV1Compact);
    a.enable_paper_model_arc(src.clone());
    let _ = pump_quant_junction::model_lifecycle::restore_held_state(&mut a, &root.join("A_MODEL_HELD.json"));
    let _ = a.model_safety_attach(&root.join("A_MODEL_SAFETY.json"));
    let mut tpl_cfg = Config::dev_portable();
    tpl_cfg.bankroll_initial_lamports = 2_000_000_000;
    let tpl = ArmTemplate {
        cfg: tpl_cfg,
        entry_contract: Some(pump_quant_app::engine::EntryContract::ProbeSlV1Compact),
        management_policy: None,
        loss_budget_lamports: Some(300_000_000),
        cancel_lag_ms: None,
    };
    let specs = parse_spec("B:veto:18446744073709551615;B2:veto:0;C:rule:0").unwrap();
    let arms = Arms::build(&specs, &tpl, Some(src), root, false).unwrap();
    (a, arms)
}

/// Drive the fixture until arm `hold_arm` holds the mint (then a few settling ticks), so the
/// isolation checks run against an OPEN position. Returns the last AMM state delivered.
fn drive(a: &mut Engine, arms: &mut Arms, lines: &[serde_json::Value], mint: &[u8; 32], hold_arm: usize) -> Option<serde_json::Value> {
    let mut last_tick = 0i64;
    let mut last_amm: Option<serde_json::Value> = None;
    for v in lines {
        if v["k"] == "A" {
            last_amm = Some(v.clone());
        }
        for ev in event_of(v) {
            arms.tick(&ev);
            a.tick(ev);
        }
        let t = v["t"].as_i64().unwrap();
        if t - last_tick >= 1_000 {
            last_tick = t;
            arms.tick(&AppEvent::Tick);
            a.tick(AppEvent::Tick);
            std::thread::sleep(std::time::Duration::from_millis(2));
            if arms.arms[hold_arm].engine.model_position_open(mint) {
                for _ in 0..5 {
                    arms.tick(&AppEvent::Tick);
                    a.tick(AppEvent::Tick);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                return last_amm;
            }
        }
    }
    last_amm
}

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report().iter().filter(|(k, _)| k.starts_with(prefix)).map(|(_, v)| *v).sum()
}

fn conservation(e: &Engine, seed: u64, mint: &[u8; 32]) {
    let view = e.model_accounting_view(mint);
    let realized = e.bankroll_realized_lamports();
    let balance = i128::from(e.bankroll_balance());
    assert_eq!(balance, i128::from(seed) + realized, "balance = seed + realized");
    let committed = i128::try_from(e.bankroll_committed_lamports()).unwrap();
    assert_eq!(committed, i128::from(view.remaining_cost_basis.unwrap_or(0)), "committed == remaining basis");
}

#[test]
fn arms_are_isolated_books_sharing_one_feed_and_one_model_lane() {
    let root = std::env::temp_dir().join(format!("pq-arms-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let memo = Arc::new(pump_quant_app::absorption::MemoSource::new(Stub(calls.clone()), 60_000));
    let src: Arc<dyn ModelSource + Send + Sync> = memo.clone();
    let lines: Vec<serde_json::Value> = FIXTURE.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mint = hex32(lines[0]["m"].as_str().unwrap());

    // 5. live authority refused by construction
    let tpl = ArmTemplate { cfg: Config::dev_portable(), entry_contract: None, management_policy: None, loss_budget_lamports: None, cancel_lag_ms: None };
    assert!(Arms::build(&parse_spec("B:veto:1").unwrap(), &tpl, Some(src.clone()), &root, true).is_err());

    let (mut a, mut arms) = build(&root, src.clone());
    let first_amm = drive(&mut a, &mut arms, &lines, &mint, 0).expect("fixture has AMM events");

    // 1. A capital-vetoed, B filled, B2 veto-refused by name, C (rule, R*=0) never bought.
    assert_eq!(rep(&a, "fill:position_opened_amm"), 0, "A: {:?}", a.model_lane_report());
    assert!(rep(&a, "notrade:breaches_floor") + rep(&a, "notrade:unpayable_clip") >= 1, "A: {:?}", a.model_lane_report());
    let b = &arms.arms[0];
    let b2 = &arms.arms[1];
    let c = &arms.arms[2];
    assert_eq!(b.kind, ArmKind::Veto { r_star_bps: u64::MAX });
    assert!(rep(&b.engine, "fill:position_opened_amm") >= 1, "B: {:?}", b.engine.model_lane_report());
    assert!(b.engine.model_position_open(&mint));
    assert!(rep(&b2.engine, "notrade:absorption|") >= 1, "B2: {:?}", b2.engine.model_lane_report());
    assert!(!b2.engine.model_position_open(&mint));
    assert_eq!(rep(&c.engine, "fill:position_opened_amm"), 0);
    assert_eq!(rep(&c.engine, "probe_sl:invalid"), 0);
    // shared inference: A, B and B2 saw identical prompt bytes → the stub was called far fewer
    // times than the three engines dispatched (C never calls Qwen).
    let dispatched: u64 = [&a, &b.engine, &b2.engine].iter().map(|e| rep(e, "probe_sl:valid:")).sum();
    let stub_calls = calls.load(Ordering::SeqCst) as u64;
    assert!(stub_calls < dispatched, "memo shared {} of {} dispatches", dispatched - stub_calls, dispatched);
    assert!(memo.hits.load(Ordering::Relaxed) > 0);

    // 4. conservation per book (B holds, A/B2/C flat)
    conservation(&b.engine, 2_000_000_000, &mint);
    conservation(&a, 400_000_000, &mint);
    conservation(&b2.engine, 2_000_000_000, &mint);
    conservation(&c.engine, 2_000_000_000, &mint);
    let b_basis = b.engine.model_accounting_view(&mint).remaining_cost_basis.unwrap_or(0);
    assert!(b_basis > 0);

    // 3a. duplicate delivery of the fill-bearing state adds no inventory anywhere
    let dup = amm_swap(&first_amm, None);
    arms.tick(&dup);
    a.tick(dup);
    assert_eq!(arms.arms[0].engine.model_accounting_view(&mint).remaining_cost_basis.unwrap_or(0), b_basis);

    // 3b. restart: persist, rebuild from the same dirs → B restored exactly once, others clean
    arms.persist_held();
    let (_a2, mut arms2) = build(&root, src.clone());
    assert!(arms2.arms[0].engine.model_position_open(&mint), "B restored");
    assert_eq!(arms2.arms[0].engine.model_accounting_view(&mint).remaining_cost_basis.unwrap_or(0), b_basis, "restored once, same basis");
    assert!(!arms2.arms[1].engine.model_position_open(&mint));
    assert!(!arms2.arms[2].engine.model_position_open(&mint));
    arms2.persist_held();
    let (_a3, arms3) = build(&root, src.clone());
    assert_eq!(arms3.arms[0].engine.model_accounting_view(&mint).remaining_cost_basis.unwrap_or(0), b_basis, "persist/restore is idempotent");

    // 2. A's ENTRY block does not disable B's protection: block A, then drop the pool to 0.5× — B
    //    opens its protective exit (standing intent) and lands it on the next state; B2 unaffected.
    let (mut a4, mut arms4) = (_a3, arms3);
    a4.set_model_entries_blocked(true);
    let t0 = first_amm["t"].as_i64().unwrap();
    let mut late = first_amm.clone();
    late["t"] = serde_json::json!(t0 + 120_000);
    late["slot"] = serde_json::json!(first_amm["slot"].as_u64().unwrap() + 300);
    let drop = amm_swap(&late, Some(5_000));
    arms4.tick(&drop);
    a4.tick(drop);
    let mut late2 = late.clone();
    late2["t"] = serde_json::json!(t0 + 121_000);
    late2["slot"] = serde_json::json!(first_amm["slot"].as_u64().unwrap() + 302);
    let next = amm_swap(&late2, Some(5_000));
    arms4.tick(&next);
    a4.tick(next);
    for _ in 0..10 {
        arms4.tick(&AppEvent::Tick);
        a4.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let b4 = &arms4.arms[0].engine;
    assert!(rep(b4, "protection:amm_hard_stop") >= 1 || rep(b4, "exit") >= 1, "B protection acted: {:?}", b4.model_lane_report());
    assert_eq!(rep(&a4, "protection:amm_hard_stop"), 0, "A held nothing");
    assert_eq!(rep(&arms4.arms[1].engine, "protection:amm_hard_stop"), 0, "B2 held nothing");
    conservation(b4, 2_000_000_000, &mint);
    let _ = std::fs::remove_dir_all(&root);
}

/// Protective handoff and restart with exposure in a research arm: the stop request covers the UNION
/// (primary flat, B holding → not flat; request JSON names arm B's exposure; the ack binds to the
/// combined digest), and a restart after B's protection fired restores B's latch, basis and
/// reservation exactly once, so the standing exit continues on the next landing state.
#[test]
fn handoff_and_restart_cover_exposure_held_in_an_arm() {
    use pump_quant_junction::model_lifecycle::{handle_stop_request_with_arms, StopGate, StopSession};
    let root = std::env::temp_dir().join(format!("pq-arms-h-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let src: Arc<dyn ModelSource + Send + Sync> = Arc::new(pump_quant_app::absorption::MemoSource::new(Stub(calls), 60_000));
    let lines: Vec<serde_json::Value> = FIXTURE.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mint = hex32(lines[0]["m"].as_str().unwrap());

    // ── Part 1: protection latch + reservation survive a restart; the standing exit continues.
    let (mut a, mut arms) = build(&root, src.clone());
    let last_amm = drive(&mut a, &mut arms, &lines, &mint, 0).unwrap();
    assert!(arms.arms[0].engine.model_position_open(&mint));
    let basis = arms.arms[0].engine.model_accounting_view(&mint).remaining_cost_basis.unwrap_or(0);
    let t0 = last_amm["t"].as_i64().unwrap();
    let mut late = last_amm.clone();
    late["t"] = serde_json::json!(t0 + 120_000);
    late["slot"] = serde_json::json!(last_amm["slot"].as_u64().unwrap() + 300);
    let drop = amm_swap(&late, Some(5_000));
    arms.tick(&drop);
    a.tick(drop);
    arms.tick(&AppEvent::Tick);
    a.tick(AppEvent::Tick);
    assert!(rep(&arms.arms[0].engine, "protection:amm_hard_stop") >= 1, "{:?}", arms.arms[0].engine.model_lane_report());
    arms.persist_held();
    let held_doc = std::fs::read_to_string(root.join("arms/B/MODEL_HELD.json")).unwrap();
    assert!(held_doc.contains("amm_hard_stop"), "latch persisted in B's ledger: {held_doc}");
    drop_all(a, arms);
    let (mut a2, mut arms2) = build(&root, src.clone());
    {
        let b2 = &arms2.arms[0].engine;
        assert!(b2.model_position_open(&mint));
        assert_eq!(b2.model_accounting_view(&mint).remaining_cost_basis.unwrap_or(0), basis);
        assert_eq!(i128::try_from(b2.bankroll_committed_lamports()).unwrap(), i128::from(basis), "reservation restored");
        assert!(!arms2.arms[1].engine.model_position_open(&mint));
        // The protective exit order that was pending at persist time is restored UNCERTAIN (never
        // assumed executed or dropped) — the same rule the daemon applies; the operator reconciles it.
        assert_eq!(b2.model_lane_report().get("held_state:restored_pending_uncertain").copied(), Some(1));
    }
    let held_json: serde_json::Value = serde_json::from_str(&held_doc).unwrap();
    assert_eq!(held_json["pending"][0]["kind"].as_str(), Some("exit"), "the pending order is B's protective exit");
    let pending_oid = held_json["pending"][0]["id"].as_u64().expect("pending exit order persisted with its id");
    assert!(arms2.arms[0].engine.model_mgmt_resolve_uncertain_not_executed(&mint, pending_oid), "operator reconciliation: not executed");
    let mut late2 = late.clone();
    late2["t"] = serde_json::json!(t0 + 125_000);
    late2["slot"] = serde_json::json!(last_amm["slot"].as_u64().unwrap() + 310);
    let next = amm_swap(&late2, Some(5_000));
    arms2.tick(&next);
    a2.tick(next);
    for _ in 0..12 {
        arms2.tick(&AppEvent::Tick);
        a2.tick(AppEvent::Tick);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let b2 = &arms2.arms[0].engine;
    assert!(rep(b2, "mgmt:protect_exit") >= 1, "standing protective exit continued after restart: {:?}", b2.model_lane_report());
    conservation(b2, 2_000_000_000, &mint);

    // ── Part 2: protective handoff covers exposure held in an arm (fresh books, B holding).
    let root2 = std::env::temp_dir().join(format!("pq-arms-h2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root2);
    std::fs::create_dir_all(&root2).unwrap();
    let (mut a3, mut arms3) = build(&root2, src.clone());
    drive(&mut a3, &mut arms3, &lines, &mint, 0).unwrap();
    assert!(arms3.arms[0].engine.model_position_open(&mint));
    let req = root2.join("REQ.json");
    let ack = root2.join("ACK.json");
    let mut st = StopSession::new();
    let gate = {
        let mut refs: Vec<(&str, &mut Engine)> = arms3.arms.iter_mut().map(|x| (x.id.as_str(), &mut x.engine)).collect();
        handle_stop_request_with_arms(&mut a3, &mut refs, &mut st, &req, &ack)
    };
    let (request_id, digest) = match gate {
        StopGate::Incomplete { assessment, request_id, exposure_digest, .. } => {
            assert_eq!(assessment.held, 1, "union assessment counts B's position");
            (request_id, exposure_digest)
        }
        other => panic!("expected incomplete handoff, got {other:?}"),
    };
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&req).unwrap()).unwrap();
    assert_eq!(doc["exposure_snapshot"]["arms"]["B"]["held"].as_u64(), Some(1), "{doc}");
    assert!(digest.contains("|B:"), "combined digest names the arm: {digest}");
    std::fs::write(
        &ack,
        serde_json::json!({"session_id": st.session_id, "request_id": request_id, "exposure_digest": digest, "recipient": "test-operator", "accepted_protective_responsibility": true}).to_string(),
    )
    .unwrap();
    let gate2 = {
        let mut refs: Vec<(&str, &mut Engine)> = arms3.arms.iter_mut().map(|x| (x.id.as_str(), &mut x.engine)).collect();
        handle_stop_request_with_arms(&mut a3, &mut refs, &mut st, &req, &ack)
    };
    assert!(matches!(gate2, StopGate::CompleteHandedOff { .. }), "{gate2:?}");
    // Flat books complete flat without any ack.
    let root3 = std::env::temp_dir().join(format!("pq-arms-h3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root3);
    std::fs::create_dir_all(&root3).unwrap();
    let (mut a4, mut arms4) = build(&root3, src);
    let mut st2 = StopSession::new();
    let gate3 = {
        let mut refs: Vec<(&str, &mut Engine)> = arms4.arms.iter_mut().map(|x| (x.id.as_str(), &mut x.engine)).collect();
        handle_stop_request_with_arms(&mut a4, &mut refs, &mut st2, &root3.join("REQ.json"), &root3.join("ACK.json"))
    };
    assert!(matches!(gate3, StopGate::CompleteFlat), "{gate3:?}");
    for r in [&root, &root2, &root3] {
        let _ = std::fs::remove_dir_all(r);
    }
}

fn drop_all(a: Engine, arms: Arms) {
    drop(a);
    drop(arms);
}

