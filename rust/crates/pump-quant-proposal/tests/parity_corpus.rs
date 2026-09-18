//! Byte-parity harness: the live proposal renderer vs. the c11 corpus.
//!
//! Each fixture is one corpus row reduced to typed inputs plus the exact user prompt the
//! corpus stores. The test rebuilds the prompt from the inputs and asserts the two are
//! byte-identical. That is the whole contract: a divergence is out-of-distribution for
//! the model, so it is a failure, not a warning.
//!
//! The size-options / cost-line numbers are reproduced by the ported cost authority from
//! the pool depth the corpus measured. For the entry family the depth is not printed in
//! the row (only a 1-dp rendering of it is), so the fixture carries a depth recovered
//! from the printed block; the arithmetic itself is pinned separately by the golden
//! tests in `cost.rs`. For the management family the corpus records the exact depth in
//! the row's meta and that value is used directly.
//!
//! Fixtures are LF-friendly JSON with CRLF line endings (repo convention); each line is
//! one self-contained JSON object, so a trailing `\r` is trimmed before parsing.

use pump_quant_proposal::decision::{AmmState, CurveState, DecisionBundle, EnrichedCandidate};
use pump_quant_proposal::management::{
    DevHistoryManagement, EnrichedManagement, ManagementBundle,
};
use pump_quant_proposal::{render_decision, render_management, FlowState, PyNum};
use serde_json::Value;

fn pynum(v: &Value) -> PyNum {
    if let Some(b) = v.as_bool() {
        PyNum::Bool(b)
    } else if v.is_f64() {
        PyNum::Float(v.as_f64().expect("f64"))
    } else {
        PyNum::Int(v.as_i64().unwrap_or_else(|| {
            panic!("fixture number is neither int, float nor bool: {v}")
        }))
    }
}

fn pynum_opt(v: &Value) -> Option<PyNum> {
    if v.is_null() {
        None
    } else {
        Some(pynum(v))
    }
}

fn i64k(v: &Value, k: &str) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("expected integer at {k}: got {v}"))
}

#[allow(dead_code)]
fn i64_of(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("expected integer at {}: got {v}", std::backtrace::Backtrace::capture().to_string().lines().count()))
}

/// A flow field that the `no_prior_flow` variant does not carry.
fn i64_or0(v: &Value) -> i64 {
    if v.is_null() { 0 } else { i64_of(v) }
}

fn f64_of(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("expected number, got {v}"))
}

fn s_of(v: &Value) -> String {
    v.as_str().unwrap_or_else(|| panic!("expected string, got {v}")).to_string()
}

fn opt_i64(v: &Value) -> Option<i64> {
    if v.is_null() {
        None
    } else {
        v.as_i64()
    }
}

fn u64_or0(v: &Value) -> u64 {
    if v.is_null() { 0 } else { v.as_u64().unwrap_or_else(|| panic!("expected uint, got {v}")) }
}

fn flow_state(v: &Value) -> FlowState {
    FlowState {
        entrants_60s: u64_or0(&v["entrants_60s"]),
        entrants_300s: u64_or0(&v["entrants_300s"]),
        net_flow_sol_300s: f64_of(&v["net_flow_sol_300s"]),
        fresh_wallet_share_300s: v["fresh_wallet_share_300s"].as_f64(),
        flow_lookback_d: f64_of(&v["flow_lookback_d"]),
        sniper_share_300s: v["sniper_share_300s"].as_f64(),
        bot_uniform_share_300s: v["bot_uniform_share_300s"].as_f64(),
        smart_entrants_300s: u64_or0(&v["smart_entrants_300s"]),
        smart_net_flow_sol_300s: f64_of(&v["smart_net_flow_sol_300s"]),
        coentry_wallets_300s: u64_or0(&v["coentry_wallets_300s"]),
        creator_trading_own_mint: v["creator_trading_own_mint"]
            .as_bool()
            .expect("creator_trading_own_mint"),
        entrant_fee_p90_lamports: v["entrant_fee_p90_lamports"].as_u64(),
        entrant_cu_p50: v["entrant_cu_p50"].as_u64(),
    }
}

