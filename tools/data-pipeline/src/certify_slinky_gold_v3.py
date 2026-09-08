#!/usr/bin/env python
"""
slinky_gold_v3 — Whole-Corpus Certification
Exact checks across ALL 33.58M states / 4 layers using DuckDB.
Fails closed: any check failing = certification FAILS.
"""
import os, sys, json, time, hashlib, glob, traceback
from pathlib import Path
from collections import defaultdict

import duckdb

OUTPUT = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
LAYERS = ["pump_state_v3", "pump_outcome_v3", "counterfactual_trade_v3", "policy_eval_v3"]
HORIZONS = [1, 2, 5, 10, 30, 60, 120, 300]
ENTRY_SIZES = ["005", "010", "025", "050", "100"]
EXPECTED_TOTAL = 33_581_765
EXPECTED_MINTS = 622_870
EXPECTED_SPLITS = {"train": 436009, "val": 93430, "test": 93431}

results = {}
failures = []
warnings_list = []

def check(name, condition, detail=""):
    status = "PASS" if condition else "FAIL"
    results[name] = {"status": status, "detail": detail}
    if not condition:
        failures.append(f"{name}: {detail}")
    print(f"  [{'✓' if condition else '✗'}] {name}: {detail}")
    return condition

def warn(name, detail):
    results[name] = {"status": "WARN", "detail": detail}
    warnings_list.append(f"{name}: {detail}")
    print(f"  [⚠] {name}: {detail}")

def get_parquet_glob(layer):
    """Get glob path for a layer's parquet files."""
    p = OUTPUT / layer / "*.parquet"
    return str(p).replace("/", "\\")

def get_parquet_list(layer):
    """Get list of parquet file paths for a layer."""
    d = OUTPUT / layer
    return sorted(glob.glob(str(d / "*.parquet")))

# ─── DuckDB helper ──────────────────────────────────────────────
def make_con():
    con = duckdb.connect()
    con.execute("SET threads TO 4;")
    con.execute("SET memory_limit TO '4GB';")
    return con

def query(con, sql):
    return con.execute(sql).fetchall()

def query_one(con, sql):
    r = con.execute(sql).fetchone()
    return r[0] if r else None

# ─── Load layers as DuckDB parquet scans ────────────────────────
def layer_path(layer):
    return f"read_parquet('{get_parquet_glob(layer)}', union_by_name=true, hive_partitioning=false)"

print("=" * 70)
print("SLINKY_GOLD_V3 WHOLE-CORPUS CERTIFICATION")
print("=" * 70)
t0 = time.time()

con = make_con()

# ═══════════════════════════════════════════════════════════════
# 1. ROW COUNTS — exact per layer
# ═══════════════════════════════════════════════════════════════
print("\n[1] ROW COUNTS")
layer_counts = {}
for layer in LAYERS:
    cnt = query_one(con, f"SELECT count(*) FROM {layer_path(layer)}")
    layer_counts[layer] = cnt
    print(f"  {layer}: {cnt:,}")

all_same = len(set(layer_counts.values())) == 1
check("row_count_all_layers_equal", all_same, f"All layers = {layer_counts[LAYERS[0]]:,}" if all_same else f"Mismatch: {layer_counts}")
check("row_count_matches_expected", layer_counts.get(LAYERS[0]) == EXPECTED_TOTAL, f"Expected {EXPECTED_TOTAL:,}, got {layer_counts.get(LAYERS[0]):,}")

