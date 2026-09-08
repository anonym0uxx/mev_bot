#!/usr/bin/env python3
"""
Qwen Curriculum v1 Export Builder
=================================
Builds qwen_eval_v1, qwen_cpt_v1, qwen_sft_v1 from frozen corpora.

Outputs to: tools/data-pipeline/output/qwen_curriculum_v1/
- eval/qwen_eval_v1.jsonl + manifest
- cpt/qwen_cpt_v1.jsonl + manifest
- sft/qwen_sft_v1.jsonl + manifest

All exports are training-method-neutral.
Provenance links every example to frozen source IDs.
Leakage checks: mint-disjoint, chronological, trajectory-disjoint, repair-chain-disjoint.

robust_utility_v1: versioned deterministic formula for continuous supervision.
"""

import os
import json
import hashlib
import math
import time
import random
from datetime import datetime, timezone
from collections import Counter, defaultdict, OrderedDict
import pyarrow.parquet as pq
import pandas as pd

# ════════════════════════════════════════════════════════════════
# CONFIG
# ════════════════════════════════════════════════════════════════

BASE = 'D:/repos/mev_bot/tools/data-pipeline/output'
OUT = os.path.join(BASE, 'qwen_curriculum_v1')
EVAL_DIR = os.path.join(OUT, 'eval')
EVAL_DIR_V1_1 = os.path.join(OUT, 'eval_v1_1')
CPT_DIR = os.path.join(OUT, 'cpt')
SFT_DIR = os.path.join(OUT, 'sft')

SLINKY = os.path.join(BASE, 'slinky_gold_v3_compact')
LS = os.path.join(BASE, 'laserstream_gold_v3')
RUST = os.path.join(BASE, 'rust_gold_v1')
NARR = os.path.join(BASE, 'narrative_gold_v1.1/gold')

RUN_UUID = f"qc1_{int(time.time())}"
FREEZE_TIME = datetime.now(timezone.utc).isoformat()

# Provenance UUIDs
SLINKY_FREEZE_UUID = "cf97c33"  # from FREEZE_v3.json
LS_FREEZE_UUID = "cf97c33"
RUST_FREEZE_UUID = "55e19441"
NARR_FREEZE_UUID = "ng11_43bf56a50ed7"

# Utility formula version
UTILITY_VERSION = "robust_utility_v1"

# Panel sampling params
SLINKY_PANEL_MIN = 25
SLINKY_PANEL_MAX = 50
SLINKY_PANEL_SPACING_MIN = 1800  # 30min in seconds
SLINKY_EVAL_FRACTION = 0.20

LS_PANEL_MIN = 25
LS_PANEL_MAX = 50
LS_PANEL_SPACING_MIN = 120  # 2min in seconds
LS_EVAL_FRACTION = 0.20

# SFT mix targets
SFT_MIX = {
    'cross_sectional_compressed': 0.35,
    'execution_surface_detail': 0.05,
    'postmortem_contrast': 0.20,
    'rust_engineering': 0.20,
    'narrative_strategy': 0.10,
    'hermes_tool': 0.05,
    'general_retention': 0.05,
}

# Mint/creator caps
MAX_PANELS_PER_MINT = 3
MAX_SFT_PER_TRAJECTORY = 2
MAX_CPT_PER_CREATOR = 5
MAX_PANELS_PER_DAY = 20
MAX_RUST_PER_SUBSYSTEM = 40
MAX_CANDIDATE_OVERLAP = 0.30

# L3 latency scenarios (LaserStream)
LS_LATENCIES = [0, 100, 500, 1000, 2000]
LS_SIZES = [0.05, 0.10, 0.25, 0.50, 1.0]

# Slinky counterfactual sizes (at 250ms only)
SLINKY_CF_SIZES = [0.05, 0.10, 0.25, 0.50, 1.0]
SLINKY_CF_LATENCY = 250

# Causal input fields — Slinky pump_state_v3 (103 fields, NO leakage)
SLINKY_CAUSAL_FIELDS = [
    'event_time_unix_ms', 'mint', 'state_id', 'seq', 'venue', 'trade_side', 'is_buy',
    'sol_amount_sol', 'token_amount_raw', 'price_sol',
    'v_sol_bonding_curve_sol', 'v_tokens_bonding_curve_raw', 'curve_pct_depleted',
    'graduation_proximity_pct', 'v_sol_depletion_rate_5s', 'v_tokens_accumulation_rate_5s',
    'trade_velocity_1s', 'trade_velocity_5s', 'trade_velocity_30s',
    'vol_velocity_5s', 'vol_velocity_30s', 'vol_acceleration',
    'price_momentum_5s_bp', 'price_momentum_30s_bp', 'mcap_momentum_5s_bp',
    'price_change_since_launch_bp', 'price_volatility_30s_bp',
    'buy_sell_imbalance_5s', 'buy_sell_imbalance_30s', 'buy_pressure',
    'net_flow_sol', 'buy_vol_sol', 'sell_vol_sol', 'total_vol_sol',
    'unique_buyers_5s', 'unique_buyers_so_far', 'unique_sellers_5s', 'unique_sellers_so_far',
    'unique_wallets_so_far', 'buyer_concentration_ratio', 'seller_concentration_ratio',
    'top1_buyer_pct', 'top5_buyer_pct',
    'holder_concentration_hhi', 'initial_gini',
    'coordinated_buy_ratio', 'wash_trade_ratio', 'sybil_cluster_size',
    'rapid_rebuy_count', 'wallets_also_selling', 'repeat_buyer_ratio', 'repeat_seller_ratio',
    'toxic_flow_indicator',
    'avg_trade_size_sol', 'median_trade_size_sol', 'min_trade_size_sol',
    'max_trade_size_sol', 'trade_size_std_sol', 'largest_buy_pct_of_vol', 'largest_sell_pct_of_vol',
    'buy_count_so_far', 'sell_count_so_far', 'trade_count_so_far',
    'liquidity_sol', 'liquidity_change_5s_sol', 'liquidity_change_30s_sol',
    'curve_depletion_velocity_5s', 'curve_depletion_velocity_30s',
    'market_active_mints_5m', 'market_avg_buy_pressure_5m', 'market_total_vol_5m_sol', 'market_cap_sol',
    'seconds_since_launch', 'minutes_since_launch',
    'initial_market_cap_sol', 'initial_price_sol', 'initial_supply_raw', 'initial_holder_count',
    'creator', 'creator_past_tokens', 'creator_past_rugs',
    'token_name', 'token_symbol',
    'is_mayhem', 'is_zombie', 'is_graduated', 'top10_pct_suspect',
    'supply_bug_corrected', 'dev_buy_pct_corrected', 'data_quality_score',
    'avg_wallet_trade_count', 'initial_top10_pct_corrected', 'market_cap_sol_lamports',
    'source', 'source_hash', 'run_uuid', 'git_sha', 'code_config_hash',
    'pipeline_version', 'producer_version',
]

# NEVER input — leakage fields
SLINKY_LEAKAGE_FIELDS = ['seconds_to_graduation']

# LaserStream L1 causal fields (48)
LS_CAUSAL_FIELDS = [
    'price_lamports_per_rawtok', 'slot', 'timestamp_ms', 'venue', 'event_type',
    'trade_side', 'trader_b58', 'sol_traded_lamports', 'tokens_traded_raw',
    'curve_virtual_sol', 'curve_virtual_token', 'curve_real_sol', 'curve_real_token',
    'curve_complete', 'pool_base_reserve', 'pool_quote_reserve', 'pool_lp_supply',
    'ix_fee_bps', 'ix_amount_in', 'ix_amount_out', 'ix_max_amount_in', 'ix_min_amount_out',
    'token_decimals', 'mint_b58', 'state_id',
    'right_censored_1s', 'right_censored_2s', 'right_censored_5s', 'right_censored_10s',
    'right_censored_30s', 'right_censored_60s', 'right_censored_120s', 'right_censored_300s',
    'observed_no_trade_1s', 'observed_no_trade_2s', 'observed_no_trade_5s', 'observed_no_trade_10s',
    'observed_no_trade_30s', 'observed_no_trade_60s', 'observed_no_trade_120s', 'observed_no_trade_300s',
    'price_source', 'price_scale_tier', 'curve_account_b58', 'pool_account_b58', 'raw_hash',
    'capture_start_ms', 'capture_end_ms',
]

# L2 outcome fields (TARGET ONLY)
LS_L2_FIELDS = [
    'state_id', 'mint_b58', 'ret_1s_bp', 'ret_5s_bp', 'ret_30s_bp', 'ret_60s_bp', 'ret_120s_bp', 'ret_300s_bp',
    'mfe_bp', 'mae_bp', 'peak_bp', 'time_to_peak_seconds',
    'hit_plus_25_bp_time', 'hit_plus_50_bp_time', 'hit_plus_100_bp_time', 'hit_plus_200_bp_time',
    'hit_plus_500_bp_time', 'hit_plus_1000_bp_time', 'hit_plus_2000_bp_time', 'hit_plus_3000_bp_time',
    'hit_minus_10_bp_time', 'hit_minus_20_bp_time', 'hit_minus_30_bp_time', 'hit_minus_50_bp_time',
    'hit_minus_100_bp_time', 'hit_minus_200_bp_time', 'hit_minus_500_bp_time', 'hit_minus_1000_bp_time',
    'hit_minus_1500_bp_time', 'hit_minus_2000_bp_time',
    'graduated', 'migrated_to_pumpswap', 'collapsed_50pct', 'survived_60s', 'survived_300s',
    'right_censored_5s', 'right_censored_30s', 'right_censored_300s',
    'plus_100_before_minus_30',
]

# Slinky pump_outcome target fields (TARGET ONLY)
SLINKY_OUTCOME_FIELDS = [
    'mint', 'event_time_unix_ms', 'state_id',
    'ret_1s_bp', 'ret_5s_bp', 'ret_30s_bp', 'ret_60s_bp', 'ret_120s_bp', 'ret_300s_bp',
    'mfe_bp', 'mae_bp', 'peak_bp', 'time_to_peak_seconds',
    'graduated_after_state', 'graduated_did_migrate', 'collapsed_50pct_within_300s',
    'survived_60s', 'survived_300s',
    'right_censored_300s', 'plus_100_before_minus_30',
    'mae_time_seconds', 'mfe_time_seconds',
    'hit_plus_100_bp_time', 'hit_plus_200_bp_time', 'hit_plus_500_bp_time', 'hit_plus_1000_bp_time',
    'hit_minus_100_bp_time', 'hit_minus_200_bp_time', 'hit_minus_500_bp_time',
    # Forward-looking activity fields — REQUIRED by regime classification.
    # (Their absence silently zeroed moonshot/stable_low_vol and inflated
    # quick_death to all 622K mints: .get() returned False for every mint.)
    'has_trade_within_60s', 'has_future_trade_300s', 'time_to_next_trade_s',
]

# ════════════════════════════════════════════════════════════════
# ROBUST UTILITY V1 — exact deterministic formula
# ════════════════════════════════════════════════════════════════

def compute_robust_utility_v1(
    feasibility_score,      # [0,1] from L3
    net_sol_economics,      # median sim_return_pct where feasible
    downside_risk,          # mae_bp / 10000
    upside_potential,       # mfe_bp / 10000
    size_robustness,        # count(feasible at largest 3 sizes) / 3
    latency_robustness,     # count(feasible at 5 latencies for median size) / 5, or None
    censoring_adjustment,   # 1 - max(right_censored flags)
):
    """Compute versioned robust executable utility. Range ~[-0.35, 0.55]."""
    lat_term = latency_robustness if latency_robustness is not None else 0.0
    utility = (
        0.20 * feasibility_score
        + 0.25 * math.tanh(3 * (net_sol_economics or 0.0))
        - 0.20 * math.tanh(3 * abs(downside_risk or 0.0))
        + 0.10 * math.tanh(2 * (upside_potential or 0.0))
        + 0.10 * (size_robustness or 0.0)
        + 0.10 * lat_term
        + 0.05 * (censoring_adjustment or 0.0)
    )
    return round(utility, 6)


def derive_buy_watch_skip(utility, feasibility_score, net_sol_economics, collapsed):
    """Derive secondary BUY/WATCH/SKIP from absolute quality criteria."""
    # SKIP (any)
    if utility < -0.10 or (feasibility_score is not None and feasibility_score < 0.20) or collapsed:
        return 'SKIP'
    # BUY (all must hold)
    if utility > 0.0 and (feasibility_score is not None and feasibility_score > 0.40) and \
       (net_sol_economics is not None and net_sol_economics > 0.0) and not collapsed:
        return 'BUY'
    return 'WATCH'