fn curve_state(v: &Value) -> CurveState {
    match v["kind"].as_str().expect("curve.kind") {
        "absent" => CurveState::Absent { reason: s_of(&v["reason"]) },
        "present" => CurveState::Present {
            staleness_ms: i64k(&v["staleness_ms"], "staleness_ms"),
            pricing_eligible: v["pricing_eligible"].as_bool().expect("pricing_eligible"),
            v_sol_reserves_lamports: i64k(&v["v_sol_reserves_lamports"], "v_sol_reserves_lamports"),
            v_tokens_reserves: i64k(&v["v_tokens_reserves"], "v_tokens_reserves"),
            real_sol_reserves_lamports: i64k(&v["real_sol_reserves_lamports"], "real_sol_reserves_lamports"),
            real_tokens_reserves: i64k(&v["real_tokens_reserves"], "real_tokens_reserves"),
            curve_price_sol_per_raw_token: f64_of(&v["curve_price_sol_per_raw_token"]),
            curve_k: s_of(&v["curve_k"]),
            curve_progress: f64_of(&v["curve_progress"]),
            curve_regime: s_of(&v["curve_regime"]),
        },
        other => panic!("unknown curve kind {other}"),
    }
}

fn opt_slot(v: &Value) -> Option<i64> {
    match v.as_str() {
        Some("None") | None => None,
        Some(s) => Some(s.parse().expect("reserve_slot")),
    }
}

fn amm_state(v: &Value) -> AmmState {
    match v["kind"].as_str().expect("amm.kind") {
        "absent" => AmmState::Absent { reason: s_of(&v["reason"]) },
        "present" => AmmState::Present {
            pool: s_of(&v["pool"]),
            staleness_ms: i64k(&v["staleness_ms"], "staleness_ms"),
            pricing_eligible: v["pricing_eligible"].as_bool().expect("pricing_eligible"),
            base_reserves_raw: i64k(&v["base_reserves_raw"], "base_reserves_raw"),
            quote_reserves_lamports: i64k(&v["quote_reserves_lamports"], "quote_reserves_lamports"),
            amm_price_sol_per_raw_token: f64_of(&v["amm_price_sol_per_raw_token"]),
            reserve_slot: opt_slot(&v["reserve_slot"]),
        },
        other => panic!("unknown amm kind {other}"),
    }
}

fn decision_bundle(v: &Value) -> DecisionBundle {
    for k in ["t_dec_ms", "n_prior_trades", "buy_count", "sell_count", "unique_traders",
              "buy_volume_lamports", "sell_volume_lamports", "net_flow_lamports"] {
        assert!(!v[k].is_null(), "null int field {k}");
    }
    assert!(!v["dev"]["creator_known"].is_null(), "null dev.creator_known");
    let e = &v["enriched"];
    let d = &v["dev"];
    DecisionBundle {
        t_dec_ms: i64k(&v["t_dec_ms"], "t_dec_ms"),
        age_s: pynum(&v["age_s"]),
        last_trade_age_s: pynum(&v["last_trade_age_s"]),
        venue: s_of(&v["venue"]),
        curve_present: v["curve_present"].as_bool().expect("curve_present"),
        evidence_status: s_of(&v["evidence_status"]),
        n_prior_trades: i64k(&v["n_prior_trades"], "n_prior_trades"),
        buy_count: i64k(&v["buy_count"], "buy_count"),
        sell_count: i64k(&v["sell_count"], "sell_count"),
        unique_traders: i64k(&v["unique_traders"], "unique_traders"),
        price_lamports_per_raw_token: pynum(&v["price_lamports_per_raw_token"]),
        ret_5s_bp: pynum_opt(&v["ret_5s_bp"]),
        ret_30s_bp: pynum_opt(&v["ret_30s_bp"]),
        vol_30s_bp: pynum_opt(&v["vol_30s_bp"]),
        buy_volume_lamports: i64k(&v["buy_volume_lamports"], "buy_volume_lamports"),
        sell_volume_lamports: i64k(&v["sell_volume_lamports"], "sell_volume_lamports"),
        net_flow_lamports: i64k(&v["net_flow_lamports"], "net_flow_lamports"),
        top1_trader_share: pynum(&v["top1_trader_share"]),
        top5_trader_share: pynum(&v["top5_trader_share"]),
        buyer_seller_ratio: pynum_opt(&v["buyer_seller_ratio"]),
        enriched: EnrichedCandidate {
            mcap_sol_at_t: pynum_opt(&e["mcap_sol_at_t"]),
            mcap_source: s_of(&e["mcap_source"]),
            holders_at_t: pynum(&e["holders_at_t"]),
            top1_float_share: pynum(&e["top1_float_share"]),
            top5_float_share: pynum(&e["top5_float_share"]),
            holder_hhi: pynum(&e["holder_hhi"]),
            bundle_slots: pynum(&e["bundle_slots"]),
            bundle_wallets: pynum(&e["bundle_wallets"]),
            volume_sol_at_t: pynum(&e["volume_sol_at_t"]),
            wash_ratio: pynum(&e["wash_ratio"]),
        },
        dev: pump_quant_proposal::decision::DevHistoryDecision {
            creator_past_launches: opt_i64(&d["creator_past_launches"]),
            creator_known: i64_of(&d["creator_known"]),
        },
        flow: flow_state(&v["flow"]),
        flow_no_prior: v["flow_no_prior"].as_bool().unwrap_or(false),
        curve: curve_state(&v["curve"]),
        amm: amm_state(&v["amm"]),
        size_depth_sol: v["size_depth_sol"].as_f64(),
        size_amm: v["size_amm"].as_bool().unwrap_or(false),
    }
}