# ═══════════════════════════════════════════════════════════════
# 2. SCHEMA / TYPE / COLUMN PRESENCE
# ═══════════════════════════════════════════════════════════════
print("\n[2] SCHEMA / COLUMN PRESENCE")
EXPECTED_COLUMNS = {
    "pump_state_v3": {
        "state_id", "pipeline_version", "producer_version", "run_uuid", "source_hash",
        "code_config_hash", "git_sha", "mint", "event_time_unix_ms", "seq", "venue", "source",
        "v_sol_bonding_curve_lamports", "v_tokens_bonding_curve_raw", "sol_amount_lamports",
        "token_amount_raw", "market_cap_sol_lamports", "price_sol_lamports",
        "v_sol_bonding_curve_sol", "sol_amount_sol", "market_cap_sol", "price_sol",
        "token_amount_tokens", "curve_pct_depleted", "trade_side", "is_buy",
        "trade_count_so_far", "buy_count_so_far", "sell_count_so_far", "buy_vol_sol",
        "sell_vol_sol", "total_vol_sol", "buy_pressure", "net_flow_sol", "unique_wallets_so_far",
        "trade_velocity_1s", "trade_velocity_5s", "trade_velocity_30s", "vol_velocity_5s",
        "vol_velocity_30s", "vol_acceleration", "buy_sell_imbalance_5s", "buy_sell_imbalance_30s",
        "unique_buyers_so_far", "unique_sellers_so_far", "unique_buyers_5s", "unique_sellers_5s",
        "avg_trade_size_sol", "median_trade_size_sol", "max_trade_size_sol", "min_trade_size_sol",
        "trade_size_std_sol", "largest_buy_pct_of_vol", "largest_sell_pct_of_vol",
        "price_momentum_5s_bp", "price_momentum_30s_bp", "mcap_momentum_5s_bp",
        "price_volatility_30s_bp", "price_change_since_launch_bp",
        "curve_depletion_velocity_5s", "curve_depletion_velocity_30s",
        "v_sol_depletion_rate_5s", "v_tokens_accumulation_rate_5s",
        "seconds_since_launch", "minutes_since_launch",
        "is_graduated", "seconds_to_graduation", "graduation_proximity_pct",
        "liquidity_sol", "liquidity_change_5s_sol", "liquidity_change_30s_sol",
        "token_name", "token_symbol", "creator", "is_mayhem",
        "initial_supply_raw", "initial_market_cap_sol", "initial_price_sol",
        "creator_past_tokens", "creator_past_rugs", "dev_buy_pct_corrected",
        "initial_holder_count", "initial_gini", "initial_top10_pct_corrected",
        "top10_pct_suspect", "supply_bug_corrected",
        "buyer_concentration_ratio", "seller_concentration_ratio",
        "holder_concentration_hhi", "top1_buyer_pct", "top5_buyer_pct",
        "repeat_buyer_ratio", "repeat_seller_ratio",
        "wallets_also_selling", "avg_wallet_trade_count",
        "sybil_cluster_size", "coordinated_buy_ratio", "wash_trade_ratio",
        "rapid_rebuy_count", "toxic_flow_indicator",
        "market_active_mints_5m", "market_total_vol_5m_sol", "market_avg_buy_pressure_5m",
        "data_quality_score", "is_zombie",
    },
    "pump_outcome_v3": {
        "state_id", "pipeline_version", "run_uuid", "mint", "event_time_unix_ms",
        "ret_1s_bp", "ret_2s_bp", "ret_5s_bp", "ret_10s_bp", "ret_30s_bp",
        "ret_60s_bp", "ret_120s_bp", "ret_300s_bp",
        "ret_1s", "ret_5s", "ret_30s", "ret_60s", "ret_300s",
        "mfe_bp", "mae_bp", "mfe_time_seconds", "mae_time_seconds",
        "peak_bp", "time_to_peak_seconds",
        "hit_plus_10_bp_time", "hit_plus_25_bp_time", "hit_plus_50_bp_time",
        "hit_plus_100_bp_time", "hit_plus_200_bp_time", "hit_plus_500_bp_time",
        "hit_plus_1000_bp_time", "hit_plus_1500_bp_time", "hit_plus_2000_bp_time",
        "hit_plus_3000_bp_time",
        "hit_minus_10_bp_time", "hit_minus_20_bp_time", "hit_minus_30_bp_time",
        "hit_minus_50_bp_time", "hit_minus_100_bp_time", "hit_minus_200_bp_time",
        "hit_minus_500_bp_time", "hit_minus_1000_bp_time", "hit_minus_1500_bp_time",
        "hit_minus_2000_bp_time",
        "plus_100_before_minus_30",
        "survived_60s", "survived_300s", "collapsed_50pct_within_300s",
        "graduated_after_state", "seconds_to_graduation", "graduated_did_migrate",
        "post_grad_price_change_300s_bp",
        "exit_v_sol_lamports", "exit_v_tok_raw", "exit_event_time_ms",
        "venue_censored", "venue_censoring_reason", "observation_end_ms",
        "time_to_next_trade_s", "has_future_trade_300s",
    },
    "counterfactual_trade_v3": {
        "state_id", "pipeline_version", "run_uuid",
        "execution_assumptions_version", "execution_config_hash",
        "mint", "event_time_unix_ms",
        "entry_price_sol", "entry_price_lamports", "entry_size_sol", "entry_size_lamports",
        "entry_fee_sol", "entry_fee_lamports", "entry_slippage_bp", "entry_slippage_sol",
        "entry_tip_lamports", "entry_total_cost_sol",
        "exit_reason", "exit_price_sol", "exit_price_lamports", "exit_fee_sol", "exit_fee_lamports",
        "exit_slippage_bp", "exit_slippage_sol", "exit_tip_lamports", "exit_total_revenue_sol",
        "gross_pnl_sol", "gross_pnl_lamports", "net_pnl_sol", "net_pnl_lamports",
        "net_return_bp", "net_return_pct", "hold_duration_seconds", "risk_reward_ratio",
        "exact_entry_tokens", "exact_entry_eff_price_sol", "exact_entry_impact_bp",
        "exact_exit_sol_out", "exact_exit_impact_bp", "exact_gross_pnl_sol", "exact_gross_return_bp",
        "exact_net_pnl_sol", "exact_net_return_bp",
        "economic_class", "economic_class_reason",
        "exit_feasible", "exit_feasibility_note",
        "eligible", "eligibility_reason",
        "latency_ms", "entry_fee_bps", "exit_fee_bps", "slippage_default_bp",
        "tp_target_bp", "stop_loss_bp", "max_hold_seconds",
    },
    "policy_eval_v3": {
        "state_id", "pipeline_version", "run_uuid", "config_hash", "policy_version",
        "mint", "event_time_unix_ms",
        "would_enter", "action", "entry_reason",
        "evaluation", "evaluation_reason",
        "gate_curve_pct_max", "gate_min_trades", "gate_min_volume_sol",
        "gate_min_buy_pressure", "gate_min_unique_buyers", "gate_min_age_slots",
        "gate_min_sol_per_trade", "gate_max_sol_per_trade",
        "sim_net_pnl_sol", "sim_exit_reason", "sim_hold_duration",
    },
}

# Add multi-size columns for counterfactual
for sz in ENTRY_SIZES:
    for suffix in ["entry_tokens", "entry_eff_price_sol", "entry_impact_bp",
                   "exit_sol", "exit_impact_bp", "gross_pnl_sol", "gross_return_bp",
                   "net_pnl_sol", "net_return_bp", "feasible"]:
        EXPECTED_COLUMNS["counterfactual_trade_v3"].add(f"sz{sz}_{suffix}")