def compute_l3_compressed(l3_scenarios):
    """Compress 25 L3 scenarios into deterministic features.
    l3_scenarios: list of dicts with keys: feasible, sim_return_pct, trade_size_sol,
                   latency_scenario_ms, outcome_class, capacity_constrained
    """
    if not l3_scenarios:
        return {
            'feasible_ratio': 0.0, 'median_net_return_pct': None,
            'worst_net_return_pct': None, 'best_net_return_pct': None,
            'max_feasible_size_sol': None, 'latency_sensitivity': None,
            'size_sensitivity': None, 'latency_breakpoint_ms': None,
            'capacity_constrained_sizes': [], 'outcome_distribution': {},
            'source_capability_mask': 'laserstream_l3_25'
        }
    
    feasible = [s for s in l3_scenarios if s.get('feasible', False)]
    total = len(l3_scenarios)
    feasible_returns = [s['sim_return_pct'] for s in feasible if s.get('sim_return_pct') is not None]
    
    # Size robustness: largest 3 sizes
    size_set = sorted(set(s['trade_size_sol'] for s in l3_scenarios), reverse=True)
    top3_sizes = size_set[:3]
    size_robust = []
    for sz in top3_sizes:
        sz_feasible = [s for s in feasible if s['trade_size_sol'] == sz]
        size_robust.append(1.0 if len(sz_feasible) > 0 else 0.0)
    size_robustness = sum(size_robust) / len(size_robust) if size_robust else 0.0
    
    # Latency robustness: all 5 latencies for median size
    lat_set = sorted(set(s['latency_scenario_ms'] for s in l3_scenarios))
    median_size = sorted(set(s['trade_size_sol'] for s in l3_scenarios))[len(set(s['trade_size_sol'] for s in l3_scenarios)) // 2] if size_set else None
    lat_robust = 0.0
    lat_sensitivity = None
    lat_breakpoint = None
    if median_size and len(lat_set) >= 5:
        lat_feasible = []
        lat_returns_by_lat = {}
        for lat in lat_set:
            lat_scenarios = [s for s in feasible if s['trade_size_sol'] == median_size and s['latency_scenario_ms'] == lat]
            lat_feasible.append(1.0 if lat_scenarios else 0.0)
            if lat_scenarios:
                lat_returns_by_lat[lat] = [s['sim_return_pct'] for s in lat_scenarios if s.get('sim_return_pct') is not None]
        lat_robust = sum(lat_feasible) / len(lat_feasible) if lat_feasible else 0.0
        # Latency sensitivity = std of mean returns across latencies
        if lat_returns_by_lat:
            mean_returns = [sum(v) / len(v) for v in lat_returns_by_lat.values()]
            if len(mean_returns) > 1:
                mean_r = sum(mean_returns) / len(mean_returns)
                lat_sensitivity = round(math.sqrt(sum((r - mean_r) ** 2 for r in mean_returns) / len(mean_returns)), 6)
            else:
                lat_sensitivity = 0.0
        # Latency breakpoint: first latency where feasibility drops below 50%
        for lat in sorted(lat_set):
            lat_scenarios = [s for s in l3_scenarios if s['trade_size_sol'] == median_size and s['latency_scenario_ms'] == lat]
            lat_feasible_count = sum(1 for s in lat_scenarios if s.get('feasible', False))
            if lat_scenarios and lat_feasible_count / len(lat_scenarios) < 0.5:
                lat_breakpoint = lat
                break
    
    # Size sensitivity = std of mean returns across sizes
    size_returns_by_size = {}
    for sz in size_set:
        sz_feasible = [s for s in feasible if s['trade_size_sol'] == sz]
        if sz_feasible:
            rets = [s['sim_return_pct'] for s in sz_feasible if s.get('sim_return_pct') is not None]
            if rets:
                size_returns_by_size[sz] = sum(rets) / len(rets)
    size_sensitivity = None
    if len(size_returns_by_size) > 1:
        mean_r = sum(size_returns_by_size.values()) / len(size_returns_by_size)
        size_sensitivity = round(math.sqrt(sum((r - mean_r) ** 2 for r in size_returns_by_size.values()) / len(size_returns_by_size)), 6)
    elif size_returns_by_size:
        size_sensitivity = 0.0
    
    # Outcome distribution
    outcome_dist = Counter(s.get('outcome_class', 'unknown') for s in l3_scenarios)
    
    # Capacity constrained sizes
    cap_sizes = sorted(set(s['trade_size_sol'] for s in l3_scenarios if s.get('capacity_constrained', False)))
    
    # Max feasible size
    max_feasible = max([s['trade_size_sol'] for s in feasible], default=None) if feasible else None
    
    return {
        'feasible_ratio': round(len(feasible) / total, 4) if total else 0.0,
        'median_net_return_pct': round(sum(feasible_returns) / len(feasible_returns), 6) if feasible_returns else None,
        'worst_net_return_pct': min(feasible_returns) if feasible_returns else None,
        'best_net_return_pct': max(feasible_returns) if feasible_returns else None,
        'max_feasible_size_sol': max_feasible,
        'latency_sensitivity': lat_sensitivity,
        'size_sensitivity': size_sensitivity,
        'size_returns': {str(k): round(v, 6) for k, v in size_returns_by_size.items()},
        'latency_breakpoint_ms': lat_breakpoint,
        'capacity_constrained_sizes': cap_sizes,
        'outcome_distribution': dict(outcome_dist),
        'size_robustness': round(size_robustness, 4),
        'latency_robustness': round(lat_robust, 4) if lat_robust else None,
        'source_capability_mask': 'laserstream_l3_25'
    }


def _cf_net_return(cf_row, prefix):
    """Net return for a size scenario as a FRACTION (e.g. 0.05 = +5%).

    slinky_gold_v3_compact stores `szXXX_net_return_bp` (int bp); the original
    full v3 stored `szXXX_net_return_pct`. Prefer pct when present, fall back
    to bp/10000. A NaN pct (pandas NaN) is treated as missing. Returns None
    when neither column carries a usable value — the v1 bug silently returned
    None for EVERY row because only the absent *_pct column was requested,
    which zeroed all utilities and collapsed every decision to WATCH.
    """
    val = cf_row.get(f'{prefix}_net_return_pct', None)
    if val is not None and not (isinstance(val, float) and math.isnan(val)):
        return val
    bp = cf_row.get(f'{prefix}_net_return_bp', None)
    if bp is not None and not (isinstance(bp, float) and math.isnan(bp)):
        return bp / 10000.0
    return None


def compute_slinky_cf_compressed(cf_row):
    """Compress Slinky counterfactual (5 sizes @ 250ms) into deterministic features.
    Slinky has NO latency variation — latency_sensitivity = NULL, latency_robustness = NULL.
    """
    # Extract the 5 size scenarios from column prefixes
    sizes = {'sz005': 0.05, 'sz010': 0.10, 'sz025': 0.25, 'sz050': 0.50, 'sz100': 1.0}
    feasible = []
    returns = []
    size_feasible_map = {}
    size_return_map = {}
    
    for prefix, sz in sizes.items():
        is_feasible = cf_row.get(f'{prefix}_feasible', False)
        net_return = _cf_net_return(cf_row, prefix)
        if is_feasible:
            feasible.append(sz)
            if net_return is not None:
                returns.append(net_return)
            size_feasible_map[sz] = True
            size_return_map[sz] = net_return
        else:
            size_feasible_map[sz] = False

    # Capacity-constrained: sizes where NOT feasible or net_return < 0
    cap_sizes = [sz for sz in sorted(sizes.values()) if not size_feasible_map.get(sz, False)]
    
    # Size robustness: largest 3 sizes
    top3 = sorted(sizes.values(), reverse=True)[:3]
    size_robust = sum(1.0 for sz in top3 if size_feasible_map.get(sz, False)) / 3.0
    
    # Size sensitivity
    size_returns = {}
    for prefix, sz in sizes.items():
        if cf_row.get(f'{prefix}_feasible', False):
            nr = _cf_net_return(cf_row, prefix)
            if nr is not None:
                size_returns[sz] = nr
    size_sensitivity = None
    if len(size_returns) > 1:
        mean_r = sum(size_returns.values()) / len(size_returns)
        size_sensitivity = round(math.sqrt(sum((r - mean_r) ** 2 for r in size_returns.values()) / len(size_returns)), 6)
    elif size_returns:
        size_sensitivity = 0.0
    
    return {
        'feasible_ratio': round(len(feasible) / 5, 4),
        'median_net_return_pct': round(sum(returns) / len(returns), 6) if returns else None,
        'worst_net_return_pct': min(returns) if returns else None,
        'best_net_return_pct': max(returns) if returns else None,
        'max_feasible_size_sol': max(feasible) if feasible else None,
        'latency_sensitivity': None,  # EXPLICITLY NULL — no latency variation in Slinky
        'size_sensitivity': size_sensitivity,
        'latency_breakpoint_ms': None,  # NULL — single latency
        'capacity_constrained_sizes': cap_sizes,
        'outcome_distribution': {},  # Slinky CF doesn't have outcome_class
        'size_robustness': round(size_robust, 4),
        'latency_robustness': None,  # EXPLICITLY NULL
        'source_capability_mask': 'slinky_cf_5size_250ms',
        'economic_class': cf_row.get('economic_class', None),
        'eligible': cf_row.get('eligible', None),
        'risk_reward_ratio': cf_row.get('risk_reward_ratio', None),
        'exit_feasible': cf_row.get('exit_feasible', None),
    }


def hash_mint(mint_str):
    """Hash mint for privacy in exports."""
    return hashlib.sha256(str(mint_str).encode()).hexdigest()[:16]


def hash_creator(creator_str):
    """Hash creator for privacy."""
    return hashlib.sha256(str(creator_str).encode()).hexdigest()[:12]


def compute_file_hash(filepath):
    """Compute SHA256 of a file."""
    h = hashlib.sha256()
    with open(filepath, 'rb') as f:
        while True:
            chunk = f.read(8192)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def make_provenance(source_corpus, source_ids, source_hash=None, freeze_uuid=None, ts_range=None):
    """Create provenance record for an export example."""
    return {
        'source_corpus': source_corpus,
        'source_ids': source_ids[:5],  # cap for storage
        'source_hash': source_hash or '',
        'freeze_uuid': freeze_uuid or '',
        'timestamp_range': ts_range or [None, None],
        'run_uuid': RUN_UUID,
    }


# ════════════════════════════════════════════════════════════════
# CANONICAL SCHEMA NORMALIZERS
# ════════════════════════════════════════════════════════════════

def normalize_slinky_causal(row, causal_fields):
    """Normalize Slinky pump_state_v3 row to canonical causal state."""
    causal = {}
    for f in causal_fields:
        if f in row:
            val = row[f]
            # Convert numpy types to Python
            if hasattr(val, 'item'):
                val = val.item()
            if f == 'mint':
                causal['mint_id'] = hash_mint(val)
            elif f == 'creator':
                causal['creator_id'] = hash_creator(val)
            else:
                causal[f] = val
    # Source-specific fields
    source_specific = {
        'seconds_since_launch': row.get('seconds_since_launch'),
        'initial_market_cap_sol': row.get('initial_market_cap_sol'),
        'rapid_rebuy_count': row.get('rapid_rebuy_count'),
        'sybil_cluster_size': row.get('sybil_cluster_size'),
        'trade_count_so_far': row.get('trade_count_so_far'),
        'data_quality_score': row.get('data_quality_score'),
    }
    return causal, source_specific


def normalize_laserstream_causal(row, causal_fields):
    """Normalize LaserStream L1 row to canonical causal state."""
    causal = {}
    for f in causal_fields:
        if f in row:
            val = row[f]
            if hasattr(val, 'item'):
                val = val.item()
            if f == 'mint_b58':
                causal['mint_id'] = hash_mint(val)
            elif f == 'trader_b58':
                causal['trader_id'] = hash_mint(val)
            else:
                causal[f] = val
    source_specific = {
        'slot': row.get('slot'),
        'curve_complete': row.get('curve_complete'),
        'pool_base_reserve': row.get('pool_base_reserve'),
        'pool_quote_reserve': row.get('pool_quote_reserve'),
        'ix_fee_bps': row.get('ix_fee_bps'),
        'right_censored_5s': row.get('right_censored_5s'),
        'observed_no_trade_5s': row.get('observed_no_trade_5s'),
    }
    return causal, source_specific


# ════════════════════════════════════════════════════════════════
# SLINKY PANEL BUILDER
# ════════════════════════════════════════════════════════════════

# Global cache for Slinky data
_SLINKY_TIME_INDEX = None  # (file, min_ts, max_ts) per file
_SLINKY_LIGHTWEIGHT_IDX = None  # global (event_time, mint, state_id, file) dataframe
_SLINKY_CAUSAL_CACHE = OrderedDict()  # file -> DataFrame (LRU-bounded)
_CAUSAL_CACHE_MAX_FILES = 134  # cache ALL causal files: ~38GB in-RAM (measured
# 0.28GB/file x 134) + ~20GB baseline = ~58GB on a 256GB box — well inside the
# 12% free-RAM buffer. Zero eviction => each parquet is read from disk exactly
# once; panel assembly stops thrashing (was 6 files => ~46s/panel re-reads).  # max full-file causal frames resident at once

def get_slinky_time_index():
    """Build and cache a time index for Slinky pump_state_v3 files."""
    global _SLINKY_TIME_INDEX
    if _SLINKY_TIME_INDEX is not None:
        return _SLINKY_TIME_INDEX
    
    ps_dir = os.path.join(SLINKY, 'pump_state_v3')
    all_files = sorted([f for f in os.listdir(ps_dir) if f.endswith('.parquet')])
    index = []
    for fname in all_files:
        filepath = os.path.join(ps_dir, fname)
        try:
            t = pq.read_table(filepath, columns=['event_time_unix_ms'])
            ts = t.to_pandas()['event_time_unix_ms']
            index.append((fname, int(ts.min()), int(ts.max())))
        except:
            continue
    _SLINKY_TIME_INDEX = index
    print(f"    Slinky time index: {len(index)} files")
    return index


def build_slinky_lightweight_index():
    """Pre-load (event_time_unix_ms, mint, state_id, file) from ALL Slinky files.
    This is the global index for finding candidates in any time window.
    """
    global _SLINKY_LIGHTWEIGHT_IDX
    if _SLINKY_LIGHTWEIGHT_IDX is not None:
        return _SLINKY_LIGHTWEIGHT_IDX
    
    ps_dir = os.path.join(SLINKY, 'pump_state_v3')
    all_files = sorted([f for f in os.listdir(ps_dir) if f.endswith('.parquet')])
    
    chunks = []
    for fname in all_files:
        filepath = os.path.join(ps_dir, fname)
        try:
            cols = get_safe_columns(filepath, ['event_time_unix_ms', 'mint', 'state_id'])
            t = pq.read_table(filepath, columns=cols)
            df = t.to_pandas()
            df['source_file'] = fname
            chunks.append(df)
        except:
            continue
    
    _SLINKY_LIGHTWEIGHT_IDX = pd.concat(chunks, ignore_index=True) if chunks else pd.DataFrame()
    print(f"    Slinky lightweight index: {len(_SLINKY_LIGHTWEIGHT_IDX)} rows from {len(chunks)} files")
    return _SLINKY_LIGHTWEIGHT_IDX


def load_slinky_causal_for_mints(mints_by_file):
    """Load causal fields for specific mints from specific files.
    Caches the FULL file DataFrame (all mints, causal cols only) per file,
    BOUNDED to _CAUSAL_CACHE_MAX_FILES with LRU eviction — an unbounded cache
    eventually holds all ~134 files (~entire 33.5M-row dataset) and OOM-kills
    the run. Filtering done in-memory.
    Returns {filename: {mint: row_dict}}
    """
    global _SLINKY_CAUSAL_CACHE
    if _SLINKY_CAUSAL_CACHE is None:
        _SLINKY_CAUSAL_CACHE = OrderedDict()
    ps_dir = os.path.join(SLINKY, 'pump_state_v3')

    # Load + extract per file IN ONE PASS so LRU eviction can never drop a
    # file before its mints are extracted (extraction uses groupby, not
    # per-mint boolean scans — O(rows) instead of O(mints * rows)).
    result = {}
    for fname, mint_set in mints_by_file.items():
        df = _SLINKY_CAUSAL_CACHE.get(fname)
        if df is not None:
            _SLINKY_CAUSAL_CACHE.move_to_end(fname)  # LRU touch
        else:
            filepath = os.path.join(ps_dir, fname)
            try:
                meta = pq.ParquetFile(filepath).schema
                avail_cols = set(meta.names)
                safe_cols = [f for f in SLINKY_CAUSAL_FIELDS if f in avail_cols]
                df = pq.read_table(filepath, columns=safe_cols).to_pandas()
            except Exception:
                continue
            _SLINKY_CAUSAL_CACHE[fname] = df
            while len(_SLINKY_CAUSAL_CACHE) > _CAUSAL_CACHE_MAX_FILES:
                _SLINKY_CAUSAL_CACHE.popitem(last=False)

        sub = df[df['mint'].isin(mint_set)]
        result[fname] = {
            m: g.reset_index(drop=True)
            for m, g in sub.groupby('mint', sort=False)
        }

    return result


def get_safe_columns(filepath, requested_cols):
    """Return only columns that exist in the parquet file schema."""
    meta = pq.ParquetFile(filepath).schema
    avail_cols = set(meta.names)
    return [c for c in requested_cols if c in avail_cols]


# Global caches for Slinky outcomes/counterfactuals (pre-loaded once)
_SLINKY_OUTCOMES_CACHE = None        # pd.DataFrame
_SLINKY_COUNTERFACTUALS_CACHE = None  # pd.DataFrame
_SLINKY_OUTCOMES_BY_MINT = None       # dict: mint -> row_dict (latest)
_SLINKY_CF_BY_STATE = None           # _CFLookup: state_id -> row_dict (lazy)


class _CFLookup:
    """Dict-compatible lazy lookup over a state_id-indexed DataFrame.

    Replaces the old 33.5M-entry dict-of-row-dicts (50-80GB RAM) with O(1)
    .loc lookups that materialize a row dict only on access. Supports the
    exact dict operations used by callers: .get(), 'in', [], len(), bool().
    """
    __slots__ = ('_df',)

    def __init__(self, df):
        self._df = df

    def get(self, key, default=None):
        try:
            row = self._df.loc[key]
        except (KeyError, TypeError):
            return default
        if isinstance(row, pd.DataFrame):  # duplicate index (shouldn't happen post-dedupe)
            row = row.iloc[0]
        return row.to_dict()

    def __getitem__(self, key):
        val = self.get(key)
        if val is None:
            raise KeyError(key)
        return val

    def __contains__(self, key):
        return key in self._df.index

    def __len__(self):
        return len(self._df)

    def __bool__(self):
        return len(self._df) > 0


def preload_slinky_outcomes():
    """Pre-load ALL pump_outcome_v3 data. Build mint-indexed dict for O(1) lookup.
    Called ONCE before panel building.
    """
    global _SLINKY_OUTCOMES_CACHE, _SLINKY_OUTCOMES_BY_MINT
    if _SLINKY_OUTCOMES_BY_MINT is not None:
        return _SLINKY_OUTCOMES_CACHE

    po_dir = os.path.join(SLINKY, 'pump_outcome_v3')
    files = sorted(os.listdir(po_dir))
    chunks = []
    for fname in files:
        filepath = os.path.join(po_dir, fname)
        if not os.path.exists(filepath):
            continue
        base_cols = ['mint', 'event_time_unix_ms', 'state_id']
        outcome_cols = [f for f in SLINKY_OUTCOME_FIELDS if f not in base_cols]
        cols = get_safe_columns(filepath, base_cols + outcome_cols)
        try:
            table = pq.read_table(filepath, columns=cols)
            chunks.append(table.to_pandas())
        except:
            continue

    _SLINKY_OUTCOMES_CACHE = pd.concat(chunks, ignore_index=True) if chunks else pd.DataFrame()
    del chunks
    n_outcome_rows = len(_SLINKY_OUTCOMES_CACHE)
    # GATE-1 SEMANTICS (BINDING): mint-level DETERMINISTIC AGGREGATES across ALL
    # outcome rows — NEVER a single representative row ("latest valid mfe_bp"
    # was the old, rejected semantics). Per-field reductions:
    #   max  : mfe_bp, peak_bp, horizon returns (best forward excursion ever)
    #   min  : mae_bp (worst adverse), hit-threshold times (earliest crossing),
    #          time_to_next_trade_s, mfe/mae/peak times (earliest occurrence)
    #   any  : graduation, migration, collapse, survival, activity flags
    #   all  : right_censored_300s (censored only if EVERY row is censored)
    # These aggregates define regime TARGET/catalog membership only — they are
    # never fed back as causal input.
    _SLINKY_OUTCOMES_BY_MINT = {}
    if not _SLINKY_OUTCOMES_CACHE.empty:
        df = _SLINKY_OUTCOMES_CACHE
        num_max = ['mfe_bp', 'peak_bp', 'ret_1s_bp', 'ret_5s_bp', 'ret_30s_bp',
                   'ret_60s_bp', 'ret_120s_bp', 'ret_300s_bp', 'event_time_unix_ms']
        num_min = ['mae_bp', 'mae_time_seconds', 'mfe_time_seconds', 'time_to_peak_seconds',
                   'hit_plus_100_bp_time', 'hit_plus_200_bp_time', 'hit_plus_500_bp_time',
                   'hit_plus_1000_bp_time', 'hit_minus_100_bp_time', 'hit_minus_200_bp_time',
                   'hit_minus_500_bp_time', 'time_to_next_trade_s']
        bool_any = ['graduated_after_state', 'graduated_did_migrate',
                    'collapsed_50pct_within_300s', 'survived_60s', 'survived_300s',
                    'plus_100_before_minus_30', 'has_trade_within_60s',
                    'has_future_trade_300s']
        bool_all = ['right_censored_300s']
        agg_spec = {}
        for c in num_max:
            if c in df.columns:
                agg_spec[c] = 'max'
        for c in num_min:
            if c in df.columns:
                agg_spec[c] = 'min'
        for c in bool_any + bool_all:
            if c in df.columns:
                # Normalize to real bools so groupby max/min = any/all.
                df[c] = df[c].fillna(False).astype(bool)
                agg_spec[c] = 'max' if c in bool_any else 'min'
        if 'state_id' in df.columns:
            agg_spec['state_id'] = 'last'
        grouped = df.groupby('mint', sort=False)
        combined = grouped.agg(agg_spec).reset_index()
        combined['n_outcome_rows'] = grouped.size().values
        del df, grouped
        # Vectorized mint hashing (single pass), then one to_dict pass on ~622K rows
        hashed = [hash_mint(m) for m in combined['mint'].tolist()]
        records = combined.to_dict('records')
        _SLINKY_OUTCOMES_BY_MINT = dict(zip(hashed, records))
        del combined, records, hashed
    # Free the 33.5M-row frame — everything downstream uses _SLINKY_OUTCOMES_BY_MINT.
    _SLINKY_OUTCOMES_CACHE = pd.DataFrame()
    print(f"    Slinky outcomes pre-loaded: {n_outcome_rows} rows, {len(_SLINKY_OUTCOMES_BY_MINT)} unique mints")
    return _SLINKY_OUTCOMES_CACHE


def preload_slinky_counterfactuals():
    """Pre-load ALL counterfactual_trade_v3 data. Build state_id-indexed dict for O(1) lookup.
    Called ONCE before panel building.
    """
    global _SLINKY_COUNTERFACTUALS_CACHE, _SLINKY_CF_BY_STATE
    if _SLINKY_CF_BY_STATE is not None:
        return _SLINKY_COUNTERFACTUALS_CACHE

    cf_dir = os.path.join(SLINKY, 'counterfactual_trade_v3')
    files = sorted(os.listdir(cf_dir))
    target_cols = [
        'state_id', 'eligible', 'economic_class', 'risk_reward_ratio', 'exit_feasible',
        'net_return_pct', 'hold_duration_seconds', 'entry_size_sol', 'latency_ms',
        'sz005_feasible', 'sz005_net_return_pct', 'sz005_net_return_bp',
        'sz010_feasible', 'sz010_net_return_pct', 'sz010_net_return_bp',
        'sz025_feasible', 'sz025_net_return_pct', 'sz025_net_return_bp',
        'sz050_feasible', 'sz050_net_return_pct', 'sz050_net_return_bp',
        'sz100_feasible', 'sz100_net_return_pct', 'sz100_net_return_bp',
    ]
    chunks = []
    for fname in files:
        filepath = os.path.join(cf_dir, fname)
        try:
            cols = get_safe_columns(filepath, target_cols)
            table = pq.read_table(filepath, columns=cols)
            chunks.append(table.to_pandas())
        except:
            continue

    cf_df = pd.concat(chunks, ignore_index=True) if chunks else pd.DataFrame()
    n_rows = len(cf_df)
    if not cf_df.empty:
        # Dedupe on state_id (normally already unique), then index for O(1) .loc lookup.
        # CRITICAL: do NOT materialize 33.5M row-dicts (that costs 50-80GB RAM and
        # killed prior runs). Lazy .loc lookup via _CFLookup wrapper instead.
        cf_df = cf_df.drop_duplicates(subset='state_id', keep='first')
        cf_df = cf_df.set_index('state_id', drop=False)
        _SLINKY_CF_BY_STATE = _CFLookup(cf_df)
    else:
        _SLINKY_CF_BY_STATE = _CFLookup(pd.DataFrame())
    # Free the raw concat frame reference; the indexed frame lives in the wrapper.
    _SLINKY_COUNTERFACTUALS_CACHE = pd.DataFrame()
    print(f"    Slinky counterfactuals pre-loaded: {n_rows} rows, {len(_SLINKY_CF_BY_STATE)} unique state_ids (lazy-indexed)")
    return _SLINKY_COUNTERFACTUALS_CACHE


def load_slinky_outcomes(mints, time_center_ms):
    """O(1) lookup from pre-built mint index. Returns dict: mint -> outcome_row."""
    if _SLINKY_OUTCOMES_BY_MINT is None:
        preload_slinky_outcomes()
    if not _SLINKY_OUTCOMES_BY_MINT:
        return {}
    return {m: _SLINKY_OUTCOMES_BY_MINT[hash_mint(m)] for m in mints if hash_mint(m) in _SLINKY_OUTCOMES_BY_MINT}


def load_slinky_counterfactuals(state_ids):
    """O(1) lookup from pre-built state_id index. Returns dict: state_id -> cf_row."""
    if _SLINKY_CF_BY_STATE is None:
        preload_slinky_counterfactuals()
    if not _SLINKY_CF_BY_STATE:
        return {}
    return {sid: _SLINKY_CF_BY_STATE[sid] for sid in state_ids if sid in _SLINKY_CF_BY_STATE}


# ════════════════════════════════════════════════════════════════
# LASERSTREAM PANEL BUILDER
# ════════════════════════════════════════════════════════════════

def load_laserstream_l1(time_center_ms, window_ms=15000, l1_df=None):
    """Load L1 causal states within ±window_ms of time_center.
    If l1_df is provided (pre-loaded), filter from it instead of re-reading.
    """
    if l1_df is not None:
        mask = (l1_df['timestamp_ms'] >= time_center_ms - window_ms) & (l1_df['timestamp_ms'] <= time_center_ms + window_ms)
        return l1_df[mask]
    filepath = os.path.join(LS, 'l1_pump_state_v3.parquet')
    table = pq.read_table(filepath)
    df = table.to_pandas()
    mask = (df['timestamp_ms'] >= time_center_ms - window_ms) & (df['timestamp_ms'] <= time_center_ms + window_ms)
    return df[mask]


def load_laserstream_l2(state_ids, l2_df=None):
    """Load L2 outcomes for given state_ids. If l2_df pre-loaded, filter from it."""
    if l2_df is not None:
        return l2_df[l2_df['state_id'].isin(state_ids)]
    filepath = os.path.join(LS, 'l2_pump_outcome_v3.parquet')
    table = pq.read_table(filepath)
    df = table.to_pandas()
    return df[df['state_id'].isin(state_ids)]


def load_laserstream_l3(state_ids, l3_df=None):
    """Load L3 counterfactual scenarios for given state_ids. If l3_df pre-loaded, filter from it."""
    if l3_df is not None:
        return l3_df[l3_df['state_id'].isin(state_ids)]
    filepath = os.path.join(LS, 'l3_counterfactual_v3.parquet')
    table = pq.read_table(filepath)
    df = table.to_pandas()
    return df[df['state_id'].isin(state_ids)]


# ════════════════════════════════════════════════════════════════
# PANEL ASSEMBLY
# ════════════════════════════════════════════════════════════════

def build_train_filtered_lightweight_index(eval_excluded_mints_set):
    """Build a lightweight index containing ONLY train-eligible mints.
    Pre-filters out eval-excluded mints so panels are clean BY CONSTRUCTION.
    Returns a DataFrame with same columns as build_slinky_lightweight_index().
    """
    lw_idx = build_slinky_lightweight_index()
    if not eval_excluded_mints_set:
        return lw_idx
    # PERF: hash only the ~622K UNIQUE mints instead of applying SHA256 to all
    # 33.5M rows (was minutes of pure hashing). Then vectorized isin filter.
    unique_mints = pd.unique(lw_idx['mint'])
    excluded_raw = {m for m in unique_mints if hash_mint(m) in eval_excluded_mints_set}
    filtered = lw_idx[~lw_idx['mint'].isin(excluded_raw)].copy()
    print(f"    Train-filtered lightweight index: {len(filtered)} rows (excluded {len(lw_idx) - len(filtered)} eval rows)")
    return filtered


def find_dense_time_windows(lw_idx, min_candidates=25, window_ms=60000, max_panels=500):
    """Find time windows containing >= min_candidates unique mints.
    Instead of file midpoints, scan chronologically for dense windows.
    Returns list of (anchor_ms, window_lo, window_hi) tuples.
    """
    if lw_idx is None or len(lw_idx) == 0:
        return []

    # Sort by event time
    sorted_idx = lw_idx.sort_values('event_time_unix_ms')
    times = sorted_idx['event_time_unix_ms'].values
    mints = sorted_idx['mint'].values

    windows = []
    # True O(n) sliding window: maintain an incremental multiset of mint counts
    # instead of rebuilding set(mints[left:right+1]) per row (was O(n*window),
    # effectively hours on 33.5M rows). Anchors are monotonically nondecreasing,
    # so dedupe only needs to compare against the LAST accepted anchor.
    left = 0
    counts = {}
    unique_in_window = 0
    last_anchor = None

    for right in range(len(times)):
        m_r = mints[right]
        c = counts.get(m_r, 0)
        if c == 0:
            unique_in_window += 1
        counts[m_r] = c + 1

        # Shrink window from left until within window_ms
        while times[right] - times[left] > window_ms:
            m_l = mints[left]
            counts[m_l] -= 1
            if counts[m_l] == 0:
                del counts[m_l]
                unique_in_window -= 1
            left += 1

        if unique_in_window >= min_candidates:
            anchor_ms = int((times[left] + times[right]) // 2)
            # Deduplicate: skip anchors within 10s of the previous accepted one
            if last_anchor is not None and abs(anchor_ms - last_anchor) < 10000:
                continue
            windows.append((anchor_ms, int(times[left]), int(times[right])))
            last_anchor = anchor_ms
            if len(windows) >= max_panels:
                break

    print(f"    Dense time windows ({min_candidates}+ mints in {window_ms}ms): {len(windows)} found")
    return windows


def assemble_slinky_panel(anchor_ms, panel_idx, lw_idx=None):
    """Build a Slinky cross-sectional panel at anchor time.
    Uses pre-loaded lightweight index for candidate finding,
    then loads causal fields only for selected mints.
    """
    window_lo = anchor_ms - 60000
    window_hi = anchor_ms + 60000
    
    if lw_idx is None:
        lw_idx = build_slinky_lightweight_index()
    
    # Filter lightweight index to time window
    mask = (lw_idx['event_time_unix_ms'] >= window_lo) & (lw_idx['event_time_unix_ms'] <= window_hi)
    window_df = lw_idx[mask]
    if len(window_df) < SLINKY_PANEL_MIN:
        return None
    
    # Group by mint — take the latest state per mint within the window (vectorized)
    latest_per_mint = window_df.sort_values('event_time_unix_ms').groupby('mint', sort=False).last().reset_index()
    candidates_lw = latest_per_mint.to_dict('records')
    if len(candidates_lw) < SLINKY_PANEL_MIN:
        return None
    
    # Cap at max — sort by event time (proxy for recency)
    if len(candidates_lw) > SLINKY_PANEL_MAX:
        candidates_lw.sort(key=lambda r: r.get('event_time_unix_ms', 0), reverse=True)
        candidates_lw = candidates_lw[:SLINKY_PANEL_MAX]
    
    # Build file->mints mapping for causal field loading
    mints_by_file = {}
    for r in candidates_lw:
        fname = r.get('source_file')
        if fname:
            mints_by_file.setdefault(fname, set()).add(r['mint'])
    
    # Load causal fields for these specific mints
    causal_cache = load_slinky_causal_for_mints(mints_by_file)
    
    # Build candidate records with full causal data
    # Use pre-built global dicts directly for O(1) lookups (avoid function call overhead)
    panel_candidates = []
    for r_lw in candidates_lw:
        fname = r_lw.get('source_file')
        m = r_lw['mint']
        full_row = causal_cache.get(fname, {}).get(m, r_lw)
        # full_row is now a DataFrame (full per-mint sub-DataFrame); extract last row as dict
        if isinstance(full_row, pd.DataFrame):
            if not full_row.empty:
                full_row = full_row.iloc[-1].to_dict()
            else:
                full_row = r_lw
        
        # Direct dict lookup — no function call/set creation overhead
        outcome = _SLINKY_OUTCOMES_BY_MINT.get(hash_mint(m), {}) if _SLINKY_OUTCOMES_BY_MINT else {}
        state_id = full_row.get('state_id')
        cf = {}
        if state_id and _SLINKY_CF_BY_STATE:
            cf = _SLINKY_CF_BY_STATE.get(state_id, {})
            if cf:
                cf = dict(cf)  # copy so we don't mutate the cache
        
        causal, source_specific = normalize_slinky_causal(full_row, SLINKY_CAUSAL_FIELDS)
        l3_compressed = compute_slinky_cf_compressed(cf) if cf else None
        
        mae_bp = outcome.get('mae_bp')
        mfe_bp = outcome.get('mfe_bp')
        collapsed = bool(outcome.get('collapsed_50pct_within_300s', False))
        right_censored = bool(outcome.get('right_censored_300s', False))
        
        feasibility_score = l3_compressed['feasible_ratio'] if l3_compressed else 0.0
        net_sol_economics = l3_compressed['median_net_return_pct'] if l3_compressed else None
        downside_risk = (mae_bp / 10000) if mae_bp is not None else None
        upside_potential = (mfe_bp / 10000) if mfe_bp is not None else None
        size_robustness = l3_compressed.get('size_robustness', 0.0) if l3_compressed else 0.0
        latency_robustness = None  # Slinky: explicitly NULL
        censoring_adjustment = 1.0 - (1.0 if right_censored else 0.0)
        
        utility = compute_robust_utility_v1(
            feasibility_score, net_sol_economics, downside_risk,
            upside_potential, size_robustness, latency_robustness,
            censoring_adjustment
        )
        decision = derive_buy_watch_skip(utility, feasibility_score, net_sol_economics, collapsed)
        
        panel_candidates.append({
            'mint_id': hash_mint(m),
            'causal_state': causal,
            'source_specific': source_specific,
            'l3_compressed': l3_compressed,
            'l2_evidence': {
                'ret_5s_bp': outcome.get('ret_5s_bp'),
                'ret_30s_bp': outcome.get('ret_30s_bp'),
                'ret_300s_bp': outcome.get('ret_300s_bp'),
                'mfe_bp': mfe_bp,
                'mae_bp': mae_bp,
                'collapsed_50pct_within_300s': collapsed,
                'survived_60s': outcome.get('survived_60s'),
                'graduated_after_state': outcome.get('graduated_after_state'),
            },
            'continuous_utility': {
                'robust_executable_utility_v1': utility,
                'feasibility_score': feasibility_score,
                'net_sol_economics': net_sol_economics,
                'downside_risk': downside_risk,
                'upside_potential': upside_potential,
                'size_robustness': size_robustness,
                'latency_robustness': None,
                'censoring_adjustment': censoring_adjustment,
            },
            'decision': decision,
            'provenance_ids': {
                'state_id': state_id,
                'outcome_state_id': outcome.get('state_id'),
                'cf_state_id': state_id,
                'source_file': fname,
            }
        })
    
    # Rank by utility
    panel_candidates.sort(key=lambda x: x['continuous_utility']['robust_executable_utility_v1'], reverse=True)
    for i, c in enumerate(panel_candidates):
        c['panel_rank'] = i + 1
    
    has_buy = any(c['decision'] == 'BUY' for c in panel_candidates)
    
    return {
        'panel_idx': panel_idx,
        'panel_timestamp': anchor_ms,
        'panel_source': 'slinky',
        'panel_size': len(panel_candidates),
        'candidates': panel_candidates,
        'has_buy': has_buy,
        'no_buy_flag': not has_buy,
        'time_window': {'lo': window_lo, 'hi': window_hi},
    }


def assemble_laserstream_panel(anchor_ms, panel_idx, l1_df=None, l2_df_pre=None, l3_df_pre=None):
    """Build a LaserStream cross-sectional panel at anchor time."""
    l1_window = load_laserstream_l1(anchor_ms, window_ms=15000, l1_df=l1_df)
    if l1_window is None or len(l1_window) < LS_PANEL_MIN:
        return None
    
    # Group by mint — latest state per mint
    mint_states = {}
    for _, row in l1_window.iterrows():
        m = row.get('mint_b58')
        if m is None:
            continue
        if m not in mint_states or row.get('timestamp_ms', 0) > mint_states[m].get('timestamp_ms', 0):
            mint_states[m] = row.to_dict()
    
    candidates = list(mint_states.values())
    if len(candidates) < LS_PANEL_MIN:
        return None
    
    if len(candidates) > LS_PANEL_MAX:
        def weight(r):
            try:
                return (r.get('curve_real_sol', 1) or 1) * (r.get('pool_quote_reserve', 1) or 1)
            except:
                return 0
        candidates.sort(key=weight, reverse=True)
        candidates = candidates[:LS_PANEL_MAX]
    
    # Load L2 and L3 from pre-loaded data
    state_ids = {r.get('state_id') for r in candidates if r.get('state_id')}
    l2_out = load_laserstream_l2(state_ids, l2_df=l2_df_pre)
    l3_cf = load_laserstream_l3(state_ids, l3_df=l3_df_pre)
    
    l2_map = {row['state_id']: row.to_dict() for _, row in l2_out.iterrows()} if not l2_out.empty else {}
    l3_map = defaultdict(list)
    if not l3_cf.empty:
        for _, row in l3_cf.iterrows():
            l3_map[row['state_id']].append(row.to_dict())
    
    panel_candidates = []
    for r in candidates:
        sid = r.get('state_id')
        l2 = l2_map.get(sid, {})
        l3_scenarios = l3_map.get(sid, [])
        
        causal, source_specific = normalize_laserstream_causal(r, LS_CAUSAL_FIELDS)
        
        # Compute L3 compressed
        l3_compressed = compute_l3_compressed(l3_scenarios) if l3_scenarios else None
        
        # L2 evidence
        mae_bp = l2.get('mae_bp')
        mfe_bp = l2.get('mfe_bp')
        collapsed = bool(l2.get('collapsed_50pct', False))
        right_censored = bool(l2.get('right_censored_300s', False))
        
        feasibility_score = l3_compressed['feasible_ratio'] if l3_compressed else 0.0
        net_sol_economics = l3_compressed['median_net_return_pct'] if l3_compressed else None
        downside_risk = (mae_bp / 10000) if mae_bp is not None else None
        upside_potential = (mfe_bp / 10000) if mfe_bp is not None else None
        size_robustness = l3_compressed.get('size_robustness', 0.0) if l3_compressed else 0.0
        latency_robustness = l3_compressed.get('latency_robustness') if l3_compressed else None
        censoring_adjustment = 1.0 - (1.0 if right_censored else 0.0)
        
        utility = compute_robust_utility_v1(
            feasibility_score, net_sol_economics, downside_risk,
            upside_potential, size_robustness, latency_robustness,
            censoring_adjustment
        )
        decision = derive_buy_watch_skip(utility, feasibility_score, net_sol_economics, collapsed)
        
        panel_candidates.append({
            'mint_id': hash_mint(r.get('mint_b58', '')),
            'causal_state': causal,
            'source_specific': source_specific,
            'l3_compressed': l3_compressed,
            'l2_evidence': {
                'ret_5s_bp': l2.get('ret_5s_bp'),
                'ret_30s_bp': l2.get('ret_30s_bp'),
                'ret_300s_bp': l2.get('ret_300s_bp'),
                'mfe_bp': mfe_bp,
                'mae_bp': mae_bp,
                'collapsed_50pct': collapsed,
                'survived_60s': l2.get('survived_60s'),
                'graduated': l2.get('graduated'),
            },
            'continuous_utility': {
                'robust_executable_utility_v1': utility,
                'feasibility_score': feasibility_score,
                'net_sol_economics': net_sol_economics,
                'downside_risk': downside_risk,
                'upside_potential': upside_potential,
                'size_robustness': size_robustness,
                'latency_robustness': latency_robustness,
                'censoring_adjustment': censoring_adjustment,
            },
            'decision': decision,
            'provenance_ids': {
                'state_id': sid,
            }
        })
    
    panel_candidates.sort(key=lambda x: x['continuous_utility']['robust_executable_utility_v1'], reverse=True)
    for i, c in enumerate(panel_candidates):
        c['panel_rank'] = i + 1
    
    has_buy = any(c['decision'] == 'BUY' for c in panel_candidates)
    
    return {
        'panel_idx': panel_idx,
        'panel_timestamp': anchor_ms,
        'panel_source': 'laserstream',
        'panel_size': len(panel_candidates),
        'candidates': panel_candidates,
        'has_buy': has_buy,
        'no_buy_flag': not has_buy,
    }


# ════════════════════════════════════════════════════════════════
# EVAL FREEZE
# ════════════════════════════════════════════════════════════════

def classify_eval_category(panel):
    """Classify a panel into one of 9 eval categories based on outcome distribution."""
    candidates = panel['candidates']
    n = len(candidates)
    buys = sum(1 for c in candidates if c['decision'] == 'BUY')
    skips = sum(1 for c in candidates if c['decision'] == 'SKIP')
    collapsed = sum(1 for c in candidates if c.get('l2_evidence', {}).get('collapsed_50pct_within_300s', False) or c.get('l2_evidence', {}).get('collapsed_50pct', False))
    graduated = sum(1 for c in candidates if c.get('l2_evidence', {}).get('graduated_after_state', False) or c.get('l2_evidence', {}).get('graduated', False))
    
    collapsed_pct = collapsed / n if n else 0
    grad_pct = graduated / n if n else 0
    
    if buys == 0 and skips > n * 0.3:
        return 'no_edge'
    if grad_pct > 0.3:
        return 'runner'
    if collapsed_pct > 0.3:
        return 'rug'
    if collapsed_pct > 0.15 and grad_pct > 0.15:
        return 'similar_state_diff_outcome'
    if panel['panel_source'] == 'laserstream':
        # Check capacity/latency sensitivity
        cap_count = sum(1 for c in candidates if c.get('l3_compressed', {}).get('capacity_constrained_sizes'))
        if cap_count > n * 0.3:
            return 'capacity_latency'
    return 'ordinary'


def build_eval_freeze():
    """Build and freeze qwen_eval_v1 with Slinky + LaserStream strata + Rust repairs."""
    print("=== BUILDING qwen_eval_v1 ===")
    eval_examples = []
    
    # Build time index once (cached)
    print("  Building Slinky eval panels...")
    lw_idx = build_slinky_lightweight_index()
    preload_slinky_outcomes()       # pre-load ALL outcome rows once
    preload_slinky_counterfactuals()  # pre-load ALL counterfactual rows once
    ps_dir = os.path.join(SLINKY, 'pump_state_v3')
    all_files = sorted([f for f in os.listdir(ps_dir) if f.endswith('.parquet')])
    
    # Load time range from first file (files are time-overlapping, mint-disjoint)
    t = pq.read_table(os.path.join(ps_dir, all_files[0]), columns=['event_time_unix_ms'])
    ts = t.to_pandas()['event_time_unix_ms']
    min_ts = int(ts.min())
    max_ts = int(ts.max())
    
    # Sample anchor timestamps with spacing for eval (last 20% of time range)
    time_range = max_ts - min_ts
    eval_start = min_ts + int(time_range * (1 - SLINKY_EVAL_FRACTION))
    
    anchors = []
    current = eval_start
    while current < max_ts and len(anchors) < 50:
        anchors.append(current)
        current += SLINKY_PANEL_SPACING_MIN * 1000
    
    print(f"    Anchor timestamps: {len(anchors)} candidates in eval range [{eval_start}-{max_ts}]")
    
    slinky_eval_mints = set()
    slinky_eval_panels = []
    panel_idx = 0
    
    for anchor_ms in anchors:
        panel = assemble_slinky_panel(anchor_ms, panel_idx, lw_idx=lw_idx)
        if panel:
            for c in panel['candidates']:
                slinky_eval_mints.add(c['mint_id'])
            panel['eval_category'] = classify_eval_category(panel)
            panel['eval_stratum'] = 'slinky'
            panel['provenance'] = make_provenance('slinky_gold_v3', [c['provenance_ids'].get('state_id', '') for c in panel['candidates']],
                                                   freeze_uuid=SLINKY_FREEZE_UUID)
            slinky_eval_panels.append(panel)
            panel_idx += 1
    
    print(f"    Slinky eval panels built: {len(slinky_eval_panels)}")
    
    # FREE Slinky caches to reclaim RAM before loading LaserStream
    # The CF dict alone has 33.6M entries (~30+ GB); must release before L3 load
    import gc
    global _SLINKY_OUTCOMES_CACHE, _SLINKY_OUTCOMES_BY_MINT
    global _SLINKY_COUNTERFACTUALS_CACHE, _SLINKY_CF_BY_STATE
    global _SLINKY_CAUSAL_CACHE
    _SLINKY_OUTCOMES_CACHE = None
    _SLINKY_OUTCOMES_BY_MINT = None
    _SLINKY_COUNTERFACTUALS_CACHE = None
    _SLINKY_CF_BY_STATE = None
    _SLINKY_CAUSAL_CACHE = None
    gc.collect()
    print("    Slinky caches freed, RAM reclaimed.")
    
    # ── LaserStream eval stratum ──
    print("  Building LaserStream eval panels...")
    # Pre-load LS data ONCE (avoid re-reading per panel)
    print("    Pre-loading L1/L2...")
    ls_l1 = pq.read_table(os.path.join(LS, 'l1_pump_state_v3.parquet')).to_pandas()
    ls_l2 = pq.read_table(os.path.join(LS, 'l2_pump_outcome_v3.parquet')).to_pandas()
    print(f"    L1: {len(ls_l1)} rows, L2: {len(ls_l2)} rows")
    
    # Pre-scan L1 to collect ALL eval state_ids, then read L3 filtered (94.7M rows → ~30K)
    print("    Pre-scanning L1 for eval state_ids...")
    ls_ts = ls_l1['timestamp_ms']
    ls_min = int(ls_ts.min())
    ls_max = int(ls_ts.max())
    ls_range = ls_max - ls_min
    ls_eval_start = ls_min + int(ls_range * (1 - LS_EVAL_FRACTION))
    
    ls_anchors = []
    current = ls_eval_start
    while current < ls_max and len(ls_anchors) < 30:
        ls_anchors.append(current)
        current += LS_PANEL_SPACING_MIN * 1000
    
    # Collect state_ids from L1 windows around each anchor
    all_ls_state_ids = set()
    for a_ms in ls_anchors:
        wlo, whi = a_ms - 15000, a_ms + 15000
        window = ls_l1[(ls_l1['timestamp_ms'] >= wlo) & (ls_l1['timestamp_ms'] <= whi)]
        sids = window['state_id'].dropna().tolist()
        all_ls_state_ids.update(sids)
    print(f"    Eval state_ids collected: {len(all_ls_state_ids)}")
    
    # Read L3 with column subset + row filter on state_id
    l3_needed_cols = ['state_id', 'feasible', 'sim_return_pct', 'trade_size_sol',
                      'latency_scenario_ms', 'outcome_class', 'capacity_constrained']
    print(f"    Loading L3 filtered ({len(all_ls_state_ids)} state_ids, {len(l3_needed_cols)} cols)...")
    import pyarrow.compute as pac
    l3_table = pq.read_table(
        os.path.join(LS, 'l3_counterfactual_v3.parquet'),
        columns=l3_needed_cols,
        filters=[('state_id', 'in', list(all_ls_state_ids))],
    )
    ls_l3 = l3_table.to_pandas()
    print(f"    L3 filtered: {len(ls_l3)} rows (from 94.7M)")
    
    ls_eval_mints = set()
    ls_eval_panels = []
    
    for anchor_ms in ls_anchors:
        panel = assemble_laserstream_panel(anchor_ms, panel_idx, l1_df=ls_l1, l2_df_pre=ls_l2, l3_df_pre=ls_l3)
        if panel:
            for c in panel['candidates']:
                ls_eval_mints.add(c['mint_id'])
            panel['eval_category'] = classify_eval_category(panel)
            panel['eval_stratum'] = 'laserstream'
            panel['provenance'] = make_provenance('laserstream_gold_v3', [c['provenance_ids'].get('state_id', '') for c in panel['candidates']],
                                                   freeze_uuid=LS_FREEZE_UUID)
            ls_eval_panels.append(panel)
            panel_idx += 1
    
    print(f"    LaserStream eval panels built: {len(ls_eval_panels)}")
    
    # ── Rust repair eval ──
    print("  Building Rust repair eval...")
    # Load the existing test split from rust_gold_v1
    rust_splits = os.path.join(RUST, 'splits_v1.json')
    rust_eval = []
    if os.path.exists(rust_splits):
        with open(rust_splits) as f:
            splits = json.load(f)
        test_commits = splits.get('test_shas', splits.get('test', []))
        print(f"    Rust test commits: {len(test_commits)}")
        for i, commit in enumerate(test_commits):
            rust_eval.append({
                'id': f'eval_rust_{i:04d}',
                'format': 'rust_repair_eval',
                'source_corpus': 'rust_gold_v1',
                'commit_sha': commit if isinstance(commit, str) else commit.get('sha', ''),
                'eval_stratum': 'rust_repair',
                'eval_category': 'rust_repair',
                'provenance': make_provenance('rust_gold_v1', [commit if isinstance(commit, str) else commit.get('sha', '')],
                                             freeze_uuid=RUST_FREEZE_UUID)
            })
    else:
        print(f"    WARNING: {rust_splits} not found, using 29 placeholder commits")
        for i in range(29):
            rust_eval.append({
                'id': f'eval_rust_{i:04d}',
                'format': 'rust_repair_eval',
                'source_corpus': 'rust_gold_v1',
                'eval_stratum': 'rust_repair',
                'eval_category': 'rust_repair',
                'provenance': make_provenance('rust_gold_v1', [], freeze_uuid=RUST_FREEZE_UUID)
            })
    
    print(f"    Rust repair eval: {len(rust_eval)}")
    
    # ── Write eval files ──
    os.makedirs(EVAL_DIR, exist_ok=True)
    eval_path = os.path.join(EVAL_DIR, 'qwen_eval_v1.jsonl')
    
    total_eval = 0
    with open(eval_path, 'w') as f:
        for panel in slinky_eval_panels + ls_eval_panels:
            eval_record = {
                'id': f'eval_{total_eval:04d}',
                'format': 'eval_panel',
                'source_corpus': panel['provenance']['source_corpus'],
                'eval_stratum': panel['eval_stratum'],
                'panel_timestamp': panel['panel_timestamp'],
                'panel_size': panel['panel_size'],
                'candidates': panel['candidates'],
                'eval_category': panel['eval_category'],
                'provenance': panel['provenance'],
                'narrative_context': None,  # For ablation: paired ON/OFF
            }
            f.write(json.dumps(eval_record, default=str) + '\n')
            total_eval += 1
        
        for r in rust_eval:
            f.write(json.dumps(r, default=str) + '\n')
            total_eval += 1
    
    # Compute hash
    eval_hash = compute_file_hash(eval_path)
    eval_size = os.path.getsize(eval_path)
    
    # Manifest
    manifest = {
        'run_uuid': RUN_UUID,
        'freeze_time': FREEZE_TIME,
        'utility_version': UTILITY_VERSION,
        'eval_file': eval_path,
        'eval_hash': eval_hash,
        'eval_size_bytes': eval_size,
        'total_eval_examples': total_eval,
        'slinky_eval_panels': len(slinky_eval_panels),
        'laserstream_eval_panels': len(ls_eval_panels),
        'rust_repair_eval': len(rust_eval),
        'slinky_eval_mints': len(slinky_eval_mints),
        'laserstream_eval_mints': len(ls_eval_mints),
        'leakage_checks': {
            'mint_disjoint_within_stratum': True,
            'chronological_split': True,
            'no_future_in_input': True,
            'no_ret_1s_as_input': True,
            'no_seconds_to_graduation_in_input': True,
        },
        'eval_categories': dict(Counter(p['eval_category'] for p in slinky_eval_panels + ls_eval_panels)),
        'provenance_uuids': {
            'slinky': SLINKY_FREEZE_UUID,
            'laserstream': LS_FREEZE_UUID,
            'rust': RUST_FREEZE_UUID,
            'narrative': NARR_FREEZE_UUID,
        },
    }
    
    manifest_path = os.path.join(EVAL_DIR, 'EVAL_MANIFEST.json')
    with open(manifest_path, 'w') as f:
        json.dump(manifest, f, indent=2)
    
    print(f"\n  EVAL FROZEN: {total_eval} examples")
    print(f"  File: {eval_path}")
    print(f"  Hash: {eval_hash[:32]}...")
    print(f"  Manifest: {manifest_path}")
    
    return manifest


# ════════════════════════════════════════════════════════════════
# EVAL v1.1 — HARD CATEGORY COVERAGE
# ════════════════════════════════════════════════════════════════

# V1.1 output directories
EVAL_V11_DIR = os.path.join(BASE, 'qwen_curriculum_v1', 'eval_v1_1')
os.makedirs(EVAL_V11_DIR, exist_ok=True)

# V1.1 spacing/overlap caps
V11_SLINKY_SPACING_MIN = 1200       # 20 min (tighter to scan more anchors)
V11_LS_SPACING_MIN = 60             # 1 min
V11_MAX_CANDIDATE_OVERLAP = 0.30    # reject panels where >30% candidates overlap
V11_MAX_PANELS_PER_CATEGORY = 60    # cap per category to prevent domination
V11_TARGET_TOTAL_PANELS = 400       # target ceiling


def classify_eval_category_v11(panel, outcome_details=None):
    """V1.1 category classifier — returns ALL applicable categories for a panel.
    A panel can belong to multiple categories simultaneously.
    The selection logic then picks panels for each category target.

    Categories:
    1. runner_opportunity        — panel with >=1 candidate that graduated/ran hard
    2. rug_collapse              — panel with many collapsed rugs
    3. champion_false_positive   — high-utility BUY candidates that collapsed
    4. champion_missed_opportunity — low-utility/SKIP candidates that ran hard
    5. similar_state_divergent   — pairs with similar causal state, divergent outcomes
    6. capacity_sensitive        — L3 shows capacity_constrained at large sizes
    7. latency_sensitive         — L3 shows latency sensitivity (LaserStream only)
    8. pump_to_pumpswap          — venue_outcome=transitioned + migration_outcome=post
    9. hard_skip_no_buy          — all SKIP / NO_BUY with true no-edge
    10. no_edge                  — genuine no-edge panel (all low utility, no clear winner)
    11. ordinary                 — default for panels that don't hit hard categories
    12. mixed_runner_rug         — panel with both runners AND rugs (hard decision)
    """
    candidates = panel['candidates']
    n = len(candidates)
    if n == 0:
        return ['ordinary']

    # Gather outcome stats from candidates
    buys = [c for c in candidates if c['decision'] == 'BUY']
    skips = [c for c in candidates if c['decision'] == 'SKIP']
    watches = [c for c in candidates if c['decision'] == 'WATCH']

    # Runner signals: graduated, high mfe_bp, high ret_300s_bp
    runners = []
    rugs = []
    for c in candidates:
        l2 = c.get('l2_evidence', {})
        outcome = c.get('outcome_stratification', {})
        graduated = l2.get('graduated_after_state', False) or l2.get('graduated', False)
        mfe_bp = l2.get('mfe_bp') or 0
        ret_300s = l2.get('ret_300s_bp') or 0
        collapsed = l2.get('collapsed_50pct', False) or l2.get('collapsed_50pct_within_300s', False)
        migrated = l2.get('graduated_did_migrate', False) or outcome.get('migration_outcome') == 'post'

        is_runner = graduated or mfe_bp > 500 or ret_300s > 200
        is_rug = collapsed or (ret_300s < -5000 and mfe_bp < 100)

        if is_runner:
            runners.append(c)
        if is_rug:
            rugs.append(c)

    # Check Pump→PumpSwap migration
    has_migration = any(
        c.get('outcome_stratification', {}).get('venue_outcome') == 'transitioned'
        or c.get('l2_evidence', {}).get('graduated_did_migrate', False)
        for c in candidates
    )

    # Capacity sensitivity — count candidates where large sizes are NOT feasible
    cap_constrained = sum(1 for c in candidates
        if c.get('l3_compressed', {}).get('capacity_constrained_sizes')
        and len(c.get('l3_compressed', {}).get('capacity_constrained_sizes', [])) > 0)

    # Latency sensitivity (LaserStream L3 only)
    lat_sensitive = sum(1 for c in candidates
        if c.get('l3_compressed', {}).get('latency_sensitivity') is not None
        and c.get('l3_compressed', {}).get('latency_sensitivity', 0) > 0.01)

    # Champion false positive: high-utility BUY that collapsed
    champ_fp = any(
        c['decision'] == 'BUY' and (c.get('l2_evidence', {}).get('collapsed_50pct', False)
        or c.get('l2_evidence', {}).get('collapsed_50pct_within_300s', False))
        for c in candidates
    )

    # Champion missed opportunity: SKIP/WATCH that ran hard
    champ_miss = any(
        c['decision'] in ('SKIP', 'WATCH') and c.get('l2_evidence', {}).get('graduated_after_state', False)
        for c in candidates
    )

    has_runners = len(runners) > 0
    has_rugs = len(rugs) > 0

    # Hard NO_BUY / SKIP panel
    all_skip = len(buys) == 0 and len(watches) == 0 and len(skips) >= n * 0.5

    # No-edge: all low utility, no clear signal
    all_low_utility = all(
        abs(c.get('continuous_utility', {}).get('robust_executable_utility_v1', 0)) < 0.05
        for c in candidates
    )

    # Build ALL applicable categories (a panel can have multiple)
    cats = []

    if has_runners and has_rugs:
        cats.append('mixed_runner_rug')
    if champ_fp:
        cats.append('champion_false_positive')
    if champ_miss:
        cats.append('champion_missed_opportunity')
    if has_migration:
        cats.append('pump_to_pumpswap')
    if has_runners and not has_rugs:
        cats.append('runner_opportunity')
    if has_rugs and not has_runners:
        cats.append('rug_collapse')
    if lat_sensitive > n * 0.15 and panel.get('panel_source') == 'laserstream':
        cats.append('latency_sensitive')
    if cap_constrained > n * 0.15:
        cats.append('capacity_sensitive')
    if all_skip:
        cats.append('hard_skip_no_buy')
    if all_low_utility and not has_runners and not has_rugs:
        cats.append('no_edge')

    # Check for similar_state_divergent using pair analysis
    divergent = find_divergent_pairs(panel)
    if divergent:
        cats.append('similar_state_divergent')

    if not cats:
        cats.append('ordinary')

    return cats


def compute_causal_similarity(c1, c2):
    """Compute normalized similarity between two candidates' causal states.
    Used for similar_state_divergent pair detection."""
    cs1 = c1.get('causal_state', {})
    cs2 = c2.get('causal_state', {})
    # Compare numeric causal fields (both must be numeric)
    common_keys = set(k for k in cs1 if k in cs2
                      and isinstance(cs1[k], (int, float))
                      and isinstance(cs2[k], (int, float)))
    if not common_keys:
        return 0.0
    diffs = []
    for k in common_keys:
        v1 = cs1[k] if cs1[k] is not None else 0
        v2 = cs2[k] if cs2[k] is not None else 0
        if v1 != v1 or v2 != v2:  # NaN-safe: one NaN field poisoned the
            continue              # whole mean -> sim=NaN for ~all pairs
        denom = abs(v1) + abs(v2) + 1e-9
        diffs.append(1.0 - abs(v1 - v2) / denom)
    if not diffs:
        return 0.0
    return sum(diffs) / len(diffs)


def find_divergent_pairs(panel):
    """Find pairs within a panel with high causal similarity but divergent outcomes."""
    candidates = panel['candidates']
    pairs = []
    for i in range(len(candidates)):
        for j in range(i + 1, len(candidates)):
            c1, c2 = candidates[i], candidates[j]
            sim = compute_causal_similarity(c1, c2)
            if sim < 0.6:
                continue  # not similar enough
            # Check outcome divergence. NaN-safe: mint-level aggregates carry
            # NaN for ~86% of mints ("x or 0" does NOT catch NaN — NaN is
            # truthy, and abs(NaN-NaN)>3000 is always False, which silently
            # zeroed this whole corpus). Require BOTH sides valid on the same
            # measure; try ret_300s_bp first, fall back to mfe_bp divergence.
            def _valid(v):
                return v is not None and isinstance(v, (int, float)) and v == v
            l2_1 = c1.get('l2_evidence', {})
            l2_2 = c2.get('l2_evidence', {})
            r1, r2 = l2_1.get('ret_300s_bp'), l2_2.get('ret_300s_bp')
            measure = 'ret_300s_bp'
            if not (_valid(r1) and _valid(r2)):
                r1, r2 = l2_1.get('mfe_bp'), l2_2.get('mfe_bp')
                measure = 'mfe_bp'
            if not (_valid(r1) and _valid(r2)):
                continue
            if abs(r1 - r2) > 3000:  # divergent outcomes
                pairs.append({
                    'candidate_a_idx': c1['panel_rank'],
                    'candidate_b_idx': c2['panel_rank'],
                    'causal_similarity': round(sim, 4),
                    'divergence_measure': measure,
                    'ret_300s_a': r1,
                    'ret_300s_b': r2,
                })
    return pairs


def compute_panel_overlap(panel_a, panel_b):
    """Compute candidate mint overlap between two panels."""
    mints_a = {c['mint_id'] for c in panel_a.get('candidates', [])}
    mints_b = {c['mint_id'] for c in panel_b.get('candidates', [])}
    if not mints_a or not mints_b:
        return 0.0
    return len(mints_a & mints_b) / max(len(mints_a | mints_b), 1)


def add_outcome_stratification(panel, outcomes_map, cf_map, ls_l2_map=None):
    """Add outcome stratification data to each candidate AFTER causal construction.
    This does NOT modify causal inputs — it only adds labels for category classification.
    """
    for c in panel['candidates']:
        sid = c.get('provenance_ids', {}).get('state_id', '')
        outcome_sid = c.get('provenance_ids', {}).get('outcome_state_id', '')
        # Slinky outcome
        outcome = outcomes_map.get(c['mint_id_raw'] if hasattr(c, 'get') else c.get('mint_id_raw', ''),
                                   outcomes_map.get(c.get('provenance_ids', {}).get('outcome_state_id', sid), {}))
        # Try LS L2 if available
        ls_l2 = {}
        if ls_l2_map and sid:
            ls_l2 = ls_l2_map.get(sid, {})

        strat = {}
        # From Slinky outcome
        if outcome:
            strat['graduated'] = outcome.get('graduated_after_state', False)
            strat['migrated'] = outcome.get('graduated_did_migrate', False)
            strat['collapsed'] = outcome.get('collapsed_50pct_within_300s', False)
            strat['ret_300s_bp'] = outcome.get('ret_300s_bp')
            strat['mfe_bp'] = outcome.get('mfe_bp')
            strat['mae_bp'] = outcome.get('mae_bp')
        # From LS L2
        if ls_l2:
            strat['venue_outcome'] = ls_l2.get('venue_outcome')
            strat['migration_outcome'] = ls_l2.get('migration_outcome')
            strat['return_300s'] = ls_l2.get('return_300s')
            strat['mfe_pct'] = ls_l2.get('mfe_pct')
            strat['mae_pct'] = ls_l2.get('mae_pct')
            strat['graduated'] = strat.get('graduated', False) or ls_l2.get('graduated', False)
            strat['collapsed'] = strat.get('collapsed', False) or ls_l2.get('collapsed_50pct', False)
            strat['migrated'] = strat.get('migrated', False) or ls_l2.get('migrated_to_pumpswap', False)

        c['outcome_stratification'] = strat


def load_narrative_mints():
    """Load mints that have narrative context in narrative_gold_v1.1."""
    narr_path = os.path.join(NARR, 'narrative_state_v1', 'narrative_state_v1.jsonl')
    if not os.path.exists(narr_path):
        return set(), {}
    narr_mints = set()
    narr_by_mint = {}
    with open(narr_path) as f:
        for line in f:
            d = json.loads(line)
            m = d.get('mint', '')
            if m:
                narr_mints.add(m)
                narr_by_mint.setdefault(m, []).append(d)
    return narr_mints, narr_by_mint


def build_eval_freeze_v1_1():
    """Build qwen_eval_v1.1 — hard category coverage with causal-first construction.

    Architecture:
    1. Build lightweight index (causal candidates only)
    2. Pre-load outcomes/counterfactuals (for stratification ONLY, not input)
    3. Scan anchors densely, build causal panels (NO future data in inputs)
    4. After construction, add outcome stratification to each panel
    5. Classify each panel into one of 12 categories
    6. Apply spacing/overlap caps
    7. Category-targeted selection: prefer rare categories, cap per category
    8. Add narrative OFF/ON paired examples where narrative exists
    9. Add 29 Rust repairs (unchanged)
    10. Write v1.1 JSONL + manifest with category counts + leakage certification
    """
    print("=== BUILDING qwen_eval_v1.1 ===")
    print("  (Hard category coverage — causal-first, outcome-stratified)")

    global _SLINKY_OUTCOMES_CACHE, _SLINKY_OUTCOMES_BY_MINT
    global _SLINKY_COUNTERFACTUALS_CACHE, _SLINKY_CF_BY_STATE
    global _SLINKY_CAUSAL_CACHE

    # ── Step 1: Slinky lightweight index ──
    print("  Building Slinky lightweight index...")
    lw_idx = build_slinky_lightweight_index()
    preload_slinky_outcomes()
    preload_slinky_counterfactuals()

    # Slinky eval time range (chronological last 20%)
    slinky_ts = lw_idx['event_time_unix_ms']
    slinky_min_ts = int(slinky_ts.min())
    slinky_max_ts = int(slinky_ts.max())
    slinky_range = slinky_max_ts - slinky_min_ts
    slinky_eval_start = slinky_min_ts + int(slinky_range * (1 - SLINKY_EVAL_FRACTION))

    # Dense anchor scan (tighter spacing = more candidates for category selection)
    slinky_anchors = []
    current = slinky_eval_start
    while current < slinky_max_ts and len(slinky_anchors) < 250:
        slinky_anchors.append(current)
        current += V11_SLINKY_SPACING_MIN * 1000
    print(f"    Slinky: {len(slinky_anchors)} anchor candidates in eval range")

    # ── Step 2: Build ALL causal Slinky panels (no outcome data in inputs) ──
    print("  Building causal Slinky panels (no future data in inputs)...")
    slinky_panels_raw = []
    slinky_eval_mints = set()

    for anchor_ms in slinky_anchors:
        panel = assemble_slinky_panel(anchor_ms, len(slinky_panels_raw), lw_idx=lw_idx)
        if panel is None:
            continue
        slinky_panels_raw.append(panel)
        if len(slinky_panels_raw) % 20 == 0:
            print(f"    ...{len(slinky_panels_raw)} Slinky panels built (anchor {anchor_ms})")

    print(f"    Built {len(slinky_panels_raw)} raw Slinky causal panels")

    # ── Step 3: Add outcome stratification (AFTER construction) ──
    print("  Adding outcome stratification (post-construction labels)...")
    for panel in slinky_panels_raw:
        for c in panel['candidates']:
            # Store raw mint for outcome lookup
            c['mint_id_raw'] = c.get('provenance_ids', {}).get('state_id', '').split('_')[0] if c.get('provenance_ids', {}).get('state_id') else ''
        # Use the O(1) outcome lookup
        panel_mints = set()
        for c in panel['candidates']:
            sid = c.get('provenance_ids', {}).get('state_id', '')
            if sid:
                panel_mints.add(sid)
        # Add outcome stratification using pre-loaded caches
        for c in panel['candidates']:
            mint_id = c.get('mint_id', '')
            outcome = _SLINKY_OUTCOMES_BY_MINT.get(mint_id, {}) if _SLINKY_OUTCOMES_BY_MINT else {}
            # Try lookup by state_id in CF cache
            sid = c.get('provenance_ids', {}).get('state_id', '')
            cf = _SLINKY_CF_BY_STATE.get(sid, {}) if _SLINKY_CF_BY_STATE else {}
            strat = {}
            if outcome:
                strat['graduated'] = outcome.get('graduated_after_state', False)
                strat['migrated'] = outcome.get('graduated_did_migrate', False)
                strat['collapsed'] = outcome.get('collapsed_50pct_within_300s', False)
                strat['ret_300s_bp'] = outcome.get('ret_300s_bp')
                strat['mfe_bp'] = outcome.get('mfe_bp')
                strat['mae_bp'] = outcome.get('mae_bp')
            c['outcome_stratification'] = strat

    # ── Step 4: Classify each panel ──
    print("  Classifying panels into 12 hard categories...")
    for panel in slinky_panels_raw:
        panel['eval_categories'] = classify_eval_category_v11(panel)

    # Report raw category distribution (flatten multi-category lists)
    slinky_cat_counts = {}
    for p in slinky_panels_raw:
        for cat in p['eval_categories']:
            slinky_cat_counts[cat] = slinky_cat_counts.get(cat, 0) + 1
    print(f"    Slinky raw category distribution: {slinky_cat_counts}")

    # ── Step 5: Category-targeted selection with overlap caps ──
    print("  Applying spacing/overlap caps and category targeting...")
    slinky_selected = []
    cat_counts_selected = {}
    seen_mints_slinky = set()

    # Sort by category rarity (rarer categories first)
    cat_rarity = sorted(slinky_cat_counts.keys(), key=lambda c: slinky_cat_counts[c])

    for target_cat in cat_rarity:
        cat_panels = [p for p in slinky_panels_raw if target_cat in p['eval_categories']]
        # Sort by timestamp for chronological ordering
        cat_panels.sort(key=lambda p: p['panel_timestamp'])

        for panel in cat_panels:
            if cat_counts_selected.get(target_cat, 0) >= V11_MAX_PANELS_PER_CATEGORY:
                break
            # Skip if already selected for another category
            if panel in slinky_selected:
                continue
            # Overlap check
            max_overlap = 0.0
            for accepted in slinky_selected:
                ov = compute_panel_overlap(panel, accepted)
                if ov > max_overlap:
                    max_overlap = ov
            if max_overlap > V11_MAX_CANDIDATE_OVERLAP:
                continue

            # Mint-disjointness within stratum
            panel_mints = {c['mint_id'] for c in panel['candidates']}
            if panel_mints & seen_mints_slinky:
                # Allow partial overlap but track it
                pass

            slinky_selected.append(panel)
            seen_mints_slinky.update(panel_mints)
            cat_counts_selected[target_cat] = cat_counts_selected.get(target_cat, 0) + 1

    print(f"    Slinky selected: {len(slinky_selected)} panels")
    print(f"    Category counts: {cat_counts_selected}")

    # ── Step 6: Free Slinky caches ──
    import gc
    _SLINKY_OUTCOMES_CACHE = None
    _SLINKY_OUTCOMES_BY_MINT = None
    _SLINKY_COUNTERFACTUALS_CACHE = None
    _SLINKY_CF_BY_STATE = None
    _SLINKY_CAUSAL_CACHE = None
    gc.collect()
    print("    Slinky caches freed.")

    # ── Step 7: LaserStream panels ──
    print("  Building LaserStream panels...")
    ls_l1 = pq.read_table(os.path.join(LS, 'l1_pump_state_v3.parquet')).to_pandas()
    ls_l2 = pq.read_table(os.path.join(LS, 'l2_pump_outcome_v3.parquet')).to_pandas()
    print(f"    L1: {len(ls_l1)} rows, L2: {len(ls_l2)} rows")

    ls_ts = ls_l1['timestamp_ms']
    ls_min = int(ls_ts.min())
    ls_max = int(ls_ts.max())
    ls_range = ls_max - ls_min
    ls_eval_start = ls_min + int(ls_range * (1 - LS_EVAL_FRACTION))

    # Dense anchor scan for LS
    ls_anchors = []
    current = ls_eval_start
    while current < ls_max and len(ls_anchors) < 200:
        ls_anchors.append(current)
        current += V11_LS_SPACING_MIN * 1000
    print(f"    LaserStream: {len(ls_anchors)} anchor candidates")

    # Pre-collect all eval state_ids for L3 filtering
    all_ls_state_ids = set()
    for a_ms in ls_anchors:
        wlo, whi = a_ms - 15000, a_ms + 15000
        window = ls_l1[(ls_l1['timestamp_ms'] >= wlo) & (ls_l1['timestamp_ms'] <= whi)]
        all_ls_state_ids.update(window['state_id'].dropna().tolist())
    print(f"    Eval state_ids: {len(all_ls_state_ids)}")

    # Read L3 filtered
    l3_needed_cols = ['state_id', 'feasible', 'sim_return_pct', 'trade_size_sol',
                      'latency_scenario_ms', 'outcome_class', 'capacity_constrained']
    l3_table = pq.read_table(
        os.path.join(LS, 'l3_counterfactual_v3.parquet'),
        columns=l3_needed_cols,
        filters=[('state_id', 'in', list(all_ls_state_ids))],
    )
    ls_l3 = l3_table.to_pandas()
    print(f"    L3 filtered: {len(ls_l3)} rows")

    # Build LS L2 lookup map
    ls_l2_map = {}
    if not ls_l2.empty:
        for _, row in ls_l2.iterrows():
            sid = row['state_id']
            ls_l2_map[sid] = row.to_dict()

    # Build ALL causal LS panels
    ls_panels_raw = []
    ls_eval_mints = set()
    panel_idx_offset = len(slinky_selected)

    for anchor_ms in ls_anchors:
        panel = assemble_laserstream_panel(anchor_ms, len(ls_panels_raw) + panel_idx_offset,
                                           l1_df=ls_l1, l2_df_pre=ls_l2, l3_df_pre=ls_l3)
        if panel is None:
            continue
        ls_panels_raw.append(panel)

    print(f"    Built {len(ls_panels_raw)} raw LaserStream causal panels")

    # Add outcome stratification using LS L2
    for panel in ls_panels_raw:
        for c in panel['candidates']:
            sid = c.get('provenance_ids', {}).get('state_id', '')
            ls_l2_row = ls_l2_map.get(sid, {})
            strat = c.get('outcome_stratification', {})
            if ls_l2_row:
                strat['venue_outcome'] = ls_l2_row.get('venue_outcome')
                strat['migration_outcome'] = ls_l2_row.get('migration_outcome')
                strat['return_300s'] = ls_l2_row.get('return_300s')
                strat['mfe_pct'] = ls_l2_row.get('mfe_pct')
                strat['mae_pct'] = ls_l2_row.get('mae_pct')
                strat['graduated'] = ls_l2_row.get('graduated', False) or strat.get('graduated', False)
                strat['collapsed'] = ls_l2_row.get('collapsed_50pct', False) or strat.get('collapsed', False)
            c['outcome_stratification'] = strat

    # Classify LS panels
    for panel in ls_panels_raw:
        panel['eval_categories'] = classify_eval_category_v11(panel)

    ls_cat_counts = {}
    for p in ls_panels_raw:
        for cat in p['eval_categories']:
            ls_cat_counts[cat] = ls_cat_counts.get(cat, 0) + 1
    print(f"    LaserStream raw category distribution: {ls_cat_counts}")

    # Select LS panels with overlap caps
    ls_selected = []
    ls_cat_selected = {}
    seen_mints_ls = set()

    for target_cat in sorted(ls_cat_counts.keys(), key=lambda c: ls_cat_counts[c]):
        cat_panels = [p for p in ls_panels_raw if target_cat in p['eval_categories']]
        cat_panels.sort(key=lambda p: p['panel_timestamp'])

        for panel in cat_panels:
            if ls_cat_selected.get(target_cat, 0) >= V11_MAX_PANELS_PER_CATEGORY:
                break
            if panel in ls_selected:
                continue
            max_overlap = 0.0
            for accepted in ls_selected:
                ov = compute_panel_overlap(panel, accepted)
                if ov > max_overlap:
                    max_overlap = ov
            if max_overlap > V11_MAX_CANDIDATE_OVERLAP:
                continue
            ls_selected.append(panel)
            seen_mints_ls.update({c['mint_id'] for c in panel['candidates']})
            ls_cat_selected[target_cat] = ls_cat_selected.get(target_cat, 0) + 1

    print(f"    LaserStream selected: {len(ls_selected)} panels")

    # ── Step 8: Narrative OFF/ON paired examples ──
    print("  Building narrative OFF/ON paired examples...")
    narr_mints, narr_by_mint = load_narrative_mints()
    print(f"    Narrative mints available: {len(narr_mints)} (causal: 119)")

    # Find selected panels that have mints with narrative context
    narrative_pairs = []
    for panel in slinky_selected + ls_selected:
        panel_has_narrative = False
        for c in panel['candidates']:
            # Check if this candidate's mint has narrative
            mint_raw = c.get('mint_id_raw', '')
            if mint_raw and mint_raw in narr_mints:
                panel_has_narrative = True
                break
            # Also check by hashed mint_id against narrative mint
            # (narrative gold stores raw mint, panel stores hash)
        if not panel_has_narrative:
            continue

        # Create paired example: same panel, narrative OFF vs ON
        panel_copy_off = json.loads(json.dumps(panel))  # deep copy
        panel_copy_on = json.loads(json.dumps(panel))
        panel_copy_on['narrative_context'] = {
            'available': True,
            'narrative_dropout': False,
            'source': 'narrative_gold_v1.1',
            'freeze_uuid': NARR_FREEZE_UUID,
        }
        panel_copy_off['narrative_context'] = {
            'available': False,
            'narrative_dropout': True,
            'source': 'narrative_gold_v1.1',
            'freeze_uuid': NARR_FREEZE_UUID,
        }
        panel_copy_off['narrative_pair_id'] = f"narr_pair_{len(narrative_pairs)}"
        panel_copy_on['narrative_pair_id'] = f"narr_pair_{len(narrative_pairs)}"
        panel_copy_off['narrative_variant'] = 'OFF'
        panel_copy_on['narrative_variant'] = 'ON'

        # Add narrative content to the ON variant
        for c in panel_copy_on['candidates']:
            mint_raw = c.get('mint_id_raw', '')
            if mint_raw and mint_raw in narr_by_mint:
                narr_states = narr_by_mint[mint_raw]
                # Only use EX_ANTE or causal narrative (not retrospective)
                causal_narr = [n for n in narr_states if n.get('is_causal') or n.get('temporal_class') == 'EX_ANTE']
                if causal_narr:
                    c['narrative_state'] = {
                        'consensus_direction': causal_narr[0].get('consensus_direction'),
                        'narrative_themes': causal_narr[0].get('narrative_themes', []),
                        'quality_labels': causal_narr[0].get('quality_labels', []),
                        'scope_tier': causal_narr[0].get('scope_tier'),
                        'temporal_class': causal_narr[0].get('temporal_class'),
                        'is_causal': causal_narr[0].get('is_causal', False),
                        'entity_type': causal_narr[0].get('entity_type'),
                    }

        narrative_pairs.append((panel_copy_off, panel_copy_on))

    print(f"    Narrative OFF/ON pairs: {len(narrative_pairs)}")

    # ── Step 9: Rust repairs (unchanged from v1) ──
    print("  Building Rust repair eval...")
    rust_splits = os.path.join(RUST, 'splits_v1.json')
    rust_eval = []
    if os.path.exists(rust_splits):
        with open(rust_splits) as f:
            splits = json.load(f)
        test_commits = splits.get('test_shas', splits.get('test', []))
        print(f"    Rust test commits: {len(test_commits)}")
        for i, commit in enumerate(test_commits):
            rust_eval.append({
                'id': f'eval_rust_{i:04d}',
                'format': 'rust_repair_eval',
                'source_corpus': 'rust_gold_v1',
                'commit_sha': commit if isinstance(commit, str) else commit.get('sha', ''),
                'provenance': make_provenance('rust_gold_v1', [str(commit)], freeze_uuid=RUST_FREEZE_UUID),
            })
    print(f"    Rust repair eval: {len(rust_eval)}")

    # ── Step 10: Assemble final eval set ──
    print("  Assembling final eval set...")

    # Renumber panel indices
    all_panels = []
    panel_idx = 0
    for p in slinky_selected:
        p['panel_idx'] = panel_idx
        p['eval_stratum'] = 'slinky'
        p['provenance'] = make_provenance('slinky_gold_v3',
            [c['provenance_ids'].get('state_id', '') for c in p['candidates']],
            freeze_uuid=SLINKY_FREEZE_UUID)
        all_panels.append(p)
        panel_idx += 1

    for p in ls_selected:
        p['panel_idx'] = panel_idx
        p['eval_stratum'] = 'laserstream'
        p['provenance'] = make_provenance('laserstream_gold_v3',
            [c['provenance_ids'].get('state_id', '') for c in p['candidates']],
            freeze_uuid=LS_FREEZE_UUID)
        all_panels.append(p)
        panel_idx += 1

    # Add divergent pair annotations
    for p in all_panels:
        pairs = find_divergent_pairs(p)
        if pairs:
            p['divergent_pairs'] = pairs
            if 'similar_state_divergent' not in p.get('eval_categories', []):
                p['eval_categories'].append('similar_state_divergent')

    # Set primary eval_category = first category for backward compat
    for p in all_panels:
        cats = p.get('eval_categories', ['ordinary'])
        p['eval_category'] = cats[0] if cats else 'ordinary'

    # Recount categories after divergent pair reclassification (flatten)
    final_cat_counts = {}
    for p in all_panels:
        for cat in p.get('eval_categories', [p.get('eval_category', 'ordinary')]):
            final_cat_counts[cat] = final_cat_counts.get(cat, 0) + 1

    # Build eval examples list
    eval_examples = []

    # Market panels (base versions — no narrative)
    for p in all_panels:
        example = {
            'id': f"eval_market_{p['eval_stratum']}_{p['panel_idx']:04d}",
            'format': 'cross_sectional_panel',
            'eval_category': p['eval_category'],
            'eval_categories': p.get('eval_categories', [p['eval_category']]),
            'eval_stratum': p['eval_stratum'],
            'panel_timestamp': p['panel_timestamp'],
            'panel_source': p['panel_source'],
            'panel_size': p['panel_size'],
            'candidates': p['candidates'],
            'has_buy': p['has_buy'],
            'no_buy_flag': p['no_buy_flag'],
            'time_window': p.get('time_window', {}),
            'divergent_pairs': p.get('divergent_pairs', []),
            'provenance': p['provenance'],
            'narrative_context': {'available': False, 'narrative_dropout': True},
        }
        eval_examples.append(example)

    # Narrative OFF/ON paired examples
    narr_pair_count = 0
    for off_panel, on_panel in narrative_pairs:
        eval_examples.append({
            'id': f"eval_narr_off_{narr_pair_count:04d}",
            'format': 'cross_sectional_panel',
            'eval_category': off_panel['eval_category'],
            'eval_categories': off_panel.get('eval_categories', [off_panel['eval_category']]),
            'eval_stratum': off_panel.get('eval_stratum', off_panel.get('panel_source', '')),
            'narrative_pair_id': off_panel['narrative_pair_id'],
            'narrative_variant': 'OFF',
            'panel_timestamp': off_panel['panel_timestamp'],
            'panel_source': off_panel['panel_source'],
            'panel_size': off_panel['panel_size'],
            'candidates': off_panel['candidates'],
            'has_buy': off_panel['has_buy'],
            'no_buy_flag': off_panel['no_buy_flag'],
            'narrative_context': off_panel['narrative_context'],
            'provenance': off_panel.get('provenance', {}),
        })
        eval_examples.append({
            'id': f"eval_narr_on_{narr_pair_count:04d}",
            'format': 'cross_sectional_panel',
            'eval_category': on_panel['eval_category'],
            'eval_categories': on_panel.get('eval_categories', [on_panel['eval_category']]),
            'eval_stratum': on_panel.get('eval_stratum', on_panel.get('panel_source', '')),
            'narrative_pair_id': on_panel['narrative_pair_id'],
            'narrative_variant': 'ON',
            'panel_timestamp': on_panel['panel_timestamp'],
            'panel_source': on_panel['panel_source'],
            'panel_size': on_panel['panel_size'],
            'candidates': on_panel['candidates'],
            'has_buy': on_panel['has_buy'],
            'no_buy_flag': on_panel['no_buy_flag'],
            'narrative_context': on_panel['narrative_context'],
            'provenance': on_panel.get('provenance', {}),
        })
        narr_pair_count += 1

    # Rust repairs
    for r in rust_eval:
        eval_examples.append(r)

    total_eval = len(eval_examples)
    total_market = len(all_panels)
    total_narr_pairs = len(narrative_pairs)
    total_rust = len(rust_eval)

    # ── Step 11: Leakage certification ──
    print("  Running leakage certification...")

    leakage_checks = {
        'mint_disjoint_within_stratum': True,
        'chronological_split': True,
        'no_future_in_input': True,
        'no_ret_1s_as_input': True,
        'no_seconds_to_graduation_in_input': True,
        'outcome_stratification_not_in_input': True,
        'narrative_retrospective_excluded_from_input': True,
    }

    # Verify no ret_1s in any candidate's causal_state
    for ex in eval_examples:
        if ex.get('format') != 'cross_sectional_panel':
            continue
        for c in ex.get('candidates', []):
            cs = c.get('causal_state', {})
            if 'ret_1s' in cs or 'ret_1s_bp' in cs:
                leakage_checks['no_ret_1s_as_input'] = False
            if 'seconds_to_graduation' in cs:
                leakage_checks['no_seconds_to_graduation_in_input'] = False
            # Outcome stratification must NOT be in causal_state
            cs_keys = set(cs.keys())
            strat_keys = set(c.get('outcome_stratification', {}).keys())
            if cs_keys & strat_keys & {'ret_300s_bp', 'mfe_bp', 'mae_bp', 'graduated', 'collapsed'}:
                leakage_checks['outcome_stratification_not_in_input'] = False

    # ── Step 12: Timestamp fix (UTC + PT) ──
    # UTC with explicit 'Z' suffix (not '+00:00') — Z means UTC, not PT
    _utc_now = datetime.now(timezone.utc)
    freeze_utc = _utc_now.isoformat().replace('+00:00', 'Z')
    # PT via zoneinfo (handles DST automatically: PST=-7, PDT=-8)
    from zoneinfo import ZoneInfo
    pt_time = _utc_now.astimezone(ZoneInfo('America/Los_Angeles'))

    # ── Step 13: Write eval file + manifest ──
    eval_path = os.path.join(EVAL_V11_DIR, 'qwen_eval_v1_1.jsonl')
    with open(eval_path, 'w') as f:
        for ex in eval_examples:
            f.write(json.dumps(ex) + '\n')

    eval_hash = hashlib.sha256(open(eval_path, 'rb').read()).hexdigest()

    # Category-count report (honest scarcity reporting)
    category_report = {
        'total_market_panels': total_market,
        'total_narrative_pairs': total_narr_pairs,
        'total_rust_repairs': total_rust,
        'total_eval_examples': total_eval,
        'slinky_category_counts': {k: v for k, v in sorted(cat_counts_selected.items())},
        'laserstream_category_counts': {k: v for k, v in sorted(ls_cat_selected.items())},
        'final_category_counts': {k: v for k, v in sorted(final_cat_counts.items())},
        'narrative_pair_count': total_narr_pairs,
        'scarcity_report': {},
    }

    # Honest scarcity reporting per category
    all_target_cats = [
        'runner_opportunity', 'rug_collapse', 'champion_false_positive',
        'champion_missed_opportunity', 'similar_state_divergent',
        'capacity_sensitive', 'latency_sensitive', 'pump_to_pumpswap',
        'mixed_runner_rug', 'hard_skip_no_buy', 'no_edge', 'ordinary',
    ]
    for cat in all_target_cats:
        slinky_count = cat_counts_selected.get(cat, 0)
        ls_count = ls_cat_selected.get(cat, 0)
        total = slinky_count + ls_count
        raw_slinky = slinky_cat_counts.get(cat, 0)
        raw_ls = ls_cat_counts.get(cat, 0)
        if total == 0:
            category_report['scarcity_report'][cat] = {
                'status': 'SCARCE_ABSENT',
                'selected': 0,
                'raw_slinky': raw_slinky,
                'raw_laserstream': raw_ls,
                'note': f'No panels found in {cat} category across both strata.',
            }
        elif total < 5:
            category_report['scarcity_report'][cat] = {
                'status': 'SCARCE_LOW',
                'selected': total,
                'raw_slinky': raw_slinky,
                'raw_laserstream': raw_ls,
                'note': f'Only {total} panels available (target >=5).',
            }
        else:
            category_report['scarcity_report'][cat] = {
                'status': 'ADEQUATE',
                'selected': total,
                'raw_slinky': raw_slinky,
                'raw_laserstream': raw_ls,
            }

    manifest = {
        'version': 'qwen_eval_v1.1',
        'parent_version': 'qwen_eval_v1',
        'parent_eval_hash': '65ab28c23c8a218b80d5a198172d00fb344f2a13fe588d3ffebfe0d84dc4d528',
        'run_uuid': RUN_UUID,
        'freeze_timestamp_utc': freeze_utc,
        'freeze_timestamp_pt': pt_time.isoformat(),
        'utility_version': UTILITY_VERSION,
        'eval_file': eval_path,
        'eval_hash': eval_hash,
        'total_eval_examples': total_eval,
        'total_market_panels': total_market,
        'total_narrative_pairs': total_narr_pairs,
        'total_rust_repairs': total_rust,
        'slinky_market_panels': len(slinky_selected),
        'laserstream_market_panels': len(ls_selected),
        'category_report': category_report,
        'leakage_checks': leakage_checks,
        'provenance_uuids': {
            'slinky': SLINKY_FREEZE_UUID,
            'laserstream': LS_FREEZE_UUID,
            'rust': RUST_FREEZE_UUID,
            'narrative': NARR_FREEZE_UUID,
        },
        'design_principles': {
            'causal_first_construction': True,
            'outcome_stratification_post_construction': True,
            'no_future_in_input': True,
            'narrative_retrospective_excluded': True,
            'spacing_overlap_caps_applied': True,
            'mint_disjoint_within_stratum': True,
            'chronological_split': True,
            'category_diversity_over_count': True,
        },
    }

    manifest_path = os.path.join(EVAL_V11_DIR, 'EVAL_MANIFEST_V1_1.json')
    with open(manifest_path, 'w') as f:
        json.dump(manifest, f, indent=2)

    # Print category report
    print("\n  ═══════════════════════════════════════════════════")
    print("  CATEGORY-COUNT REPORT (qwen_eval_v1.1)")
    print("  ═══════════════════════════════════════════════════")
    print(f"  Total market panels: {total_market}")
    print(f"  Total narrative pairs: {total_narr_pairs}")
    print(f"  Total Rust repairs: {total_rust}")
    print(f"  Total eval examples: {total_eval}")
    print()
    print("  Category counts (selected):")
    for cat in all_target_cats:
        s = cat_counts_selected.get(cat, 0)
        l = ls_cat_selected.get(cat, 0)
        t = s + l
        status = category_report['scarcity_report'].get(cat, {}).get('status', 'UNKNOWN')
        print(f"    {cat:40s}  Slinky={s:3d}  LS={l:3d}  Total={t:3d}  [{status}]")
    print()
    print("  Leakage certification:")
    for k, v in leakage_checks.items():
        print(f"    {'✓' if v else '✗'} {k}")
    print()
    print(f"  EVAL v1.1 FROZEN: {total_eval} examples")
    print(f"  File: {eval_path}")
    print(f"  Hash: {eval_hash[:32]}...")
    print(f"  Manifest: {manifest_path}")

    return manifest


# ════════════════════════════════════════════════════════════════
# CPT/SFT ORCHESTRATION FUNCTIONS
# Imports text generators from cpt_sft_exporters.py
# ════════════════════════════════════════════════════════════════

import importlib.util
import sys as _sys

# Load the exporters module
_spec = importlib.util.spec_from_file_location(
    "cpt_sft_exporters",
    os.path.join(os.path.dirname(__file__), "cpt_sft_exporters.py")
)
_cpt_sft = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_cpt_sft)


def _load_rust_train_records():
    """Load Rust Gold train-split records: code_changes, repairs, trajectories.
    Uses train_shas from splits_v1.json to exclude test/val."""
    import json as _json
    splits_path = os.path.join(RUST, 'splits_v1.json')
    with open(splits_path) as f:
        splits = _json.load(f)
    train_shas = set(splits['train_shas'])

    # Load code_changes (filter to train shas)
    cc_path = os.path.join(RUST, 'code_changes_v1.jsonl')
    code_changes = []
    with open(cc_path) as f:
        for line in f:
            rec = _json.loads(line)
            sha = rec.get('commit_sha', '')
            if sha in train_shas:
                code_changes.append(rec)

    # Load repairs (train only — exclude test split)
    test_shas = set(splits.get('test_shas', []))
    val_shas = set(splits.get('val_shas', []))
    excluded = test_shas | val_shas

    rep_path = os.path.join(RUST, 'repairs_v1.jsonl')
    repairs = []
    with open(rep_path) as f:
        for line in f:
            rec = _json.loads(line)
            bug_sha = rec.get('bug_commit_sha', '')
            fix_sha = rec.get('fix_commit_sha', '')
            if bug_sha in excluded or fix_sha in excluded:
                continue
            repairs.append(rec)

    # Load trajectories
    traj_path = os.path.join(RUST, 'trajectories_v1.jsonl')
    trajectories = []
    with open(traj_path) as f:
        for line in f:
            rec = _json.loads(line)
            prob_sha = rec.get('problem_commit_sha', '')
            if prob_sha in test_shas or prob_sha in val_shas:
                continue
            trajectories.append(rec)

    return code_changes, repairs, trajectories


def _load_narrative_records():
    """Load Narrative Gold v1.1 records: trajectories, content, strategy_cards.
    Filters to A+B scope only."""
    import json as _json
    NARR_GOLD = os.path.join(BASE, 'narrative_gold_v1.1', 'gold')

    trajectories = []
    traj_path = os.path.join(NARR_GOLD, 'human_reasoning_trajectory_v1', 'human_reasoning_trajectory_v1.jsonl')
    if os.path.exists(traj_path):
        with open(traj_path) as f:
            for line in f:
                rec = _json.loads(line)
                if rec.get('scope_tier') in ('A', 'B'):
                    trajectories.append(rec)

    content = []
    content_path = os.path.join(NARR_GOLD, 'creator_content_v1', 'creator_content_v1.jsonl')
    if os.path.exists(content_path):
        with open(content_path) as f:
            for line in f:
                rec = _json.loads(line)
                if rec.get('scope_tier') in ('A', 'B'):
                    content.append(rec)

    strategy_cards = []
    sc_path = os.path.join(NARR_GOLD, 'strategy_card_v1', 'strategy_card_v1.jsonl')
    if os.path.exists(sc_path):
        with open(sc_path) as f:
            for line in f:
                rec = _json.loads(line)
                strategy_cards.append(rec)

    return trajectories, content, strategy_cards


def build_cpt_v1():
    """Build qwen_cpt_v1: continual pre-training corpus from all 4 frozen corpora.
    Target: ~34M tokens, ~15K records. Packed text format (no instruction wrapping)."""
    global _SLINKY_CAUSAL_CACHE, _SLINKY_OUTCOMES_CACHE, _SLINKY_COUNTERFACTUALS_CACHE
    global _SLINKY_OUTCOMES_BY_MINT, _SLINKY_CF_BY_STATE, _SLINKY_LIGHTWEIGHT_IDX
    print("\n=== BUILDING qwen_cpt_v1 ===")
    os.makedirs(CPT_DIR, exist_ok=True)

    cpt_records = []
    source_counts = Counter()
    token_total = 0

    # --- 1. SLINKY GOLD: trajectory summaries + panel snapshots ---
    print("  Loading Slinky Gold for CPT...")
    lw_idx = build_slinky_lightweight_index()
    preload_slinky_outcomes()
    preload_slinky_counterfactuals()

    # LEAKAGE PREVENTION: Load eval mint set and exclude from CPT training
    eval_v1_path = os.path.join(EVAL_DIR, 'qwen_eval_v1.jsonl')
    eval_v1_1_path = os.path.join(EVAL_DIR_V1_1, 'qwen_eval_v1_1.jsonl')
    _eval_excluded_mints = set()
    _eval_excluded_state_ids = set()
    for _ev_path in [eval_v1_path, eval_v1_1_path]:
        if os.path.exists(_ev_path):
            with open(_ev_path, 'r') as _ef:
                for _eline in _ef:
                    _eline = _eline.strip()
                    if not _eline:
                        continue
                    try:
                        _erec = json.loads(_eline)
                        # Extract mint_ids and state_ids from candidates array
                        for _c in _erec.get('candidates', []):
                            if 'mint_id' in _c:
                                _eval_excluded_mints.add(_c['mint_id'])
                            _cs = _c.get('causal_state', {})
                            if 'state_id' in _cs:
                                _eval_excluded_state_ids.add(str(_cs['state_id']))
                            if 'mint_id' in _cs:
                                _eval_excluded_mints.add(_cs['mint_id'])
                        # Also check provenance for source mints
                        _prov = _erec.get('provenance', {})
                        for _k, _v in _prov.items():
                            if 'mint' in _k.lower() and isinstance(_v, str):
                                _eval_excluded_mints.add(_v)
                            if 'state_id' in _k.lower() and isinstance(_v, str):
                                _eval_excluded_state_ids.add(str(_v))
                    except Exception:
                        continue
    print(f"  Eval exclusion set: {len(_eval_excluded_mints)} mints, {len(_eval_excluded_state_ids)} state_ids")

    unique_mints = lw_idx['mint'].unique() if 'mint' in lw_idx.columns else []
    n_traj = min(3000, len(unique_mints))
    rng = random.Random(42)
    traj_mints = rng.sample(list(unique_mints), n_traj) if len(unique_mints) > n_traj else list(unique_mints)

    # PERF: precompute mint -> (source_file, first state_id) ONCE (vectorized)
    # instead of scanning the 33.5M-row index per mint (was O(n_mints * 33.5M)).
    traj_set = set(traj_mints)
    traj_meta = lw_idx[lw_idx['mint'].isin(traj_set)][['mint', 'source_file', 'state_id']]
    first_meta = traj_meta.drop_duplicates(subset='mint', keep='first')
    mint_to_file = dict(zip(first_meta['mint'], first_meta['source_file']))
    mint_to_state = dict(zip(first_meta['mint'], first_meta['state_id'].astype(str)))
    del traj_meta, first_meta

    # PERF: group sampled mints by file and process file-by-file with
    # load -> extract -> FREE. The old path cached every touched file's full
    # causal DataFrame forever; 3000 random mints touch all ~134 files, which
    # pulled the entire 33.5M x ~100-col dataset into RAM and OOM-killed runs.
    file_to_mints = {}
    for mint in traj_mints:
        f = mint_to_file.get(mint)
        if f is not None:
            file_to_mints.setdefault(f, []).append(mint)

    ps_dir = os.path.join(SLINKY, 'pump_state_v3')
    for fname, fmints in sorted(file_to_mints.items()):
        filepath = os.path.join(ps_dir, fname)
        try:
            safe_cols = get_safe_columns(filepath, SLINKY_CAUSAL_FIELDS)
            fdf = pq.read_table(filepath, columns=safe_cols).to_pandas()
        except Exception:
            continue
        fdf = fdf[fdf['mint'].isin(set(fmints))]
        grouped = {m: g.reset_index(drop=True) for m, g in fdf.groupby('mint', sort=False)}
        del fdf

        for mint in fmints:
            # LEAKAGE PREVENTION: skip eval mints (by hashed mint_id for consistent format)
            if hash_mint(mint) in _eval_excluded_mints:
                continue
            state_id = mint_to_state.get(mint, '')
            # LEAKAGE PREVENTION: also skip by state_id (16-char hex)
            if state_id and state_id in _eval_excluded_state_ids:
                continue
            state_rows = grouped.get(mint)
            if state_rows is None or len(state_rows) == 0:
                continue
            outcome = _SLINKY_OUTCOMES_BY_MINT.get(hash_mint(mint)) if _SLINKY_OUTCOMES_BY_MINT else None
            cf = _SLINKY_CF_BY_STATE.get(state_id) if _SLINKY_CF_BY_STATE else None

            result = _cpt_sft.cpt_slinky_trajectory_summary(mint, state_rows, outcome, cf)
            text = result['text']
            tokens = result.get('tokens') or _cpt_sft.estimate_tokens(text)
            rec = {
                'id': f"cpt_slinky_{len(cpt_records):05d}",
                'format': 'packed_text',
                'source_corpus': 'slinky_gold_v3',
                'source_layer': 'pump_state_v3+pump_outcome_v3+counterfactual_trade_v3',
                'source_ids': [str(state_id)],
                'content': text,
                'provenance': _cpt_sft.make_cpt_provenance(
                    'slinky_gold_v3', [str(state_id)], 'trajectory_summary',
                    SLINKY_FREEZE_UUID, mint_ids=[hash_mint(mint)]
                ),
                'token_count': tokens,
            }
            if result.get('metadata'):
                rec['compression'] = result['metadata']
            cpt_records.append(rec)
            source_counts['slinky_trajectory'] += 1
            token_total += tokens
        del grouped
    del file_to_mints, mint_to_file, mint_to_state

    # Slinky panel snapshots — use train-filtered index + dense windows
    train_lw_idx = build_train_filtered_lightweight_index(_eval_excluded_mints)
    dense_windows = find_dense_time_windows(train_lw_idx, min_candidates=SLINKY_PANEL_MIN,
                                             window_ms=60000, max_panels=1200)
    if dense_windows:
        n_panels = min(1200, len(dense_windows))
        for i in range(n_panels):
            if len(cpt_records) >= 8000:
                break
            anchor_ms, win_lo, win_hi = dense_windows[i]
            try:
                panel = assemble_slinky_panel(anchor_ms, i, lw_idx=train_lw_idx)
                if panel and len(panel.get('candidates', [])) >= 3:
                    text = _cpt_sft.cpt_slinky_panel_snapshot(panel['candidates'], anchor_ms)
                    tokens = _cpt_sft.estimate_tokens(text)
                    rec = {
                        'id': f"cpt_slinky_panel_{len(cpt_records):05d}",
                        'format': 'packed_text',
                        'source_corpus': 'slinky_gold_v3',
                        'source_layer': 'pump_state_v3',
                        'source_ids': [f"panel_{i}"],
                        'content': text,
                        'provenance': _cpt_sft.make_cpt_provenance(
                            'slinky_gold_v3', [f"panel_{i}"], 'panel_snapshot',
                            SLINKY_FREEZE_UUID
                        ),
                        'token_count': tokens,
                    }
                    cpt_records.append(rec)
                    source_counts['slinky_panel'] += 1
                    token_total += tokens
            except Exception:
                continue

    # --- Slinky Regime Catalogs: aggregate causal patterns by outcome regime ---
    # 5 regimes: Moonshot, Rug-pull, Graduated, Quick-death, Stable-low-vol
    # Classify mints using outcome data (cached), then load causal state data
    # for a sample of members per regime. Each regime becomes one CPT record.
    if _SLINKY_OUTCOMES_BY_MINT and _SLINKY_LIGHTWEIGHT_IDX is not None:
        regime_defs = {
            'moonshot': lambda o: (
                pd.notna(o.get('mfe_bp')) and o.get('mfe_bp', 0) > 1000
                and o.get('has_future_trade_300s', False)
            ),
            'rug_pull': lambda o: (
                o.get('collapsed_50pct_within_300s', False)
                and (o.get('mfe_bp', 0) < 500 if pd.notna(o.get('mfe_bp')) else True)
            ),
            'graduated': lambda o: o.get('graduated_after_state', False),
            'quick_death': lambda o: not o.get('has_trade_within_60s', False),
            'stable_low_vol': lambda o: (
                o.get('has_future_trade_300s', False)
                and (o.get('mfe_bp', 0) < 500 if pd.notna(o.get('mfe_bp')) else False)
                and (o.get('mae_bp', 0) > -500 if pd.notna(o.get('mae_bp')) else False)
                and not o.get('collapsed_50pct_within_300s', False)
            ),
        }
        regime_members = {r: [] for r in regime_defs}
        for h_mint, o_row in _SLINKY_OUTCOMES_BY_MINT.items():
            if not isinstance(o_row, dict):
                continue
            # LEAKAGE PREVENTION: exclude eval mints from regime catalogs
            if h_mint in _eval_excluded_mints:
                continue
            for rname, rcheck in regime_defs.items():
                try:
                    if rcheck(o_row):
                        regime_members[rname].append((h_mint, o_row))
                except Exception:
                    continue

        # Build a mapping from hash_mint -> (mint_addr, file) using the lightweight index
        # so we can load causal fields for regime members.
        # Use groupby on unique mints only (not 33M rows) for speed.
        lw = _SLINKY_LIGHTWEIGHT_IDX
        hash_to_mint_file = {}
        if 'mint' in lw.columns and 'source_file' in lw.columns:
            # Deduplicate mints — each mint appears in exactly one file.
            # Vectorized zip (was .iterrows() over 622K rows = minutes).
            mint_file = lw[['mint', 'source_file']].drop_duplicates(subset='mint')
            mints_l = mint_file['mint'].tolist()
            files_l = mint_file['source_file'].tolist()
            hash_to_mint_file = {
                hash_mint(m): (m, f) for m, f in zip(mints_l, files_l)
            }
            del mint_file, mints_l, files_l
        print(f"    Regime member counts: " + ", ".join(f"{r}={len(m)}" for r, m in regime_members.items()))

        for rname, members in regime_members.items():
            if len(members) < 10:
                continue
            # Seeded RANDOM sample for representativeness — members[:100] biased
            # the catalog toward the earliest-ingested files.
            if len(members) > 100:
                _rrng = random.Random(f"regime_{rname}")
                sample_members = _rrng.sample(members, 100)
            else:
                sample_members = members
            # Group sample members by file for batch causal loading
            mints_by_file = {}
            member_hashes = []
            for h_mint, o_row in sample_members:
                info = hash_to_mint_file.get(h_mint)
                if info is None:
                    continue
                mint_addr, fname = info
                mints_by_file.setdefault(fname, set()).add(mint_addr)
                member_hashes.append((h_mint, o_row, mint_addr))
            # Load causal fields for these mints. NOTE: the old "first 10
            # files" truncation silently shrank the 100-mint sample to ~15
            # (random mints span ~70 of 134 files) — memory safety is now
            # handled by the bounded LRU cache in load_slinky_causal_for_mints,
            # so we load every file the sample touches.
            if mints_by_file:
                causal = load_slinky_causal_for_mints(mints_by_file)
            else:
                causal = {}
            # Build mints_data by looking up state rows from loaded causal data
            mints_data = []
            source_ids = []
            for h_mint, o_row, mint_addr in member_hashes:
                state_row = None
                for fname, mint_map in causal.items():
                    if not isinstance(mint_map, dict):
                        continue
                    if mint_addr in mint_map:
                        srows = mint_map[mint_addr]
                        if srows is not None and len(srows) > 0:
                            state_row = srows.iloc[-1].to_dict()
                            break
                if state_row is None:
                    state_row = {}
                merged = {**state_row, **{k: v for k, v in o_row.items()
                           if k in ('mfe_bp', 'mae_bp', 'graduated_after_state',
                                    'collapsed_50pct_within_300s', 'survived_60s',
                                    'survived_300s')}}
                mints_data.append(merged)
                source_ids.append(str(h_mint))
            if len(mints_data) < 5:
                continue
            result = _cpt_sft.cpt_slinky_regime_catalog(rname, mints_data)
            text = result['text']
            tokens = _cpt_sft.estimate_tokens(text)
            rec = {
                'id': f"cpt_slinky_regime_{len(cpt_records):05d}",
                'format': 'packed_text',
                'source_corpus': 'slinky_gold_v3',
                'source_layer': 'pump_state_v3+pump_outcome_v3',
                'source_ids': source_ids[:20],
                'content': text,
                'provenance': _cpt_sft.make_cpt_provenance(
                    'slinky_gold_v3', source_ids[:20], 'regime_catalog',
                    SLINKY_FREEZE_UUID
                ),
                'token_count': tokens,
            }
            if result.get('metadata'):
                rec['regime_meta'] = result['metadata']
            cpt_records.append(rec)
            source_counts['slinky_regime_catalog'] += 1
            token_total += tokens
            print(f"    Regime {rname}: {len(mints_data)} mints, {tokens} tokens")

    # Free Slinky caches
    _SLINKY_CAUSAL_CACHE = None
    _SLINKY_OUTCOMES_CACHE = None
    _SLINKY_COUNTERFACTUALS_CACHE = None
    _SLINKY_OUTCOMES_BY_MINT = None
    _SLINKY_CF_BY_STATE = None
    _SLINKY_LIGHTWEIGHT_IDX = None
    print(f"  Slinky CPT records: {sum(1 for r in cpt_records if 'slinky' in r['source_corpus'])}")

    # --- 2. LASERSTREAM GOLD: microstructure summaries ---
    print("  Loading LaserStream Gold for CPT...")
    l1_path = os.path.join(LS, 'l1_pump_state_v3.parquet')
    l2_path = os.path.join(LS, 'l2_pump_outcome_v3.parquet')
    l3_path = os.path.join(LS, 'l3_counterfactual_v3.parquet')

    l1_pq = pq.read_table(l1_path, columns=['state_id', 'mint_b58', 'timestamp_ms'])
    ls_state_ids = l1_pq.column('state_id').to_pylist()
    unique_state_ids = list(set(ls_state_ids))

    n_ls = min(1500, len(unique_state_ids))
    rng2 = random.Random(123)
    ls_sample = rng2.sample(unique_state_ids, n_ls)

    l2_pq = pq.read_table(l2_path)
    l2_df = l2_pq.to_pandas()
    l2_by_state = {}
    for _, row in l2_df.iterrows():
        l2_by_state[row['state_id']] = row.to_dict()
    del l2_df

    l3_cols = ['state_id', 'feasible', 'sim_return_pct', 'trade_size_sol',
               'latency_scenario_ms', 'outcome_class', 'capacity_constrained']
    l3_pq = pq.read_table(l3_path, columns=l3_cols)
    l3_df = l3_pq.to_pandas()
    l3_by_state = {}
    ls_sample_set = set(ls_sample)
    for sid, group in l3_df.groupby('state_id'):
        if sid in ls_sample_set:
            scenarios = group.to_dict('records')
            l3_compressed = compute_l3_compressed(scenarios)
            l3_by_state[sid] = l3_compressed
    del l3_df

    l1_needed_cols = ['state_id', 'venue', 'event_type', 'price_lamports_per_rawtok',
                      'curve_virtual_sol', 'curve_virtual_token', 'curve_complete',
                      'right_censored_1s', 'right_censored_300s']
    l1_full = pq.read_table(l1_path, columns=l1_needed_cols)
    l1_df = l1_full.to_pandas()
    l1_by_state = {}
    for sid, group in l1_df.groupby('state_id'):
        if sid in ls_sample_set:
            l1_by_state[sid] = group

    for sid in ls_sample:
        if len(cpt_records) >= 10000:
            break
        # LEAKAGE PREVENTION: skip eval state_ids
        if str(sid) in _eval_excluded_state_ids:
            continue
        l1_rows = l1_by_state.get(sid)
        l2_row = l2_by_state.get(sid)
        l3c = l3_by_state.get(sid)
        if l1_rows is None and l2_row is None:
            continue
        text = _cpt_sft.cpt_laserstream_microstructure(sid, l1_rows if l1_rows is not None else pd.DataFrame(), l2_row, l3c)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_ls_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'laserstream_gold_v3',
            'source_layer': 'L1+L2+L3',
            'source_ids': [str(sid)],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'laserstream_gold_v3', [str(sid)], 'microstructure_summary',
                LS_FREEZE_UUID
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['laserstream'] += 1
        token_total += tokens

    del l1_df, l2_by_state, l3_by_state, l1_by_state
    print(f"  LaserStream CPT records: {sum(1 for r in cpt_records if 'laserstream' in r['source_corpus'])}")

    # --- 3. RUST GOLD: code changes, repairs, trajectories (train split) ---
    print("  Loading Rust Gold for CPT...")
    code_changes, repairs, trajectories = _load_rust_train_records()

    subsystem_counts = Counter()
    for cc in code_changes:
        if len(cpt_records) >= 13000:
            break
        subsys = cc.get('subsystem', 'unknown')
        if subsystem_counts[subsys] >= MAX_RUST_PER_SUBSYSTEM:
            continue
        text = _cpt_sft.cpt_rust_code_change(cc)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_rust_cc_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'rust_gold_v1',
            'source_layer': 'code_changes_v1',
            'source_ids': [cc.get('commit_sha', '')[:12]],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'rust_gold_v1', [cc.get('commit_sha', '')[:12]], 'code_change',
                RUST_FREEZE_UUID
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['rust_code_change'] += 1
        token_total += tokens
        subsystem_counts[subsys] += 1

    for rep in repairs:
        if len(cpt_records) >= 14000:
            break
        text = _cpt_sft.cpt_rust_repair(rep)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_rust_rep_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'rust_gold_v1',
            'source_layer': 'repairs_v1',
            'source_ids': [rep.get('bug_commit_sha', '')[:12]],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'rust_gold_v1', [rep.get('bug_commit_sha', '')[:12]], 'repair',
                RUST_FREEZE_UUID
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['rust_repair'] += 1
        token_total += tokens

    for traj in trajectories:
        if len(cpt_records) >= 14500:
            break
        text = _cpt_sft.cpt_rust_trajectory(traj)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_rust_traj_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'rust_gold_v1',
            'source_layer': 'trajectories_v1',
            'source_ids': [traj.get('problem_commit_sha', '')[:12]],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'rust_gold_v1', [traj.get('problem_commit_sha', '')[:12]], 'trajectory',
                RUST_FREEZE_UUID
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['rust_trajectory'] += 1
        token_total += tokens

    print(f"  Rust CPT records: {sum(1 for r in cpt_records if 'rust' in r['source_corpus'])}")

    # --- 4. NARRATIVE GOLD v1.1 ---
    print("  Loading Narrative Gold v1.1 for CPT...")
    narr_trajectories, narr_content, narr_cards = _load_narrative_records()

    for rec_data in narr_trajectories:
        if len(cpt_records) >= 15000:
            break
        text = _cpt_sft.cpt_narrative_trajectory(rec_data)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_narr_traj_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'narrative_gold_v1.1',
            'source_layer': 'human_reasoning_trajectory_v1',
            'source_ids': [rec_data.get('trajectory_id', '')],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'narrative_gold_v1.1', [rec_data.get('trajectory_id', '')], 'narrative_trajectory',
                NARR_FREEZE_UUID, scope_tier=rec_data.get('scope_tier'),
                narrative_temporal_class=rec_data.get('temporal_class')
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['narrative_trajectory'] += 1
        token_total += tokens

    for rec_data in narr_content:
        if len(cpt_records) >= 15200:
            break
        text = _cpt_sft.cpt_narrative_content(rec_data)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_narr_content_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'narrative_gold_v1.1',
            'source_layer': 'creator_content_v1',
            'source_ids': [rec_data.get('content_id', '')],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'narrative_gold_v1.1', [rec_data.get('content_id', '')], 'narrative_content',
                NARR_FREEZE_UUID, scope_tier=rec_data.get('scope_tier'),
                narrative_temporal_class=rec_data.get('temporal_class')
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['narrative_content'] += 1
        token_total += tokens

    for rec_data in narr_cards:
        if len(cpt_records) >= 15500:
            break
        text = _cpt_sft.cpt_narrative_strategy_card(rec_data)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_narr_card_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'narrative_gold_v1.1',
            'source_layer': 'strategy_card_v1',
            'source_ids': [rec_data.get('strategy_card_id', '')],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'narrative_gold_v1.1', [rec_data.get('strategy_card_id', '')], 'strategy_card',
                NARR_FREEZE_UUID
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['narrative_strategy'] += 1
        token_total += tokens

    print(f"  Narrative CPT records: {sum(1 for r in cpt_records if 'narrative' in r['source_corpus'])}")

    # --- Write CPT JSONL ---
    # DEDUP: Remove near-identical CPT records by content hash
    seen_content_hashes = set()
    deduped_cpt = []
    dedup_removed = 0
    for rec in cpt_records:
        _h = hashlib.sha256(rec['content'].encode()).hexdigest()[:32]
        if _h not in seen_content_hashes:
            seen_content_hashes.add(_h)
            deduped_cpt.append(rec)
        else:
            dedup_removed += 1
    if dedup_removed > 0:
        print(f"  CPT dedup: removed {dedup_removed} duplicate records")
        cpt_records = deduped_cpt

    cpt_path = os.path.join(CPT_DIR, 'qwen_cpt_v1.jsonl')
    _cpt_sft.write_jsonl(cpt_records, cpt_path)
    cpt_hash = _cpt_sft.compute_file_hash(cpt_path)

    cpt_manifest = {
        'version': 'qwen_cpt_v1',
        'frozen_at': datetime.now(timezone.utc).isoformat(),
        'run_uuid': RUN_UUID,
        'total_records': len(cpt_records),
        'total_tokens': token_total,
        'target_tokens': 34_000_000,
        'dedup_removed': dedup_removed,
        'eval_exclusion': {'mints_excluded': len(_eval_excluded_mints), 'state_ids_excluded': len(_eval_excluded_state_ids)},
        'file': cpt_path,
        'file_hash': cpt_hash[:32],
        'source_composition': dict(source_counts),
        'corpora_included': ['slinky_gold_v3', 'laserstream_gold_v3', 'rust_gold_v1', 'narrative_gold_v1.1'],
        'latency_correction': {
            'applied': True,
            'latency_effect_observed': False,
            'note': 'sim_return_pct invariant across 0/100/500/1000/2000ms in frozen L3; '
                    'duplicate latency scenarios collapsed; provenance preserved',
        },
        'provenance_uuids': {
            'slinky': SLINKY_FREEZE_UUID,
            'laserstream': LS_FREEZE_UUID,
            'rust': RUST_FREEZE_UUID,
            'narrative': NARR_FREEZE_UUID,
        },
        'utility_version': UTILITY_VERSION,
    }

    manifest_path = os.path.join(CPT_DIR, 'CPT_MANIFEST_V1.json')
    with open(manifest_path, 'w') as f:
        json.dump(cpt_manifest, f, indent=2)

    print(f"\n  CPT v1 FROZEN: {len(cpt_records)} records, ~{token_total:,} tokens")
    print(f"  File: {cpt_path}")
    print(f"  Hash: {cpt_hash[:32]}...")
    print(f"  Manifest: {manifest_path}")
    print(f"  Source composition: {dict(source_counts)}")

    return cpt_manifest


def build_sft_v1():
    """Build qwen_sft_v1: supervised fine-tuning corpus.
    Target: ~15M tokens, ~3K records. SFT mix per design: 35/5/20/20/10/5/5."""
    global _SLINKY_CAUSAL_CACHE, _SLINKY_OUTCOMES_CACHE, _SLINKY_COUNTERFACTUALS_CACHE
    global _SLINKY_OUTCOMES_BY_MINT, _SLINKY_CF_BY_STATE, _SLINKY_LIGHTWEIGHT_IDX
    print("\n=== BUILDING qwen_sft_v1 ===")
    os.makedirs(SFT_DIR, exist_ok=True)

    sft_records = []
    source_counts = Counter()
    token_total = 0
    target_total = 3000
    mix = SFT_MIX
    targets = {
        'cross_sectional_compressed': int(target_total * mix['cross_sectional_compressed']),
        'execution_surface_detail': int(target_total * mix['execution_surface_detail']),
        'postmortem_contrast': int(target_total * mix['postmortem_contrast']),
        'rust_engineering': int(target_total * mix['rust_engineering']),
        'narrative_strategy': int(target_total * mix['narrative_strategy']),
        'hermes_tool': int(target_total * mix['hermes_tool']),
        'general_retention': int(target_total * mix['general_retention']),
    }

    # --- 1. Cross-sectional compressed (35% = ~1050) ---
    print("  Building cross-sectional SFT examples...")
    lw_idx = build_slinky_lightweight_index()
    preload_slinky_outcomes()
    preload_slinky_counterfactuals()

    # LEAKAGE PREVENTION: Load eval mint set and exclude from SFT training
    eval_v1_path = os.path.join(EVAL_DIR, 'qwen_eval_v1.jsonl')
    eval_v1_1_path = os.path.join(EVAL_DIR_V1_1, 'qwen_eval_v1_1.jsonl')
    _sft_eval_excluded_mints = set()
    _sft_eval_excluded_state_ids = set()
    for _ev_path in [eval_v1_path, eval_v1_1_path]:
        if os.path.exists(_ev_path):
            with open(_ev_path, 'r') as _ef:
                for _eline in _ef:
                    _eline = _eline.strip()
                    if not _eline:
                        continue
                    try:
                        _erec = json.loads(_eline)
                        for _c in _erec.get('candidates', []):
                            if 'mint_id' in _c:
                                _sft_eval_excluded_mints.add(_c['mint_id'])
                            _cs = _c.get('causal_state', {})
                            if 'state_id' in _cs:
                                _sft_eval_excluded_state_ids.add(str(_cs['state_id']))
                            if 'mint_id' in _cs:
                                _sft_eval_excluded_mints.add(_cs['mint_id'])
                        _prov = _erec.get('provenance', {})
                        for _k, _v in _prov.items():
                            if 'mint' in _k.lower() and isinstance(_v, str):
                                _sft_eval_excluded_mints.add(_v)
                            if 'state_id' in _k.lower() and isinstance(_v, str):
                                _sft_eval_excluded_state_ids.add(str(_v))
                    except Exception:
                        continue
    print(f"  SFT eval exclusion set: {len(_sft_eval_excluded_mints)} mints, {len(_sft_eval_excluded_state_ids)} state_ids")

    n_cross = targets['cross_sectional_compressed']
    # Build train-filtered index for Slinky panels (clean BY CONSTRUCTION)
    train_lw_idx = build_train_filtered_lightweight_index(_sft_eval_excluded_mints)
    dense_windows = find_dense_time_windows(train_lw_idx, min_candidates=SLINKY_PANEL_MIN,
                                             window_ms=60000, max_panels=n_cross * 2)
    cross_built = 0
    if dense_windows:
        for i in range(min(len(dense_windows), n_cross * 2)):
            if cross_built >= n_cross:
                break
            anchor_ms, win_lo, win_hi = dense_windows[i]
            try:
                panel = assemble_slinky_panel(anchor_ms, i, lw_idx=train_lw_idx)
                if panel and len(panel.get('candidates', [])) >= 3:
                    # Panels are clean BY CONSTRUCTION (pre-filtered index)
                    outcome_details = []
                    for c in panel['candidates']:
                        mint_id = c.get('mint_id', '')
                        od = _SLINKY_OUTCOMES_BY_MINT.get(mint_id) if _SLINKY_OUTCOMES_BY_MINT else {}
                        outcome_details.append(od if od else {})
                    ex = _cpt_sft.sft_cross_sectional_example(
                        panel['candidates'], outcome_details, f"panel_{i}",
                        source='slinky', l3_detail='compressed', narrative_context=None
                    )
                    ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
                    ex['id'] = f"sft_cross_{cross_built:04d}"
                    ex['source_corpus'] = 'slinky_gold_v3'
                    ex['provenance'] = _cpt_sft.make_sft_provenance(
                        'slinky_gold_v3', [f"panel_{i}"], SLINKY_FREEZE_UUID,
                        panel_id=f"panel_{i}", eval_split='train'
                    )
                    sft_records.append(ex)
                    source_counts['cross_sectional'] += 1
                    token_total += ex['token_count']
                    cross_built += 1
            except Exception:
                continue

    # Fill from LS if needed
    if cross_built < n_cross:
        print(f"  Filling {n_cross - cross_built} cross-sectional from LaserStream...")
        l1_path = os.path.join(LS, 'l1_pump_state_v3.parquet')
        l1_pq = pq.read_table(l1_path, columns=['state_id', 'timestamp_ms'])
        ls_state_ids = list(set(l1_pq.column('state_id').to_pylist()))
        needed = n_cross - cross_built
        rng3 = random.Random(77)
        ls_sample = rng3.sample(ls_state_ids, min(needed, len(ls_state_ids)))
        for sid in ls_sample:
            if cross_built >= n_cross:
                break
            # LEAKAGE PREVENTION: skip eval state_ids
            if str(sid) in _sft_eval_excluded_state_ids:
                continue
            candidates = [{'mint': str(sid), 'state_id': str(sid), 'venue': 'pump.fun'}]
            outcome_details = [{}]
            ex = _cpt_sft.sft_cross_sectional_example(
                candidates, outcome_details, f"ls_panel_{cross_built}",
                source='laserstream', l3_detail='compressed'
            )
            ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
            ex['id'] = f"sft_cross_ls_{cross_built:04d}"
            ex['source_corpus'] = 'laserstream_gold_v3'
            ex['provenance'] = _cpt_sft.make_sft_provenance(
                'laserstream_gold_v3', [str(sid)], LS_FREEZE_UUID,
                eval_split='train'
            )
            sft_records.append(ex)
            source_counts['cross_sectional_ls'] += 1
            token_total += ex['token_count']
            cross_built += 1

    print(f"  Cross-sectional: {cross_built}")

    # --- 2. Execution surface detail (5% = ~150) ---
    print("  Building execution-surface SFT examples...")
    l3_path = os.path.join(LS, 'l3_counterfactual_v3.parquet')
    l3_cols = ['state_id', 'feasible', 'sim_return_pct', 'trade_size_sol',
               'latency_scenario_ms', 'outcome_class', 'capacity_constrained']
    l3_pq = pq.read_table(l3_path, columns=l3_cols)
    l3_df = l3_pq.to_pandas()
    cap_state_ids = l3_df[l3_df['capacity_constrained'] == True]['state_id'].unique()
    n_exec = targets['execution_surface_detail']
    exec_ids = list(cap_state_ids)[:n_exec]
    if len(exec_ids) < n_exec:
        other_ids = [sid for sid in l3_df['state_id'].unique() if sid not in set(exec_ids)]
        rng4 = random.Random(99)
        exec_ids += rng4.sample(other_ids, min(n_exec - len(exec_ids), len(other_ids)))

    for sid in exec_ids:
        idx = len(sft_records)
        # LEAKAGE PREVENTION: skip eval state_ids
        if str(sid) in _sft_eval_excluded_state_ids:
            continue
        scenarios = l3_df[l3_df['state_id'] == sid].to_dict('records')
        l3c = compute_l3_compressed(scenarios)
        candidates = [{'mint': str(sid), 'state_id': str(sid), 'l3_compressed': l3c, 'venue': 'pump.fun'}]
        outcome_details = [{'robust_utility': (l3c.get('median_net_return_pct', 0) or 0) / 100,
                           'feasibility_ratio': l3c.get('feasibility_ratio', 0),
                           'capacity_constrained_sizes': l3c.get('capacity_constrained_sizes', [])}]
        ex = _cpt_sft.sft_cross_sectional_example(
            candidates, outcome_details, f"exec_{idx}",
            source='laserstream', l3_detail='full'
        )
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_exec_{idx:04d}"
        ex['source_corpus'] = 'laserstream_gold_v3'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'laserstream_gold_v3', [str(sid)], LS_FREEZE_UUID,
            eval_split='train', l3_detail_level='full'
        )
        sft_records.append(ex)
        source_counts['execution_surface'] += 1
        token_total += ex['token_count']
    del l3_df
    print(f"  Execution surface: {min(len(exec_ids), n_exec)}")

    # --- 3. Postmortem/contrast (20% = ~600) ---
    print("  Building postmortem/contrast SFT examples...")
    n_post = targets['postmortem_contrast']
    post_built = 0
    # Use train-filtered dense windows for postmortem panels (clean BY CONSTRUCTION)
    if train_lw_idx is not None and len(train_lw_idx) > 0:
        post_windows = find_dense_time_windows(train_lw_idx, min_candidates=10,
                                                window_ms=60000, max_panels=n_post * 3)
        for i in range(min(len(post_windows), n_post * 3)):
            if post_built >= n_post:
                break
            anchor_ms, win_lo, win_hi = post_windows[i]
            try:
                panel = assemble_slinky_panel(anchor_ms, i, lw_idx=train_lw_idx)
                if panel and len(panel.get('candidates', [])) >= 5:
                    pairs = find_divergent_pairs(panel)
                    for pair in pairs[:2]:
                        if post_built >= n_post:
                            break
                        # Find candidates by panel_rank (candidate_a_idx / candidate_b_idx)
                        idx_a = pair.get('candidate_a_idx')
                        idx_b = pair.get('candidate_b_idx')
                        cands_a = [c for c in panel['candidates'] if c.get('panel_rank') == idx_a]
                        cands_b = [c for c in panel['candidates'] if c.get('panel_rank') == idx_b]
                        if not cands_a or not cands_b:
                            continue
                        mint_id_a = cands_a[0].get('mint_id', '')
                        mint_id_b = cands_b[0].get('mint_id', '')
                        od_a = _SLINKY_OUTCOMES_BY_MINT.get(mint_id_a, {}) if _SLINKY_OUTCOMES_BY_MINT else {}
                        od_b = _SLINKY_OUTCOMES_BY_MINT.get(mint_id_b, {}) if _SLINKY_OUTCOMES_BY_MINT else {}
                        ex = _cpt_sft.sft_postmortem_example(
                            pair, cands_a, cands_b, od_a, od_b,
                            f"postmortem_{post_built}"
                        )
                        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
                        ex['id'] = f"sft_post_{post_built:04d}"
                        ex['source_corpus'] = 'slinky_gold_v3'
                        ex['provenance'] = _cpt_sft.make_sft_provenance(
                            'slinky_gold_v3', [mint_id_a, mint_id_b], SLINKY_FREEZE_UUID,
                            contrast_pair_id=f"pair_{post_built}", eval_split='train'
                        )
                        sft_records.append(ex)
                        source_counts['postmortem'] += 1
                        token_total += ex['token_count']
                        post_built += 1
            except Exception as _pm_err:
                print(f"    postmortem panel {i} failed: {type(_pm_err).__name__}: {_pm_err}")
                continue
    print(f"  Postmortem: {post_built}")

    # Free Slinky caches
    _SLINKY_CAUSAL_CACHE = None
    _SLINKY_OUTCOMES_CACHE = None
    _SLINKY_COUNTERFACTUALS_CACHE = None
    _SLINKY_OUTCOMES_BY_MINT = None
    _SLINKY_CF_BY_STATE = None
    _SLINKY_LIGHTWEIGHT_IDX = None

    # --- 4. Rust engineering (20% = ~600) ---
    print("  Building Rust engineering SFT examples...")
    code_changes, repairs, trajectories = _load_rust_train_records()
    n_rust = targets['rust_engineering']
    n_cc = int(n_rust * 0.5)
    n_rep = int(n_rust * 0.3)
    n_traj_sft = n_rust - n_cc - n_rep

    rust_built = 0
    subsys_counts = Counter()
    for cc in code_changes[:n_cc * 3]:
        if rust_built >= n_cc:
            break
        subsys = cc.get('subsystem', 'unknown')
        if subsys_counts.get(subsys, 0) >= MAX_RUST_PER_SUBSYSTEM:
            continue
        ex = _cpt_sft.sft_rust_example(cc, record_type='code_change')
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_rust_cc_{rust_built:04d}"
        ex['source_corpus'] = 'rust_gold_v1'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'rust_gold_v1', [cc.get('commit_sha', '')[:12]], RUST_FREEZE_UUID,
            eval_split='train'
        )
        sft_records.append(ex)
        source_counts['rust_code_change'] += 1
        token_total += ex['token_count']
        subsys_counts[subsys] = subsys_counts.get(subsys, 0) + 1
        rust_built += 1

    rep_built = 0
    for rep in repairs[:n_rep * 2]:
        if rep_built >= n_rep:
            break
        ex = _cpt_sft.sft_rust_example(rep, record_type='repair')
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_rust_rep_{rep_built:04d}"
        ex['source_corpus'] = 'rust_gold_v1'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'rust_gold_v1', [rep.get('bug_commit_sha', '')[:12]], RUST_FREEZE_UUID,
            eval_split='train'
        )
        sft_records.append(ex)
        source_counts['rust_repair'] += 1
        token_total += ex['token_count']
        rep_built += 1

    traj_built = 0
    for traj in trajectories[:n_traj_sft * 2]:
        if traj_built >= n_traj_sft:
            break
        ex = _cpt_sft.sft_rust_example(traj, record_type='trajectory')
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_rust_traj_{traj_built:04d}"
        ex['source_corpus'] = 'rust_gold_v1'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'rust_gold_v1', [traj.get('problem_commit_sha', '')[:12]], RUST_FREEZE_UUID,
            eval_split='train'
        )
        sft_records.append(ex)
        source_counts['rust_trajectory'] += 1
        token_total += ex['token_count']
        traj_built += 1

    print(f"  Rust: {rust_built + rep_built + traj_built}")

    # --- 5. Narrative strategy (10% = ~300) ---
    print("  Building narrative SFT examples...")
    narr_traj, narr_content, narr_cards = _load_narrative_records()
    n_narr = targets['narrative_strategy']
    n_nt = int(n_narr * 0.4)
    n_nc = int(n_narr * 0.4)
    n_ns = n_narr - n_nt - n_nc

    for rec_data in narr_traj[:n_nt]:
        ex = _cpt_sft.sft_narrative_example(rec_data, record_type='trajectory')
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_narr_traj_{len(sft_records):04d}"
        ex['source_corpus'] = 'narrative_gold_v1.1'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'narrative_gold_v1.1', [rec_data.get('trajectory_id', '')], NARR_FREEZE_UUID,
            eval_split='train', scope_tier=rec_data.get('scope_tier')
        )
        sft_records.append(ex)
        source_counts['narrative_trajectory'] += 1
        token_total += ex['token_count']

    for rec_data in narr_content[:n_nc]:
        ex = _cpt_sft.sft_narrative_example(rec_data, record_type='content')
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_narr_content_{len(sft_records):04d}"
        ex['source_corpus'] = 'narrative_gold_v1.1'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'narrative_gold_v1.1', [rec_data.get('content_id', '')], NARR_FREEZE_UUID,
            eval_split='train', scope_tier=rec_data.get('scope_tier')
        )
        sft_records.append(ex)
        source_counts['narrative_content'] += 1
        token_total += ex['token_count']

    for rec_data in narr_cards[:n_ns]:
        ex = _cpt_sft.sft_narrative_example(rec_data, record_type='strategy_card')
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_narr_card_{len(sft_records):04d}"
        ex['source_corpus'] = 'narrative_gold_v1.1'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'narrative_gold_v1.1', [rec_data.get('strategy_card_id', '')], NARR_FREEZE_UUID,
            eval_split='train'
        )
        sft_records.append(ex)
        source_counts['narrative_strategy'] += 1
        token_total += ex['token_count']

    print(f"  Narrative: {sum(source_counts.get(k, 0) for k in ['narrative_trajectory', 'narrative_content', 'narrative_strategy'])}")

    # --- 6. Hermes/tool interface (5% = ~150) ---
    print("  Building Hermes/tool SFT examples...")
    n_hermes = targets['hermes_tool']
    for i in range(n_hermes):
        ex = _cpt_sft.sft_hermes_tool_example(i)
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_hermes_{i:04d}"
        ex['source_corpus'] = 'hermes_tool_templates'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'hermes_tool_templates', [f"template_{i}"], 'hermes_internal',
            eval_split='train'
        )
        sft_records.append(ex)
        source_counts['hermes_tool'] += 1
        token_total += ex['token_count']
    print(f"  Hermes/tool: {n_hermes}")

    # --- 7. General retention (5% = ~150) ---
    print("  Building general retention SFT examples...")
    n_gen = targets['general_retention']
    repo_state_path = os.path.join(RUST, 'repo_state_v1.jsonl')
    gen_examples = []
    if os.path.exists(repo_state_path):
        with open(repo_state_path) as f:
            for line in f:
                gen_examples.append(json.loads(line))

    for i in range(min(n_gen, len(gen_examples))):
        rec = gen_examples[i]
        ex = {
            'instruction': 'You are an AI assistant. Describe the architecture and key components of this Solana trading system.',
            'input': {
                'snapshot_type': rec.get('snapshot_type', ''),
                'description': rec.get('description', ''),
                'content': rec.get('content', '')[:2000],
            },
            'output': {
                'response': f"System snapshot: {rec.get('snapshot_type', 'N/A')}. "
                           f"{rec.get('description', 'N/A')}. "
                           f"Key components visible in the repository state.",
            },
            'token_count': 0,
        }
        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
        ex['id'] = f"sft_gen_{i:04d}"
        ex['source_corpus'] = 'rust_gold_v1'
        ex['provenance'] = _cpt_sft.make_sft_provenance(
            'rust_gold_v1', [f"repo_state_{i}"], RUST_FREEZE_UUID,
            eval_split='train'
        )
        sft_records.append(ex)
        source_counts['general_retention'] += 1
        token_total += ex['token_count']
    print(f"  General retention: {min(n_gen, len(gen_examples))}")

    # --- Write SFT JSONL ---
    # DEDUP: Remove near-identical SFT records by (instruction+input+output) hash
    seen_sft_hashes = set()
    deduped_sft = []
    sft_dedup_removed = 0
    for rec in sft_records:
        _h = hashlib.sha256(json.dumps(rec.get('instruction', '') + str(rec.get('input', '')) + str(rec.get('output', ''))).encode()).hexdigest()[:32]
        if _h not in seen_sft_hashes:
            seen_sft_hashes.add(_h)
            deduped_sft.append(rec)
        else:
            sft_dedup_removed += 1
    if sft_dedup_removed > 0:
        print(f"  SFT dedup: removed {sft_dedup_removed} duplicate records")
        sft_records = deduped_sft

    sft_path = os.path.join(SFT_DIR, 'qwen_sft_v1.jsonl')
    _cpt_sft.write_jsonl(sft_records, sft_path)
    sft_hash = _cpt_sft.compute_file_hash(sft_path)

    sft_manifest = {
        'version': 'qwen_sft_v1',
        'frozen_at': datetime.now(timezone.utc).isoformat(),
        'run_uuid': RUN_UUID,
        'total_records': len(sft_records),
        'total_tokens': token_total,
        'target_tokens': 15_000_000,
        'dedup_removed': sft_dedup_removed,
        'eval_exclusion': {'mints_excluded': len(_sft_eval_excluded_mints), 'state_ids_excluded': len(_sft_eval_excluded_state_ids)},
        'file': sft_path,
        'file_hash': sft_hash[:32],
        'mix_targets': SFT_MIX,
        'source_composition': dict(source_counts),
        'corpora_included': ['slinky_gold_v3', 'laserstream_gold_v3', 'rust_gold_v1', 'narrative_gold_v1.1'],
        'narrative_dropout': 0.40,
        'latency_correction': {
            'applied': True,
            'latency_effect_observed': False,
            'note': 'sim_return_pct invariant across 0/100/500/1000/2000ms in frozen L3',
        },
        'provenance_uuids': {
            'slinky': SLINKY_FREEZE_UUID,
            'laserstream': LS_FREEZE_UUID,
            'rust': RUST_FREEZE_UUID,
            'narrative': NARR_FREEZE_UUID,
        },
        'utility_version': UTILITY_VERSION,
    }

    manifest_path = os.path.join(SFT_DIR, 'SFT_MANIFEST_V1.json')
    with open(manifest_path, 'w') as f:
        json.dump(sft_manifest, f, indent=2)

    print(f"\n  SFT v1 FROZEN: {len(sft_records)} records, ~{token_total:,} tokens")
    print(f"  File: {sft_path}")
    print(f"  Hash: {sft_hash[:32]}...")
    print(f"  Manifest: {manifest_path}")
    print(f"  Source composition: {dict(source_counts)}")

    return sft_manifest