fn mgmt_opt(map: &Value, key: &str) -> Option<PyNum> {
    match map.get(key) {
        Some(Value::Null) | None => None,
        Some(v) => Some(pynum(v)),
    }
}

fn management_bundle(v: &Value) -> ManagementBundle {
    let e = &v["enriched"];
    let d = &v["dev"];
    ManagementBundle {
        mint: s_of(&v["mint"]),
        t_dec: i64k(&v["t_dec"], "t_dec"),
        step: i64k(&v["step"], "step"),
        venue: s_of(&v["venue"]),
        market: s_of(&v["market"]),
        depth_sol: f64_of(&v["depth_sol"]),
        mark: f64_of(&v["mark"]),
        entry_px: f64_of(&v["entry_px"]),
        upnl_bp: f64_of(&v["upnl_bp"]),
        held_s: f64_of(&v["held_s"]),
        mfe_bp: f64_of(&v["mfe_bp"]),
        mae_bp: f64_of(&v["mae_bp"]),
        qty_pre: f64_of(&v["qty_pre"]),
        cash_pre: f64_of(&v["cash_pre"]),
        enriched: EnrichedManagement {
            holders_at_t: mgmt_opt(e, "holders_at_t"),
            top1_float_share: mgmt_opt(e, "top1_float_share"),
            holder_hhi: mgmt_opt(e, "holder_hhi"),
            mcap_sol_at_t: mgmt_opt(e, "mcap_sol_at_t"),
            bundle_wallets: mgmt_opt(e, "bundle_wallets"),
            round_trip_wallets: mgmt_opt(e, "round_trip_wallets"),
            creator_past_launches: mgmt_opt(e, "creator_past_launches"),
            creator_known: mgmt_opt(e, "creator_known"),
            wash_ratio: mgmt_opt(e, "wash_ratio"),
        },
        dev: DevHistoryManagement {
            creator_past_launches: mgmt_opt(d, "creator_past_launches"),
            creator_known: mgmt_opt(d, "creator_known"),
            bundle_wallets: mgmt_opt(d, "bundle_wallets"),
            wash_ratio: mgmt_opt(d, "wash_ratio"),
        },
        flow: flow_state(&v["flow"]),
        flow_no_prior: v["flow_no_prior"].as_bool().unwrap_or(false),
    }
}

fn run_fixtures(path: &str, render: impl Fn(&Value) -> String) -> usize {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let mut n = 0usize;
    for (lineno, line) in raw.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{path}:{}: bad json: {e}", lineno + 1));
        let expected = row["expected"].as_str().expect("expected string");
        let got = render(&row["input"]);
        assert_eq!(
            got,
            expected,
            "\n{path}:{}: rendered prompt differs from the corpus",
            lineno + 1
        );
        n += 1;
    }
    n
}

#[test]
fn decision_prompts_are_byte_identical_to_the_corpus() {
    let n = run_fixtures("tests/fixtures/decision.jsonl", |input| {
        render_decision(&decision_bundle(input))
    });
    assert!(n >= 150, "expected the full fixture set, ran {n}");
}

#[test]
fn management_prompts_are_byte_identical_to_the_corpus() {
    let n = run_fixtures("tests/fixtures/management.jsonl", |input| {
        render_management(&management_bundle(input))
    });
    assert!(n >= 140, "expected the full fixture set, ran {n}");
}