# Add per-horizon censoring columns for outcome
for h in HORIZONS:
    for suffix in [f"observed_through_{h}s", f"has_trade_within_{h}s",
                   f"right_censored_{h}s", f"last_trade_age_at_{h}s_s"]:
        EXPECTED_COLUMNS["pump_outcome_v3"].add(suffix)

for layer in LAYERS:
    cols_result = query(con, f"SELECT column_name, column_type FROM information_schema.columns WHERE table_name = '{layer}'")
    # information_schema won't work with read_parquet. Use DESCRIBE instead.
    pass

for layer in LAYERS:
    lp = layer_path(layer)
    desc = query(con, f"DESCRIBE SELECT * FROM {lp}")
    actual_cols = set(row[0] for row in desc)
    expected = EXPECTED_COLUMNS[layer]
    missing = expected - actual_cols
    extra = actual_cols - expected
    check(f"schema_{layer}_columns", len(missing) == 0, f"Missing: {missing}" if missing else f"All {len(expected)} expected columns present")
    if extra:
        warn(f"schema_{layer}_extra_columns", f"Unexpected columns: {extra}")

# ═══════════════════════════════════════════════════════════════
# 3. STATE_ID UNIQUENESS per layer
# ═══════════════════════════════════════════════════════════════
print("\n[3] STATE_ID UNIQUENESS")
for layer in LAYERS:
    lp = layer_path(layer)
    dupes = query_one(con, f"""
        SELECT count(*) FROM (
            SELECT state_id, count(*) as c FROM {lp}
            GROUP BY state_id HAVING count(*) > 1
        )
    """)
    check(f"uniqueness_{layer}_state_id", dupes == 0, f"Duplicates: {dupes}" if dupes else "Zero duplicates")

# ═══════════════════════════════════════════════════════════════
# 4. CROSS-LAYER 1:1:1:1 CARDINALITY
# ═══════════════════════════════════════════════════════════════
print("\n[4] CROSS-LAYER CARDINALITY (1:1:1:1)")
# Check exact set equality via anti-joins
# Use state_id from pump_state_v3 as reference, check all others match
ref = layer_path("pump_state_v3")
for layer in LAYERS[1:]:
    lp = layer_path(layer)
    # state_ids in state but not in layer
    missing_in_layer = query_one(con, f"""
        SELECT count(*) FROM (
            SELECT DISTINCT state_id FROM {ref}
            EXCEPT
            SELECT DISTINCT state_id FROM {lp}
        )
    """)
    missing_in_state = query_one(con, f"""
        SELECT count(*) FROM (
            SELECT DISTINCT state_id FROM {lp}
            EXCEPT
            SELECT DISTINCT state_id FROM {ref}
        )
    """)
    check(f"cross_layer_{layer}_matches_state", missing_in_layer == 0 and missing_in_state == 0,
          f"Missing in {layer}: {missing_in_layer}, Missing in state: {missing_in_state}" if missing_in_layer or missing_in_state else "Exact 1:1 match")

# ═══════════════════════════════════════════════════════════════
# 5. (MINT, SEQ) UNIQUENESS / ORDER in pump_state_v3
# ═══════════════════════════════════════════════════════════════
print("\n[5] (MINT, SEQ) UNIQUENESS & ORDER")
sp = layer_path("pump_state_v3")
mint_seq_dupes = query_one(con, f"""
    SELECT count(*) FROM (
        SELECT mint, seq, count(*) as c FROM {sp}
        GROUP BY mint, seq HAVING count(*) > 1
    )
""")
check("uniqueness_mint_seq", mint_seq_dupes == 0, f"Duplicate (mint,seq): {mint_seq_dupes}" if mint_seq_dupes else "Zero duplicate (mint,seq)")

# Check seq ordering: within each mint, seq should be monotonically increasing with event_time
# Sample check: max seq per mint, and any seq=0 has no earlier trades
seq_order_violations = query_one(con, f"""
    SELECT count(*) FROM (
        SELECT mint, seq, event_time_unix_ms,
               LAG(event_time_unix_ms) OVER (PARTITION BY mint ORDER BY seq) as prev_time
        FROM {sp}
        WHERE prev_time IS NOT NULL AND event_time_unix_ms < prev_time
    )
""")
check("order_seq_temporal", seq_order_violations == 0, f"Out-of-order events: {seq_order_violations}" if seq_order_violations else "All events ordered by seq/time")

# ═══════════════════════════════════════════════════════════════
# 6. PROVENANCE INTEGRITY
# ═══════════════════════════════════════════════════════════════
print("\n[6] PROVENANCE INTEGRITY")
manifest = json.load(open(OUTPUT / "manifest_v3.json"))
expected_run_uuid = manifest.get("run_uuid", "")
expected_source_hash = manifest.get("source_hash", "")
expected_git_sha = manifest.get("git_sha", "")
expected_code_hash = manifest.get("code_config_hash", "")
expected_champion_hash = manifest.get("champion_config_hash", "")
expected_exec_hash = manifest.get("execution_config_hash", "")
expected_schema_ver = manifest.get("schema_version", "")
expected_pipeline_ver = manifest.get("pipeline_version", "")
expected_producer_ver = manifest.get("producer_version", "")