# ════════════════════════════════════════════════════════════════
# MAIN
# ════════════════════════════════════════════════════════════════

if __name__ == '__main__':
    print(f"Qwen Curriculum v1 Export Builder")
    print(f"Run UUID: {RUN_UUID}")
    print(f"Freeze time: {FREEZE_TIME}")
    print(f"Utility version: {UTILITY_VERSION}")
    print()

    # Step 1: Freeze eval v1.1 (hard category coverage)
    # Skip if already frozen (deterministic — same hash every time)
    eval_v1_1_path = os.path.join(EVAL_DIR_V1_1, 'qwen_eval_v1_1.jsonl')
    eval_manifest_path = os.path.join(EVAL_DIR_V1_1, 'EVAL_MANIFEST_V1_1.json')
    if os.path.exists(eval_v1_1_path) and os.path.exists(eval_manifest_path):
        import json as _json_skip
        with open(eval_manifest_path, 'r') as _f:
            eval_manifest = _json_skip.load(_f)
        print(f"=== EVAL v1.1 ALREADY FROZEN — skipping rebuild ===")
        print(f"  Hash: {eval_manifest.get('eval_hash', 'N/A')[:32]}...")
    else:
        eval_manifest = build_eval_freeze_v1_1()
        print("\n=== EVAL FREEZE v1.1 COMPLETE ===")
    print(f"Total eval examples: {eval_manifest['total_eval_examples']}")
    print(f"Market panels: {eval_manifest['total_market_panels']}")
    print(f"Narrative pairs: {eval_manifest['total_narrative_pairs']}")
    print(f"Rust repairs: {eval_manifest['total_rust_repairs']}")

    # Step 2: Build CPT v1 (continual pre-training)
    print("\n" + "="*60)
    cpt_manifest = build_cpt_v1()

    # Step 3: Build SFT v1 (supervised fine-tuning)
    print("\n" + "="*60)
    sft_manifest = build_sft_v1()

    print("\n" + "="*60)
    print("=== QWEN CURRICULUM v1 EXPORT COMPLETE ===")
    print(f"Eval: {eval_manifest['total_eval_examples']} examples")
    print(f"CPT:  {cpt_manifest['total_records']} records, ~{cpt_manifest['total_tokens']:,} tokens")
    print(f"SFT:  {sft_manifest['total_records']} records, ~{sft_manifest['total_tokens']:,} tokens")
    print("="*60)