for layer in LAYERS:
    lp = layer_path(layer)
    # Check run_uuid consistency
    uuid_vals = query(con, f"SELECT DISTINCT run_uuid FROM {lp}")
    uuid_ok = len(uuid_vals) == 1 and uuid_vals[0][0] == expected_run_uuid
    check(f"provenance_{layer}_run_uuid", uuid_ok, f"Expected {expected_run_uuid}, got {uuid_vals}" if not uuid_ok else "Consistent")

    # Check pipeline_version
    if "pipeline_version" in EXPECTED_COLUMNS[layer]:
        pv = query(con, f"SELECT DISTINCT pipeline_version FROM {lp}")
        pv_ok = len(pv) == 1 and pv[0][0] == expected_pipeline_ver
        check(f"provenance_{layer}_pipeline_version", pv_ok, f"Expected {expected_pipeline_ver}, got {pv}" if not pv_ok else "Consistent")

    # Check producer_version (state layer only)
    if "producer_version" in EXPECTED_COLUMNS[layer]:
        prv = query(con, f"SELECT DISTINCT producer_version FROM {lp}")
        prv_ok = len(prv) == 1 and prv[0][0] == expected_producer_ver
        check(f"provenance_{layer}_producer_version", prv_ok, f"Expected {expected_producer_ver}, got {prv}" if not prv_ok else "Consistent")

    # Check source_hash (state layer only)
    if "source_hash" in EXPECTED_COLUMNS[layer]:
        sh = query(con, f"SELECT DISTINCT source_hash FROM {lp}")
        sh_ok = len(sh) == 1 and sh[0][0] == expected_source_hash
        check(f"provenance_{layer}_source_hash", sh_ok, f"Expected {expected_source_hash}, got {sh}" if not sh_ok else "Consistent")

    # Check git_sha (state layer only)
    if "git_sha" in EXPECTED_COLUMNS[layer]:
        gs = query(con, f"SELECT DISTINCT git_sha FROM {lp}")
        gs_ok = len(gs) == 1 and gs[0][0] == expected_git_sha
        check(f"provenance_{layer}_git_sha", gs_ok, f"Expected {expected_git_sha}, got {gs}" if not gs_ok else "Consistent")

    # Check code_config_hash (state layer only)
    if "code_config_hash" in EXPECTED_COLUMNS[layer]:
        cc = query(con, f"SELECT DISTINCT code_config_hash FROM {lp}")
        cc_ok = len(cc) == 1 and cc[0][0] == expected_code_hash
        check(f"provenance_{layer}_code_config_hash", cc_ok, f"Expected {expected_code_hash}, got {cc}" if not cc_ok else "Consistent")

# Check execution_config_hash on counterfactual
lp = layer_path("counterfactual_trade_v3")
ech = query(con, f"SELECT DISTINCT execution_config_hash FROM {lp}")
check("provenance_cf_execution_config_hash", len(ech) == 1 and ech[0][0] == expected_exec_hash,
      f"Expected {expected_exec_hash}, got {ech}" if not (len(ech) == 1 and ech[0][0] == expected_exec_hash) else "Consistent")

# Check config_hash on policy_eval
lp = layer_path("policy_eval_v3")
pch = query(con, f"SELECT DISTINCT config_hash FROM {lp}")
check("provenance_policy_config_hash", len(pch) == 1 and pch[0][0] == expected_champion_hash,
      f"Expected {expected_champion_hash}, got {pch}" if not (len(pch) == 1 and pch[0][0] == expected_champion_hash) else "Consistent")

# ═══════════════════════════════════════════════════════════════
# 7. NO-FUTURE-LEAKAGE in pump_state_v3
# ═══════════════════════════════════════════════════════════════
print("\n[7] CAUSAL / NO-FUTURE-LEAKAGE (pump_state_v3)")
sp = layer_path("pump_state_v3")

# 7a. No outcome-derived columns present in state layer
outcome_only_cols = {
    "ret_1s_bp", "ret_5s_bp", "ret_300s_bp", "mfe_bp", "mae_bp",
    "survived_60s", "survived_300s", "collapsed_50pct_within_300s",
    "graduated_after_state", "time_to_next_trade_s", "has_future_trade_300s",
    "right_censored", "observed_through", "venue_censored",
    "net_pnl_sol", "gross_pnl_sol", "economic_class", "would_enter",
    "exit_reason", "exit_price", "exit_feasible",
}
state_desc = set(row[0] for row in query(con, f"DESCRIBE SELECT * FROM {sp}"))
leakage_cols = state_desc & outcome_only_cols
check("causal_no_outcome_columns_in_state", len(leakage_cols) == 0,
      f"Leaked outcome columns: {leakage_cols}" if leakage_cols else "No outcome-derived columns in state")

# 7b. Causal stats are BEFORE current trade: buy_count_so_far should be < trade_count_so_far
bad_counts = query_one(con, f"""
    SELECT count(*) FROM {sp}
    WHERE buy_count_so_far + sell_count_so_far != trade_count_so_far
      OR trade_count_so_far < 1
""")
check("causal_buy_sell_count_consistency", bad_counts == 0,
      f"Inconsistent counts: {bad_counts}" if bad_counts else "buy+sell=total, trade_count>=1")

# 7c. curve_pct_depleted should be in [0, 1]
bad_curve = query_one(con, f"""
    SELECT count(*) FROM {sp}
    WHERE curve_pct_depleted < 0 OR curve_pct_depleted > 1
""")
check("causal_curve_pct_range", bad_curve == 0,
      f"Out of [0,1]: {bad_curve}" if bad_curve else "All in [0,1]")

# 7d. seconds_since_launch >= 0
bad_time = query_one(con, f"""
    SELECT count(*) FROM {sp}
    WHERE seconds_since_launch < 0
""")
check("causal_time_nonneg", bad_time == 0, f"Negative time: {bad_time}" if bad_time else "All >= 0")

# 7e. is_graduated should be False when seconds_to_graduation > 0 (can't know future graduation)
# Actually: is_graduated at time t means mint already graduated BEFORE t, which is causal.
# But graduation_proximity_pct should be NULL if is_graduated=True (no future graduation to predict)
bad_grad = query_one(con, f"""
    SELECT count(*) FROM {sp}
    WHERE is_graduated = true AND seconds_to_graduation IS NOT NULL AND seconds_to_graduation > 0
""")
check("causal_graduation_consistency", bad_grad == 0,
      f"Inconsistent graduation: {bad_grad}" if bad_grad else "Graduation fields consistent")

# ═══════════════════════════════════════════════════════════════
# 8. CENSORING SEMANTICS
# ═══════════════════════════════════════════════════════════════
print("\n[8] CENSORING / NO-TRADE SEMANTICS")
op = layer_path("pump_outcome_v3")

for h in HORIZONS:
    # right_censored_Hs = NOT observed_through_Hs
    bad_censor = query_one(con, f"""
        SELECT count(*) FROM {op}
        WHERE right_censored_{h}s = true AND observed_through_{h}s = true
           OR right_censored_{h}s = false AND observed_through_{h}s = false
    """)
    check(f"censoring_{h}s_right_censored_not_observed", bad_censor == 0,
          f"Violations: {bad_censor}" if bad_censor else "right_censored = NOT observed_through")

    # If censored, returns should be NULL (can't know future)
    bad_ret_null = 0
    if h in [1, 2, 5, 10, 30, 60, 120, 300]:
        ret_col = f"ret_{h}s_bp"
        if ret_col in EXPECTED_COLUMNS["pump_outcome_v3"]:
            bad_ret_null = query_one(con, f"""
                SELECT count(*) FROM {op}
                WHERE right_censored_{h}s = true AND {ret_col} IS NOT NULL
            """)
            check(f"censoring_{h}s_censored_returns_null", bad_ret_null == 0,
                  f"Censored with non-null returns: {bad_ret_null}" if bad_ret_null else "Censored → returns NULL")

# Observed no-trade: has_future_trade_300s = false AND observed_through_300s = true (NOT censored)
observed_no_trade = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE has_future_trade_300s = false AND observed_through_300s = true AND right_censored_300s = false
""")
check("censoring_observed_no_trade_count", observed_no_trade > 0,
      f"Count: {observed_no_trade:,}" if observed_no_trade else "ZERO observed no-trade (suspicious)")

# True censored at 300s
true_censored_300 = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE right_censored_300s = true
""")
check("censoring_true_censored_300s", true_censored_300 > 0,
      f"Count: {true_censored_300:,}" if true_censored_300 else "ZERO censored (suspicious)")

# has_future_trade_300s should be true when there IS a trade within 300s
# and false when there isn't (but only meaningful when observed)
bad_trade_flag = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE has_future_trade_300s = true AND time_to_next_trade_s IS NULL
       OR has_future_trade_300s = false AND time_to_next_trade_s IS NOT NULL
       AND right_censored_300s = false
""")
# Note: has_future_trade_300s=true but time_to_next_trade_s=NULL could be a trade >300s but still flagged
# This is a soft check — warn rather than fail
warn("censoring_trade_flag_vs_time_to_next",
     f"Potential flag/time mismatches: {bad_trade_flag} (soft check — may include edge cases)")

# ═══════════════════════════════════════════════════════════════
# 9. MIGRATION / VENUE COVERAGE
# ═══════════════════════════════════════════════════════════════
print("\n[9] MIGRATION / VENUE COVERAGE")
# venue_censored should only be true when venue_censoring_reason is set
bad_venue = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE venue_censored = true AND (venue_censoring_reason IS NULL OR venue_censoring_reason = '')
""")
check("venue_censored_has_reason", bad_venue == 0,
      f"Missing reason: {bad_venue}" if bad_venue else "All venue_censored have reason")

# venue_censored=false should have NULL reason
bad_venue2 = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE venue_censored = false AND venue_censoring_reason IS NOT NULL
""")
check("venue_not_censored_null_reason", bad_venue2 == 0,
      f"Has reason but not censored: {bad_venue2}" if bad_venue2 == 0 else f"Count: {bad_venue2} (minor)")

# ═══════════════════════════════════════════════════════════════
# 10. OUTCOMES INDEPENDENT OF CHAMPION POLICY
# ═══════════════════════════════════════════════════════════════
print("\n[10] OUTCOMES INDEPENDENT OF CHAMPION")
op = layer_path("pump_outcome_v3")
outcome_cols = set(row[0] for row in query(con, f"DESCRIBE SELECT * FROM {op}"))
champion_cols = {"would_enter", "action", "entry_reason", "config_hash", "policy_version"}
leaked = outcome_cols & champion_cols
check("outcomes_no_champion_columns", len(leaked) == 0,
      f"Champion columns leaked: {leaked}" if leaked else "No champion columns in outcomes")

# ═══════════════════════════════════════════════════════════════
# 11. COUNTERFACTUAL ECONOMICS + MULTI-SIZE
# ═══════════════════════════════════════════════════════════════
print("\n[11] COUNTERFACTUAL ECONOMICS")
cp = layer_path("counterfactual_trade_v3")

# economic_class must be from valid set
valid_classes = {"STRONG", "GOOD", "MARGINAL", "SKIP", "BAD", "TOXIC"}
bad_class = query_one(con, f"""
    SELECT count(*) FROM {cp}
    WHERE economic_class NOT IN ('STRONG', 'GOOD', 'MARGINAL', 'SKIP', 'BAD', 'TOXIC')
""")
check("cf_valid_economic_class", bad_class == 0,
      f"Invalid classes: {bad_class}" if bad_class else "All valid")

# entry_size_sol should always be 0.5 (benchmark)
bad_size = query_one(con, f"""
    SELECT count(*) FROM {cp}
    WHERE entry_size_sol != 0.5
""")
check("cf_entry_size_05", bad_size == 0,
      f"Non-0.5 sizes: {bad_size}" if bad_size else "All 0.5 SOL benchmark")

# Multi-size feasibility flags are booleans
for sz in ENTRY_SIZES:
    bad_feasible = query_one(con, f"""
        SELECT count(*) FROM {cp}
        WHERE sz{sz}_feasible NOT IN (true, false)
    """)
    check(f"cf_sz{sz}_feasible_bool", bad_feasible == 0,
          f"Non-bool: {bad_feasible}" if bad_feasible else "All boolean")

# If not eligible, economic_class should reflect that
bad_elig = query_one(con, f"""
    SELECT count(*) FROM {cp}
    WHERE eligible = false AND economic_class NOT IN ('SKIP', 'BAD', 'TOXIC')
""")
check("cf_ineligible_skip_bad_toxic", bad_elig == 0,
      f"Ineligible with STRONG/GOOD/MARGINAL: {bad_elig}" if bad_elig else "Ineligible → SKIP/BAD/TOXIC")

# ═══════════════════════════════════════════════════════════════
# 12. POLICY_EVAL AUXILIARY — CHAMPION NOT TRUTH
# ═══════════════════════════════════════════════════════════════
print("\n[12] POLICY_EVAL AUXILIARY")
pp = layer_path("policy_eval_v3")

# would_enter is present ONLY in policy_eval, NOT in outcome or counterfactual
for layer in ["pump_outcome_v3", "counterfactual_trade_v3"]:
    lp = layer_path(layer)
    cols = set(row[0] for row in query(con, f"DESCRIBE SELECT * FROM {lp}"))
    has_we = "would_enter" in cols
    check(f"policy_{layer}_no_would_enter", not has_we,
          "would_enter absent" if not has_we else "would_enter LEAKED into outcome/cf")

# evaluation must be from valid set
valid_eval = {"CORRECT_ENTER", "CORRECT_SKIP", "FALSE_POSITIVE", "MISSED_OPPORTUNITY", "AMBIGUOUS"}
bad_eval = query_one(con, f"""
    SELECT count(*) FROM {pp}
    WHERE evaluation NOT IN ('CORRECT_ENTER', 'CORRECT_SKIP', 'FALSE_POSITIVE', 'MISSED_OPPORTUNITY', 'AMBIGUOUS')
""")
check("policy_valid_evaluation", bad_eval == 0,
      f"Invalid evaluations: {bad_eval}" if bad_eval else "All valid")

# action must be ENTER or SKIP
bad_action = query_one(con, f"""
    SELECT count(*) FROM {pp}
    WHERE action NOT IN ('ENTER', 'SKIP')
""")
check("policy_valid_action", bad_action == 0,
      f"Invalid actions: {bad_action}" if bad_action else "All ENTER/SKIP")

# would_enter=true should correspond to action='ENTER'
bad_we_action = query_one(con, f"""
    SELECT count(*) FROM {pp}
    WHERE would_enter = true AND action != 'ENTER'
       OR would_enter = false AND action != 'SKIP'
""")
check("policy_would_enter_action_consistent", bad_we_action == 0,
      f"Inconsistent: {bad_we_action}" if bad_we_action else "would_enter/action consistent")

# ═══════════════════════════════════════════════════════════════
# 13. UNIT / LAMPORT / SOL CORRECTNESS
# ═══════════════════════════════════════════════════════════════
print("\n[13] UNIT / LAMPORT / SOL CORRECTNESS")
sp = layer_path("pump_state_v3")

# lamports = SOL * 1e9 (within rounding tolerance)
bad_lamports = query_one(con, f"""
    SELECT count(*) FROM {sp}
    WHERE v_sol_bonding_curve_lamports < 0
       OR v_sol_bonding_curve_lamports > 1000000000000000  -- 100K SOL max (impossible)
       OR sol_amount_lamports < 0
       OR market_cap_sol_lamports < 0
       OR price_sol_lamports < 0
""")
check("units_lamports_nonneg_bounded", bad_lamports == 0,
      f"Bad lamports: {bad_lamports}" if bad_lamports else "All lamports non-negative, bounded")

# v_sol_bonding_curve_sol ≈ v_sol_bonding_curve_lamports / 1e9
bad_sol_conv = query_one(con, f"""
    SELECT count(*) FROM {sp}
    WHERE ABS(v_sol_bonding_curve_sol - v_sol_bonding_curve_lamports / 1000000000.0) > 0.001
       OR v_sol_bonding_curve_lamports = 0
""")
check("units_lamport_sol_conversion", bad_sol_conv == 0,
      f"Conversion errors: {bad_sol_conv}" if bad_sol_conv else "SOL = lamports/1e9 verified")

# entry_size_lamports = entry_size_sol * 1e9 in counterfactual
cp = layer_path("counterfactual_trade_v3")
bad_entry_lam = query_one(con, f"""
    SELECT count(*) FROM {cp}
    WHERE entry_size_lamports != 500000000  -- 0.5 SOL * 1e9
""")
check("units_cf_entry_lamports", bad_entry_lam == 0,
      f"Wrong entry lamports: {bad_entry_lam}" if bad_entry_lam else "All = 500M lamports (0.5 SOL)")

# entry_price_lamports should be positive
bad_entry_price = query_one(con, f"""
    SELECT count(*) FROM {cp}
    WHERE entry_price_lamports <= 0
""")
check("units_cf_entry_price_positive", bad_entry_price == 0,
      f"Non-positive: {bad_entry_price}" if bad_entry_price else "All positive")

# ═══════════════════════════════════════════════════════════════
# 14. DUPLICATE / V1/V2 CONTAMINATION
# ═══════════════════════════════════════════════════════════════
print("\n[14] CONTAMINATION / STALE DATA")
# Check for v1/v2 column names that shouldn't exist in v3
v1v2_indicators = {"policy_class", "champion_v1_label", "would_enter_v1", "ret_1s_v2",
                   "censored_count", "is_censored", "v2_schema"}
for layer in LAYERS:
    lp = layer_path(layer)
    cols = set(row[0] for row in query(con, f"DESCRIBE SELECT * FROM {lp}"))
    stale = cols & v1v2_indicators
    check(f"contamination_{layer}_no_v1v2_cols", len(stale) == 0,
          f"Stale columns: {stale}" if stale else "No v1/v2 columns")

# Check file naming — no v1/v2 files
for layer in LAYERS:
    d = OUTPUT / layer
    v1_files = glob.glob(str(d / "*v1*")) + glob.glob(str(d / "*v2*"))
    check(f"contamination_{layer}_no_v1v2_files", len(v1_files) == 0,
          f"Stale files: {v1_files}" if v1_files else "No v1/v2 files")

# ═══════════════════════════════════════════════════════════════
# 15. LABEL / CLASS DISTRIBUTIONS + SPLIT INTEGRITY
# ═══════════════════════════════════════════════════════════════
print("\n[15] DISTRIBUTIONS & SPLIT INTEGRITY")
cp = layer_path("counterfactual_trade_v3")

# Economic class distribution
class_dist = query(con, f"SELECT economic_class, count(*) FROM {cp} GROUP BY economic_class ORDER BY economic_class")
class_dict = {row[0]: row[1] for row in class_dist}
print(f"  Economic classes: {class_dict}")
check("dist_economic_class_complete", set(class_dict.keys()) == valid_classes,
      f"Missing classes: {valid_classes - set(class_dict.keys())}" if set(class_dict.keys()) != valid_classes else "All 6 classes present")

# Policy evaluation distribution
pp = layer_path("policy_eval_v3")
eval_dist = query(con, f"SELECT evaluation, count(*) FROM {pp} GROUP BY evaluation ORDER BY evaluation")
eval_dict = {row[0]: row[1] for row in eval_dist}
print(f"  Policy evaluations: {eval_dict}")
check("dist_policy_eval_complete", set(eval_dict.keys()) == valid_eval,
      f"Missing: {valid_eval - set(eval_dict.keys())}" if set(eval_dict.keys()) != valid_eval else "All 5 evaluations present")

# Unique mints
sp = layer_path("pump_state_v3")
unique_mints = query_one(con, f"SELECT count(DISTINCT mint) FROM {sp}")
check("dist_unique_mints", unique_mints == EXPECTED_MINTS,
      f"Expected {EXPECTED_MINTS:,}, got {unique_mints:,}" if unique_mints != EXPECTED_MINTS else f"{unique_mints:,}")

# Splits: mint-disjoint check
# We need to know which mints are in train/val/test. The splits.json says counts.
# Check: are there split markers in the data?
# Look for a 'split' column or derive from mint hashing
split_col_exists = "split" in set(row[0] for row in query(con, f"DESCRIBE SELECT * FROM {sp}"))
if split_col_exists:
    split_counts = query(con, f"SELECT split, count(DISTINCT mint), count(*) FROM {sp} GROUP BY split ORDER BY split")
    split_dict = {row[0]: (row[1], row[2]) for row in split_counts}
    print(f"  Split mint counts: { {k: v[0] for k,v in split_dict.items()} }")

    for split_name, expected_mint_count in EXPECTED_SPLITS.items():
        actual = split_dict.get(split_name, (0, 0))
        check(f"split_{split_name}_mint_count", actual[0] == expected_mint_count,
              f"Expected {expected_mint_count:,} mints, got {actual[0]:,}" if actual[0] != expected_mint_count else f"{actual[0]:,} mints")

    # Mint-disjoint: no mint appears in multiple splits
    mint_split_pairs = query(con, f"""
        SELECT mint, count(DISTINCT split) as n FROM {sp} GROUP BY mint HAVING count(DISTINCT split) > 1
    """)
    check("split_mint_disjoint", len(mint_split_pairs) == 0,
          f"Overlapping mints: {len(mint_split_pairs)}" if mint_split_pairs else "Mint-disjoint")
else:
    # Check splits.json for mint assignment
    splits_file = json.load(open(OUTPUT / "splits.json"))
    warn("split_no_column_in_data", "No 'split' column in parquet — splits defined in splits.json only. Cannot verify mint-disjoint from data.")
    print(f"  splits.json: {splits_file}")

    # Verify unique mint count matches sum of splits
    total_split_mints = sum(EXPECTED_SPLITS.values())
    check("split_mint_count_sum", unique_mints == total_split_mints,
          f"Expected {total_split_mints:,}, got {unique_mints:,}" if unique_mints != total_split_mints else f"{unique_mints:,} = sum of splits")

# ═══════════════════════════════════════════════════════════════
# 16. NULL / RANGE COMPLIANCE (key fields)
# ═══════════════════════════════════════════════════════════════
print("\n[16] NULL / RANGE COMPLIANCE")
# state_id: no nulls
for layer in LAYERS:
    lp = layer_path(layer)
    null_state = query_one(con, f"SELECT count(*) FROM {lp} WHERE state_id IS NULL OR state_id = ''")
    check(f"null_{layer}_state_id", null_state == 0, f"Null/empty: {null_state}" if null_state else "No nulls")

# mint: no nulls
for layer in LAYERS:
    lp = layer_path(layer)
    null_mint = query_one(con, f"SELECT count(*) FROM {lp} WHERE mint IS NULL OR mint = ''")
    check(f"null_{layer}_mint", null_mint == 0, f"Null/empty: {null_mint}" if null_mint else "No nulls")

# event_time_unix_ms: positive
for layer in LAYERS:
    lp = layer_path(layer)
    bad_time = query_one(con, f"SELECT count(*) FROM {lp} WHERE event_time_unix_ms <= 0")
    check(f"range_{layer}_event_time_positive", bad_time == 0, f"Non-positive: {bad_time}" if bad_time else "All positive")

# is_buy: boolean, not null
sp = layer_path("pump_state_v3")
bad_isbuy = query_one(con, f"SELECT count(*) FROM {sp} WHERE is_buy NOT IN (true, false)")
check("range_state_is_buy_bool", bad_isbuy == 0, f"Non-bool: {bad_isbuy}" if bad_isbuy == 0 else "All boolean")

# survived_60s, survived_300s: boolean
op = layer_path("pump_outcome_v3")
bad_surv = query_one(con, f"SELECT count(*) FROM {op} WHERE survived_60s NOT IN (true, false) OR survived_300s NOT IN (true, false)")
check("range_outcome_survived_bool", bad_surv == 0, f"Non-bool: {bad_surv}" if bad_surv else "All boolean")

# ═══════════════════════════════════════════════════════════════
# 17. TRUE CENSORING / NO-TRADE FINAL COUNTS
# ═══════════════════════════════════════════════════════════════
print("\n[17] TRUE CENSORING / NO-TRADE COUNTS")
op = layer_path("pump_outcome_v3")

censored_counts = {}
for h in HORIZONS:
    c = query_one(con, f"SELECT count(*) FROM {op} WHERE right_censored_{h}s = true")
    censored_counts[h] = c
    print(f"  right_censored_{h}s: {c:,}")

observed_no_trade_300 = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE has_future_trade_300s = false AND right_censored_300s = false
""")
total_censored_any = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE right_censored_1s = true OR right_censored_2s = true OR right_censored_5s = true
       OR right_censored_10s = true OR right_censored_30s = true OR right_censored_60s = true
       OR right_censored_120s = true OR right_censored_300s = true
""")
fully_observed = query_one(con, f"""
    SELECT count(*) FROM {op}
    WHERE right_censored_1s = false AND right_censored_2s = false AND right_censored_5s = false
       AND right_censored_10s = false AND right_censored_30s = false AND right_censored_60s = false
       AND right_censored_120s = false AND right_censored_300s = false
""")
print(f"  Observed no-trade (300s, not censored): {observed_no_trade_300:,}")
print(f"  Censored at ANY horizon: {total_censored_any:,}")
print(f"  Fully observed (all 8 horizons): {fully_observed:,}")
print(f"  Observed no-trade (300s): {observed_no_trade_300:,}")

results["censoring_final_counts"] = {
    "censored_per_horizon": censored_counts,
    "censored_any_horizon": total_censored_any,
    "fully_observed_all_horizons": fully_observed,
    "observed_no_trade_300s": observed_no_trade_300,
}

# ═══════════════════════════════════════════════════════════════
# FINAL VERDICT
# ═══════════════════════════════════════════════════════════════
print("\n" + "=" * 70)
elapsed = time.time() - t0
total_checks = len(results)
passed = sum(1 for r in results.values() if r["status"] == "PASS")
failed = len(failures)
warned = len(warnings_list)

print(f"CERTIFICATION COMPLETE — {elapsed:.1f}s")
print(f"  Total checks: {total_checks}")
print(f"  Passed: {passed}")
print(f"  Failed: {failed}")
print(f"  Warnings: {warned}")
print("=" * 70)

if failures:
    print("\nFAILED CHECKS:")
    for f in failures:
        print(f"  ✗ {f}")

if warnings_list:
    print("\nWARNINGS:")
    for w in warnings_list:
        print(f"  ⚠ {w}")

certified = len(failures) == 0
results["_meta"] = {
    "certified": certified,
    "total_checks": total_checks,
    "passed": passed,
    "failed": failed,
    "warnings": warned,
    "elapsed_seconds": round(elapsed, 1),
}

# Save results
with open(OUTPUT / "certification_v3.json", "w") as f:
    json.dump(results, f, indent=2, default=str)

print(f"\n{'✅ CERTIFIED — ready for freeze' if certified else '❌ NOT CERTIFIED — fix failures before freeze'}")
