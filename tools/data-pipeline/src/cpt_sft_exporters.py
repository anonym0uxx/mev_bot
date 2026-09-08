#!/usr/bin/env python3
"""
CPT/SFT Exporter functions for qwen_curriculum_v1.
Implements build_cpt_v1() and build_sft_v1() with the LS latency correction:
- sim_return_pct is invariant across 0/100/500/1000/2000ms in frozen LaserStream L3.
- Collapse duplicate latency scenarios for normal training examples.
- Preserve provenance that 5 latency scenarios existed.
- Set latency_effect_observed=false, latency_sensitivity=0 only where genuinely observed.
- Retain size sensitivity across 0.05/0.10/0.25/0.50/1.0 SOL.
- Keep genuine capacity-constrained examples without oversampling.
- All 4 corpora: Slinky Gold, LaserStream Gold, Rust Gold, Narrative Gold v1.1.
"""

import os
import json
import hashlib
import math
import time
import random
import re as _re
from datetime import datetime, timezone
from collections import Counter, defaultdict
import pyarrow.parquet as pq
import pandas as pd
import numpy as np


# ════════════════════════════════════════════════════════════════
# CONSTANTS (mirror builder's config)
# ════════════════════════════════════════════════════════════════

BASE = 'D:/repos/mev_bot/tools/data-pipeline/output'
OUT = os.path.join(BASE, 'qwen_curriculum_v1')
CPT_DIR = os.path.join(OUT, 'cpt')
SFT_DIR = os.path.join(OUT, 'sft')

SLINKY = os.path.join(BASE, 'slinky_gold_v3_compact')
LS = os.path.join(BASE, 'laserstream_gold_v3')
RUST = os.path.join(BASE, 'rust_gold_v1')
NARR = os.path.join(BASE, 'narrative_gold_v1.1/gold')

RUST_FREEZE_UUID = "55e19441"
NARR_FREEZE_UUID = "ng11_43bf56a50ed7"
SLINKY_FREEZE_UUID = "cf97c33"
LS_FREEZE_UUID = "cf97c33"

UTILITY_VERSION = "robust_utility_v1"
LS_LATENCIES = [0, 100, 500, 1000, 2000]
LS_SIZES = [0.05, 0.10, 0.25, 0.50, 1.0]

# Token estimation: ~4 chars = 1 token (rough estimate for mixed content)
CHARS_PER_TOKEN = 4.0

# ============================================================
# QWEN3.8-27B EXACT TOKENIZER
# ============================================================
# Model: unsloth/Qwen3.8-27B
# Snapshot: 3ea932cee0a432ae86e9c7826cbe8aef52323a28
# Tokenizer type: Qwen2Tokenizer, Vocab: 248,044, Max len: 262,144
# tokenizer_config.json hash: 2e6eac2825dcd97362f8910ca75f6cb0
# tokenizer.json hash: 0997f410c57a1f4e53b09e4be8f4a172
# Char/token ratio: ~5.33 (not 4.0)
# ============================================================

_QWEN_TOKENIZER = None
_QWEN_TOKENIZER_LOADED = False
QWEN_TOKENIZER_NAME = "unsloth/Qwen3.8-27B"
QWEN_TOKENIZER_SNAPSHOT = "3ea932cee0a432ae86e9c7826cbe8aef52323a28"
QWEN_TOKENIZER_CONFIG_HASH = "2e6eac2825dcd97362f8910ca75f6cb0"
QWEN_TOKENIZER_JSON_HASH = "0997f410c57a1f4e53b09e4be8f4a172"


def load_qwen_tokenizer():
    """Load the exact Qwen3.8-27B tokenizer for accurate token counting."""
    global _QWEN_TOKENIZER, _QWEN_TOKENIZER_LOADED
    if _QWEN_TOKENIZER_LOADED:
        return _QWEN_TOKENIZER
    try:
        from transformers import AutoTokenizer
        _QWEN_TOKENIZER = AutoTokenizer.from_pretrained(
            QWEN_TOKENIZER_NAME,
            cache_dir=os.path.expanduser('~/.cache/huggingface/hub')
        )
        _QWEN_TOKENIZER_LOADED = True
        print(f"  [tokenizer] Loaded {QWEN_TOKENIZER_NAME} (vocab={_QWEN_TOKENIZER.vocab_size})")
        return _QWEN_TOKENIZER
    except Exception as e:
        print(f"  [tokenizer] Fallback to char-estimate: {e}")
        _QWEN_TOKENIZER_LOADED = True  # don't retry
        return None


def count_tokens_qwen(text):
    """Count tokens using the exact Qwen3.8-27B tokenizer. Falls back to estimate."""
    tok = load_qwen_tokenizer()
    if tok is not None:
        return len(tok.encode(text))
    return max(1, int(len(text) / CHARS_PER_TOKEN))


def estimate_tokens(text):
    """Token estimate using exact Qwen tokenizer if available, else ~4 chars/token."""
    tok = load_qwen_tokenizer()
    if tok is not None:
        return len(tok.encode(text))
    return max(1, int(len(text) / CHARS_PER_TOKEN))


def make_cpt_provenance(source_corpus, source_ids, source_layer, freeze_uuid,
                        timestamp_range=None, mint_ids=None, scope_tier=None,
                        narrative_temporal_class=None):
    """Build provenance dict for CPT records."""
    prov = {
        'freeze_uuid': freeze_uuid,
        'source_corpus': source_corpus,
        'source_layer': source_layer,
        'source_ids': source_ids[:20],  # cap for storage
        'source_hash': hashlib.sha256(json.dumps(source_ids[:20]).encode()).hexdigest()[:16],
    }
    if timestamp_range:
        prov['timestamp_range'] = timestamp_range
    if mint_ids:
        prov['mint_ids_included'] = mint_ids[:20]
    if scope_tier:
        prov['scope_tier'] = scope_tier
    if narrative_temporal_class:
        prov['narrative_temporal_class'] = narrative_temporal_class
    return prov


def make_sft_provenance(source_corpus, source_ids, freeze_uuid,
                        panel_id=None, eval_split='train', scope_tier=None,
                        l3_detail_level=None, contrast_pair_id=None):
    """Build provenance dict for SFT records."""
    prov = {
        'freeze_uuid': freeze_uuid,
        'source_corpus': source_corpus,
        'source_ids': source_ids[:20],
        'source_hash': hashlib.sha256(json.dumps(source_ids[:20]).encode()).hexdigest()[:16],
        'eval_split': eval_split,
    }
    if panel_id:
        prov['panel_id'] = panel_id
    if scope_tier:
        prov['scope_tier'] = scope_tier
    if l3_detail_level:
        prov['l3_detail_level'] = l3_detail_level
    if contrast_pair_id is not None:
        prov['contrast_pair_id'] = contrast_pair_id
    return prov


def write_jsonl(records, filepath):
    """Write records to JSONL file."""
    os.makedirs(os.path.dirname(filepath), exist_ok=True)
    with open(filepath, 'w') as f:
        for rec in records:
            f.write(json.dumps(rec, default=str) + '\n')
    return sum(1 for _ in open(filepath))


def compute_file_hash(filepath):
    """SHA-256 hash of a file."""
    h = hashlib.sha256()
    with open(filepath, 'rb') as f:
        for chunk in iter(lambda: f.read(8192), b''):
            h.update(chunk)
    return h.hexdigest()


# ════════════════════════════════════════════════════════════════
# CPT TEXT GENERATORS (packed text, no instruction format)
# ════════════════════════════════════════════════════════════════

def _sf(row, key, fmt=None, default='N/A'):
    """Safe field getter — returns formatted value or default for NaN/None."""
    v = row.get(key) if hasattr(row, 'get') else row[key] if key in (row.index if hasattr(row, 'index') else []) else None
    if v is None:
        return default
    try:
        if pd.isna(v):
            return default
    except (TypeError, ValueError):
        pass
    if fmt:
        try:
            return fmt(v)
        except (TypeError, ValueError):
            return default
    return v


def _al(lines, label, row, key, fmt=None, indent='    '):
    """Append line only if field has a real (non-NaN) value. Skips N/A noise."""
    v = row.get(key) if hasattr(row, 'get') else (row[key] if key in (row.index if hasattr(row, 'index') else row) else None)
    if v is None:
        return
    try:
        if pd.isna(v):
            return
    except (TypeError, ValueError):
        return
    if fmt:
        try:
            v = fmt(v)
        except (TypeError, ValueError):
            return
    lines.append(f"{indent}{label}: {v}")


CPT_SAFETY_MAX_TOKENS = 4000  # high safety bound — NOT a target, only triggers compression
CPT_COMPRESSION_VERSION = 'v1_change_point'


def _detect_change_points(state_rows, max_points=4):
    """Detect states where key causal fields shift most (change-point detection).

    Returns (indices, scores) where indices are sorted by position and scores
    is a dict mapping each selected index to its aggregate change score.
    Selection is value-agnostic: uses only causal state fields, never outcome/future.
    """
    if len(state_rows) < 3:
        return [], {}

    # Critical causal fields for change detection (curve, velocity, wallet, liquidity)
    # NEVER use outcome/L2/L3 future fields — value-agnostic selection.
    change_fields = [
        'curve_pct_depleted', 'price_sol', 'market_cap_sol', 'buy_pressure',
        'holder_concentration_hhi', 'toxic_flow_indicator', 'wash_trade_ratio',
        'liquidity_sol', 'net_flow_sol', 'trade_velocity_30s',
        'coordinated_buy_ratio', 'buy_sell_imbalance_5s',
        'curve_depletion_velocity_5s', 'price_momentum_5s_bp',
    ]

    available = [f for f in change_fields if f in state_rows.columns]
    if not available:
        n = len(state_rows)
        indices = [n // 4, n // 2, 3 * n // 4][:max_points]
        return indices, {i: 1.0 for i in indices}

    n = len(state_rows)
    scores = np.zeros(n)
    import warnings as _w
    for f in available:
        vals = state_rows[f].values.astype(float)
        # Normalize by range to make all fields comparable
        with _w.catch_warnings(), np.errstate(invalid='ignore'):
            _w.simplefilter('ignore', RuntimeWarning)
            rng = np.nanmax(vals) - np.nanmin(vals)
        if rng == 0 or np.isnan(rng):
            continue
        norm_vals = (vals - np.nanmin(vals)) / rng
        diffs = np.abs(np.diff(norm_vals))
        scores[1:] += np.nan_to_num(diffs, nan=0)

    scores[0] = -1
    scores[-1] = -1

    selected = []
    remaining = np.argsort(scores)[::-1]
    for idx in remaining:
        if scores[idx] <= 0:
            break
        if all(abs(idx - s) >= 3 for s in selected):
            selected.append(int(idx))
            if len(selected) >= max_points:
                break

    selected.sort()
    return selected, {i: float(scores[i]) for i in selected}


def cpt_slinky_trajectory_summary(mint, state_rows, outcome_row, cf_rows=None):
    """Generate a CPT text block for a Slinky per-mint trajectory summary.
    Uses causal state + outcome targets (NOT leakage fields like ret_1s as inputs).
    Covers all ~100 causal fields from pump_state_v3 schema across 6 sections.

    Length philosophy: CPT examples MAY vary substantially in length. A genuinely
    information-dense 2K-4K token example is valid. A high safety bound (4,000
    tokens) triggers deterministic information-preserving compression — never a
    hard universal ceiling. Compression preserves causal start/latest state,
    major change points, critical Pump.fun fields, and outcome/economic evidence.
    """
    lines = []
    mint_hash = hashlib.sha256(mint.encode()).hexdigest()[:12]
    lines.append(f"Token: {mint_hash}")
    lines.append(f"Venue: Pump.fun bonding curve (Token-2022)")
    lines.append("")

    # ===== 1. LAUNCH CONTEXT (first state) =====
    if len(state_rows) > 0:
        row = state_rows.iloc[0]
        lines.append("=== LAUNCH CONTEXT ===")
        _al(lines, "Initial market cap", row, 'initial_market_cap_sol', lambda v: f'{v:.4f} SOL')
        _al(lines, "Initial supply (raw)", row, 'initial_supply_raw', lambda v: f'{v:.0f}')
        _al(lines, "Initial price", row, 'initial_price_sol', lambda v: f'{v:.10f} SOL')
        _al(lines, "Initial holder count", row, 'initial_holder_count', lambda v: f'{v:.0f}')
        _al(lines, "Initial Gini coef", row, 'initial_gini', lambda v: f'{v:.4f}')
        _al(lines, "Initial top-10 pct (corrected)", row, 'initial_top10_pct_corrected', lambda v: f'{v:.4f}')
        creator_raw = row.get('creator', '')
        if creator_raw and pd.notna(creator_raw):
            chash = hashlib.sha256(str(creator_raw).encode()).hexdigest()[:8]
            lines.append(f"  Creator: {chash}")
        _al(lines, "Creator past tokens", row, 'creator_past_tokens', lambda v: f'{v:.0f}')
        _al(lines, "Creator past rugs", row, 'creator_past_rugs', lambda v: f'{v:.0f}')
        _al(lines, "Dev buy pct (corrected)", row, 'dev_buy_pct_corrected', lambda v: f'{v:.4f}')
        _al(lines, "Data quality score", row, 'data_quality_score', lambda v: f'{v:.4f}')
        lines.append("")

    # ===== 2. INTERMEDIATE TRAJECTORY STATES (change-point detection) =====
    # Select states where key causal fields shift most, NOT uniformly spaced.
    # All real-value fields shown for each selected state — no tier truncation.
    cp_scores = {}  # {state_idx: change_score} for compression ordering
    if len(state_rows) > 2:
        lines.append("=== TRAJECTORY EVOLUTION (change-point selected) ===")
        n_states = len(state_rows)
        cp_indices, cp_scores = _detect_change_points(state_rows, max_points=4)

        for idx in cp_indices:
            if idx >= n_states:
                continue
            r = state_rows.iloc[idx]
            tsl = _sf(r, 'seconds_since_launch', lambda v: f'{v:.1f}s')
            msl = _sf(r, 'minutes_since_launch', lambda v: f'{v:.1f}m')
            lines.append(f"  [T{idx}] t={tsl} ({msl}):")

            # — Curve / bonding state —
            _al(lines, "Curve pct depleted", r, 'curve_pct_depleted', lambda v: f'{v:.6f}')
            _al(lines, "Curve depletion vel 5s", r, 'curve_depletion_velocity_5s', lambda v: f'{v:.6f}')
            _al(lines, "Curve depletion vel 30s", r, 'curve_depletion_velocity_30s', lambda v: f'{v:.6f}')
            _al(lines, "V_sol bonding curve", r, 'v_sol_bonding_curve_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "V_sol depletion rate 5s", r, 'v_sol_depletion_rate_5s', lambda v: f'{v:.6f}')
            _al(lines, "V_tokens accum rate 5s", r, 'v_tokens_accumulation_rate_5s', lambda v: f'{v:.2f}')
            _al(lines, "Graduation proximity", r, 'graduation_proximity_pct', lambda v: f'{v:.4f}')

            # — Price / market cap dynamics —
            _al(lines, "Price", r, 'price_sol', lambda v: f'{v:.10f} SOL')
            _al(lines, "Price chg since launch", r, 'price_change_since_launch_bp', lambda v: f'{v:.2f}bp')
            _al(lines, "Price momentum 5s", r, 'price_momentum_5s_bp', lambda v: f'{v:.2f}bp')
            _al(lines, "Price momentum 30s", r, 'price_momentum_30s_bp', lambda v: f'{v:.2f}bp')
            _al(lines, "Price volatility 30s", r, 'price_volatility_30s_bp', lambda v: f'{v:.2f}bp')
            _al(lines, "Market cap", r, 'market_cap_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Mcap momentum 5s", r, 'mcap_momentum_5s_bp', lambda v: f'{v:.2f}bp')

            # — Trade flow / volume —
            _al(lines, "Trade count so far", r, 'trade_count_so_far', lambda v: f'{int(v)}')
            _al(lines, "Buy count", r, 'buy_count_so_far', lambda v: f'{int(v)}')
            _al(lines, "Sell count", r, 'sell_count_so_far', lambda v: f'{int(v)}')
            _al(lines, "Buy pressure", r, 'buy_pressure', lambda v: f'{v:.4f}')
            _al(lines, "Buy/sell imbalance 5s", r, 'buy_sell_imbalance_5s', lambda v: f'{v:.4f}')
            _al(lines, "Buy/sell imbalance 30s", r, 'buy_sell_imbalance_30s', lambda v: f'{v:.4f}')
            _al(lines, "Net flow", r, 'net_flow_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Total vol", r, 'total_vol_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Buy vol", r, 'buy_vol_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Sell vol", r, 'sell_vol_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Vol velocity 5s", r, 'vol_velocity_5s', lambda v: f'{v:.4f}')
            _al(lines, "Vol velocity 30s", r, 'vol_velocity_30s', lambda v: f'{v:.4f}')
            _al(lines, "Vol acceleration", r, 'vol_acceleration', lambda v: f'{v:.4f}')

            # — Trade velocity / breadth —
            _al(lines, "Trade velocity 5s", r, 'trade_velocity_5s', lambda v: f'{v:.4f}')
            _al(lines, "Trade velocity 30s", r, 'trade_velocity_30s', lambda v: f'{v:.4f}')
            _al(lines, "Avg trade size", r, 'avg_trade_size_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Median trade size", r, 'median_trade_size_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Max trade size", r, 'max_trade_size_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Trade size std", r, 'trade_size_std_sol', lambda v: f'{v:.4f} SOL')

            # — Wallet / holder structure —
            _al(lines, "Unique wallets", r, 'unique_wallets_so_far', lambda v: f'{int(v)}')
            _al(lines, "Unique buyers", r, 'unique_buyers_so_far', lambda v: f'{int(v)}')
            _al(lines, "Unique sellers", r, 'unique_sellers_so_far', lambda v: f'{int(v)}')
            _al(lines, "Unique buyers 5s", r, 'unique_buyers_5s', lambda v: f'{int(v)}')
            _al(lines, "Unique sellers 5s", r, 'unique_sellers_5s', lambda v: f'{int(v)}')
            _al(lines, "Holder concentration HHI", r, 'holder_concentration_hhi', lambda v: f'{v:.6f}')
            _al(lines, "Top-1 buyer pct", r, 'top1_buyer_pct', lambda v: f'{v:.4f}')
            _al(lines, "Top-5 buyer pct", r, 'top5_buyer_pct', lambda v: f'{v:.4f}')
            _al(lines, "Buyer concentration ratio", r, 'buyer_concentration_ratio', lambda v: f'{v:.4f}')
            _al(lines, "Seller concentration ratio", r, 'seller_concentration_ratio', lambda v: f'{v:.4f}')

            # — Coordination / sybil / manipulation signals —
            _al(lines, "Coordinated buy ratio", r, 'coordinated_buy_ratio', lambda v: f'{v:.4f}')
            _al(lines, "Repeat buyer ratio", r, 'repeat_buyer_ratio', lambda v: f'{v:.4f}')
            _al(lines, "Repeat seller ratio", r, 'repeat_seller_ratio', lambda v: f'{v:.4f}')
            _al(lines, "Rapid rebuy count", r, 'rapid_rebuy_count', lambda v: f'{int(v)}')
            _al(lines, "Sybil cluster size", r, 'sybil_cluster_size', lambda v: f'{v:.0f}')
            _al(lines, "Wallets also selling", r, 'wallets_also_selling', lambda v: f'{int(v)}')
            _al(lines, "Avg wallet trade count", r, 'avg_wallet_trade_count', lambda v: f'{v:.2f}')
            _al(lines, "Toxic flow indicator", r, 'toxic_flow_indicator', lambda v: f'{v:.4f}')
            _al(lines, "Wash trade ratio", r, 'wash_trade_ratio', lambda v: f'{v:.4f}')
            _al(lines, "Largest buy pct of vol", r, 'largest_buy_pct_of_vol', lambda v: f'{v:.4f}')
            _al(lines, "Largest sell pct of vol", r, 'largest_sell_pct_of_vol', lambda v: f'{v:.4f}')

            # — Liquidity / capacity —
            _al(lines, "Liquidity", r, 'liquidity_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Liq change 5s", r, 'liquidity_change_5s_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Liq change 30s", r, 'liquidity_change_30s_sol', lambda v: f'{v:.4f} SOL')

            # — Market context —
            _al(lines, "Active mints (5m)", r, 'market_active_mints_5m', lambda v: f'{v:.0f}')
            _al(lines, "Market avg buy pressure (5m)", r, 'market_avg_buy_pressure_5m', lambda v: f'{v:.4f}')
            _al(lines, "Market total vol (5m)", r, 'market_total_vol_5m_sol', lambda v: f'{v:.4f} SOL')

            # — Meta —
            _al(lines, "Is graduated", r, 'is_graduated')
            _al(lines, "Is zombie", r, 'is_zombie')
            _al(lines, "Is mayhem", r, 'is_mayhem')
            lines.append("")

    # ===== 3. FINAL STATE =====
    if len(state_rows) > 0:
        last = state_rows.iloc[-1]
        lines.append("=== FINAL STATE ===")
        _al(lines, "Curve pct depleted", last, 'curve_pct_depleted', lambda v: f'{v:.6f}')
        _al(lines, "Is graduated", last, 'is_graduated')
        _al(lines, "Seconds to graduation", last, 'seconds_to_graduation', lambda v: f'{v:.1f}s')
        _al(lines, "Graduation proximity", last, 'graduation_proximity_pct', lambda v: f'{v:.4f}')
        _al(lines, "Total trades", last, 'trade_count_so_far', lambda v: f'{int(v)}')
        _al(lines, "Unique wallets", last, 'unique_wallets_so_far', lambda v: f'{int(v)}')
        _al(lines, "Market cap", last, 'market_cap_sol', lambda v: f'{v:.4f} SOL')
        _al(lines, "Buy pressure", last, 'buy_pressure', lambda v: f'{v:.4f}')
        _al(lines, "Toxic flow", last, 'toxic_flow_indicator', lambda v: f'{v:.4f}')
        _al(lines, "Wash trade ratio", last, 'wash_trade_ratio', lambda v: f'{v:.4f}')
        _al(lines, "Holder HHI", last, 'holder_concentration_hhi', lambda v: f'{v:.6f}')
        _al(lines, "Liquidity", last, 'liquidity_sol', lambda v: f'{v:.4f} SOL')
        _al(lines, "Net flow", last, 'net_flow_sol', lambda v: f'{v:.4f} SOL')
        lines.append("")

    # ===== 4. OUTCOME (TARGET EVIDENCE — not used as input) =====
    if outcome_row is not None:
        o = outcome_row
        lines.append("=== OUTCOME (TARGET EVIDENCE — not used as input) ===")
        mfe = o.get('mfe_bp')
        mae = o.get('mae_bp')
        if pd.notna(mfe) and pd.notna(mae):
            lines.append(f"  MFE: {mfe:.0f}bp, MAE: {mae:.0f}bp")
        _al(lines, "Peak return", o, 'peak_bp', lambda v: f'{v:.0f}bp')
        _al(lines, "Time to peak", o, 'time_to_peak_seconds', lambda v: f'{v:.1f}s')
        _al(lines, "MFE time", o, 'mfe_time_seconds', lambda v: f'{v:.1f}s')
        _al(lines, "MAE time", o, 'mae_time_seconds', lambda v: f'{v:.1f}s')
        _al(lines, "Graduated after state", o, 'graduated_after_state')
        _al(lines, "Graduated did migrate", o, 'graduated_did_migrate')
        _al(lines, "Collapsed 50% within 300s", o, 'collapsed_50pct_within_300s')
        _al(lines, "Survived 60s", o, 'survived_60s')
        _al(lines, "Survived 300s", o, 'survived_300s')
        for horizon in ['1s', '5s', '30s', '60s', '300s']:
            ret = o.get(f'ret_{horizon}_bp')
            if pd.notna(ret):
                lines.append(f"  Return {horizon}: {ret:.0f}bp")
        # Barrier hit times
        for bp in ['plus_10', 'plus_25', 'plus_50', 'plus_100', 'plus_200', 'plus_500', 'plus_1000']:
            t = o.get(f'hit_{bp}_bp_time')
            if pd.notna(t) and t > 0:
                label = bp.replace('plus_', '')
                lines.append(f"  Barrier +{label}bp hit at {t:.1f}s")
        for bp in ['minus_10', 'minus_30', 'minus_50', 'minus_100', 'minus_200', 'minus_500']:
            t = o.get(f'hit_{bp}_bp_time')
            if pd.notna(t) and t > 0:
                label = bp.replace('minus_', '-')
                lines.append(f"  Barrier {label}bp hit at {t:.1f}s")
        lines.append("")

    # ===== 5. COUNTERFACTUAL ECONOMICS =====
    if cf_rows is not None:
        if isinstance(cf_rows, dict):
            cf_row = cf_rows
        elif hasattr(cf_rows, 'iloc') and len(cf_rows) > 0:
            cf_row = cf_rows.iloc[0].to_dict()
        elif isinstance(cf_rows, list) and len(cf_rows) > 0:
            cf_row = cf_rows[0] if isinstance(cf_rows[0], dict) else cf_rows[0].to_dict() if hasattr(cf_rows[0], 'to_dict') else None
        else:
            cf_row = cf_rows if isinstance(cf_rows, dict) else None

        if cf_row:
            sizes = [('sz005', 0.05), ('sz010', 0.10), ('sz025', 0.25), ('sz050', 0.50), ('sz100', 1.0)]
            lines.append("=== COUNTERFACTUAL ECONOMICS (250ms latency, TP=1500bp, SL=1500bp, max hold 300s) ===")
            _al(lines, "Eligible", cf_row, 'eligible')
            _al(lines, "Eligibility reason", cf_row, 'eligibility_reason')
            _al(lines, "Economic class", cf_row, 'economic_class')
            _al(lines, "Economic class reason", cf_row, 'economic_class_reason')
            _al(lines, "Entry price", cf_row, 'entry_price_sol', lambda v: f'{v:.10f} SOL')
            _al(lines, "Entry size", cf_row, 'entry_size_sol', lambda v: f'{v:.4f} SOL')
            _al(lines, "Entry fee", cf_row, 'entry_fee_sol', lambda v: f'{v:.6f} SOL')
            _al(lines, "Entry slippage", cf_row, 'entry_slippage_bp', lambda v: f'{int(v)}bp')
            for prefix, sz in sizes:
                feasible = cf_row.get(f'{prefix}_feasible', False)
                ret_pct = cf_row.get(f'{prefix}_net_return_pct')
                pnl = cf_row.get(f'{prefix}_net_pnl_sol')
                impact = cf_row.get(f'{prefix}_entry_impact_bp')
                if feasible and ret_pct is not None and (not isinstance(ret_pct, float) or not math.isnan(ret_pct)):
                    lines.append(f"  Size {sz:.2f} SOL: FEASIBLE, net ret {ret_pct:.2f}%, net PnL {pnl:.6f} SOL, impact {impact:.1f}bp")
                else:
                    lines.append(f"  Size {sz:.2f} SOL: INFEASIBLE")
            _al(lines, "Exit feasible", cf_row, 'exit_feasible')
            _al(lines, "Risk/reward ratio", cf_row, 'risk_reward_ratio', lambda v: f'{v:.4f}')
            _al(lines, "Hold duration", cf_row, 'hold_duration_seconds', lambda v: f'{v:.1f}s')
            _al(lines, "Exit reason", cf_row, 'exit_reason')
            lines.append("")

    # ===== POST-HOC SAFETY-BOUND COMPRESSION =====
    # High safety bound (4K tokens) — NOT a target. Only triggers when genuinely
    # over. Deterministic, value-agnostic: removes intermediate change-point states
    # with the LOWEST aggregate change score first, preserving first + latest +
    # highest-change-point states. Never removes launch, final, outcome, or CF blocks.
    # Compression metadata lives in the record schema, NOT in model-visible text.
    text = '\n'.join(lines)
    pre_compress_tokens = estimate_tokens(text)
    compression_meta = None

    if pre_compress_tokens > CPT_SAFETY_MAX_TOKENS:
        # Find all [T{idx}] block start lines and map them to state indices + scores
        t_blocks = []  # list of (line_idx, state_idx, score)
        for i, l in enumerate(lines):
            if '[T' in l and '] t=' in l:
                # Parse state index from "[T17] t=..."
                m = _re.search(r'\[T(\d+)\]', l)
                state_idx = int(m.group(1)) if m else -1
                score = cp_scores.get(state_idx, 0.0)
                t_blocks.append([i, state_idx, score])

        states_original_intermediate = len(t_blocks)
        cur_tokens = pre_compress_tokens

        # Sort blocks by score ascending — remove lowest-score blocks first
        # Ties broken by position (later position = less critical, removed first)
        while cur_tokens > CPT_SAFETY_MAX_TOKENS and len(t_blocks) > 1:
            # Find the block with the lowest score (and latest position on ties)
            min_score = min(b[2] for b in t_blocks)
            candidates = [b for b in t_blocks if b[2] == min_score]
            victim = max(candidates, key=lambda b: b[0])  # latest position on tie

            line_idx = victim[0]
            # Find end of this block: next blank line or next === section
            end_idx = None
            for i in range(line_idx + 1, len(lines)):
                if lines[i] == '' or '===' in lines[i]:
                    end_idx = i
                    break
            if end_idx is None:
                break

            # Remove block lines (line_idx through end_idx exclusive)
            lines = lines[:line_idx] + lines[end_idx:]
            text = '\n'.join(lines)
            cur_tokens = estimate_tokens(text)

            # Re-index remaining blocks (line indices shifted)
            t_blocks = []
            for i, l in enumerate(lines):
                if '[T' in l and '] t=' in l:
                    m = _re.search(r'\[T(\d+)\]', l)
                    state_idx = int(m.group(1)) if m else -1
                    score = cp_scores.get(state_idx, 0.0)
                    t_blocks.append([i, state_idx, score])

        states_retained = len(t_blocks)
        compression_meta = {
            'compressed': True,
            'original_tokens': pre_compress_tokens,
            'final_tokens': cur_tokens,
            'compression_strategy': 'drop_lowest_change_point_states',
            'compression_version': CPT_COMPRESSION_VERSION,
            'states_original': states_original_intermediate,
            'states_retained': states_retained,
        }

    return {
        'text': '\n'.join(lines),
        'metadata': compression_meta,
        # Token count already computed for the safety bound — return it so the
        # caller doesn't re-encode the full text a second time (tokenization is
        # the pipeline's dominant cost).
        'tokens': (compression_meta['final_tokens'] if compression_meta
                   else pre_compress_tokens),
    }


def cpt_slinky_panel_snapshot(panel_candidates, anchor_ms):
    """Generate a CPT text block for a Slinky cross-sectional panel snapshot."""
    lines = []
    lines.append(f"Panel timestamp: {anchor_ms}ms")
    lines.append(f"Panel size: {len(panel_candidates)} candidates")
    lines.append("")

    for i, c in enumerate(panel_candidates[:10]):  # cap at 10 for brevity
        mint_hash = hashlib.sha256(str(c.get('mint', '')).encode()).hexdigest()[:8]
        lines.append(f"Candidate {i+1} ({mint_hash}):")
        lines.append(f"  Curve pct: {c.get('curve_pct_depleted', 'N/A')}")
        lines.append(f"  Velocity 30s: {c.get('trade_velocity_30s', 'N/A')}")
        lines.append(f"  Unique wallets: {c.get('unique_wallets_so_far', 'N/A')}")
        lines.append(f"  Buy pressure: {c.get('buy_pressure', 'N/A')}")
        lines.append(f"  Toxic flow: {c.get('toxic_flow_indicator', 'N/A')}")
        lines.append(f"  HHI: {c.get('holder_concentration_hhi', 'N/A')}")

    if len(panel_candidates) > 10:
        lines.append(f"... and {len(panel_candidates) - 10} more candidates")

    return '\n'.join(lines)


def cpt_slinky_regime_catalog(regime_name, mints_data):
    """Generate a CPT text block for a regime pattern catalog.

    Aggregates causal patterns across mints in the same regime category.
    Model-visible text contains ONLY domain content (Pump.fun/Solana fields).
    Returns {text, metadata} where metadata has compression/provenance info.
    """
    lines = []
    n = len(mints_data)
    lines.append(f"Regime: {regime_name}")
    lines.append(f"Sample size: {n} mints")
    lines.append("")

    # --- Aggregate outcome evidence (target, not input) ---
    mfe_vals = [d.get('mfe_bp') for d in mints_data if pd.notna(d.get('mfe_bp'))]
    mae_vals = [d.get('mae_bp') for d in mints_data if pd.notna(d.get('mae_bp'))]
    n_grad = sum(1 for d in mints_data if d.get('graduated_after_state'))
    n_collapsed = sum(1 for d in mints_data if d.get('collapsed_50pct_within_300s'))

    def _agg(vals, fmt='{:.1f}', label=''):
        vals = [v for v in vals if pd.notna(v)]
        if not vals:
            return f"  {label}: N/A"
        return f"  {label}: {fmt.format(np.mean(vals))} (n={len(vals)})"

    lines.append("=== OUTCOME EVIDENCE (TARGET — not used as input) ===")
    lines.append(_agg(mfe_vals, '{:.0f}bp', 'Mean MFE'))
    lines.append(_agg(mae_vals, '{:.0f}bp', 'Mean MAE'))
    lines.append(f"  Graduated: {n_grad}/{n} ({100*n_grad/max(1,n):.1f}%)")
    lines.append(f"  Collapsed 50% within 300s: {n_collapsed}/{n} ({100*n_collapsed/max(1,n):.1f}%)")
    lines.append("")

    # --- Typical causal patterns (from state data, not outcome) ---
    lines.append("=== TYPICAL CAUSAL PATTERNS ===")

    # Curve / bonding state
    lines.append(_agg([d.get('curve_pct_depleted') for d in mints_data], '{:.6f}', 'Curve pct depleted (mean)'))
    lines.append(_agg([d.get('curve_depletion_velocity_5s') for d in mints_data], '{:.6f}', 'Curve depletion vel 5s'))
    lines.append(_agg([d.get('graduation_proximity_pct') for d in mints_data], '{:.4f}', 'Graduation proximity'))

    # Price / market cap dynamics
    lines.append(_agg([d.get('price_change_since_launch_bp') for d in mints_data], '{:.2f}bp', 'Price chg since launch'))
    lines.append(_agg([d.get('price_momentum_5s_bp') for d in mints_data], '{:.2f}bp', 'Price momentum 5s'))
    lines.append(_agg([d.get('price_momentum_30s_bp') for d in mints_data], '{:.2f}bp', 'Price momentum 30s'))
    lines.append(_agg([d.get('price_volatility_30s_bp') for d in mints_data], '{:.2f}bp', 'Price volatility 30s'))
    lines.append(_agg([d.get('market_cap_sol') for d in mints_data], '{:.4f} SOL', 'Market cap (mean)'))

    # Trade flow / volume
    lines.append(_agg([d.get('buy_pressure') for d in mints_data], '{:.4f}', 'Buy pressure'))
    lines.append(_agg([d.get('buy_sell_imbalance_5s') for d in mints_data], '{:.4f}', 'Buy/sell imbalance 5s'))
    lines.append(_agg([d.get('buy_sell_imbalance_30s') for d in mints_data], '{:.4f}', 'Buy/sell imbalance 30s'))
    lines.append(_agg([d.get('net_flow_sol') for d in mints_data], '{:.4f} SOL', 'Net flow'))
    lines.append(_agg([d.get('total_vol_sol') for d in mints_data], '{:.4f} SOL', 'Total volume'))
    lines.append(_agg([d.get('vol_velocity_5s') for d in mints_data], '{:.4f}', 'Vol velocity 5s'))
    lines.append(_agg([d.get('vol_acceleration') for d in mints_data], '{:.4f}', 'Vol acceleration'))

    # Trade velocity / breadth
    lines.append(_agg([d.get('trade_velocity_5s') for d in mints_data], '{:.4f}', 'Trade velocity 5s'))
    lines.append(_agg([d.get('trade_velocity_30s') for d in mints_data], '{:.4f}', 'Trade velocity 30s'))
    lines.append(_agg([d.get('avg_trade_size_sol') for d in mints_data], '{:.4f} SOL', 'Avg trade size'))
    lines.append(_agg([d.get('trade_size_std_sol') for d in mints_data], '{:.4f} SOL', 'Trade size std'))

    # Wallet / holder structure
    lines.append(_agg([d.get('unique_wallets_so_far') for d in mints_data], '{:.0f}', 'Unique wallets'))
    lines.append(_agg([d.get('holder_concentration_hhi') for d in mints_data], '{:.6f}', 'Holder HHI'))
    lines.append(_agg([d.get('top1_buyer_pct') for d in mints_data], '{:.4f}', 'Top-1 buyer pct'))
    lines.append(_agg([d.get('top5_buyer_pct') for d in mints_data], '{:.4f}', 'Top-5 buyer pct'))
    lines.append(_agg([d.get('buyer_concentration_ratio') for d in mints_data], '{:.4f}', 'Buyer concentration ratio'))

    # Coordination / sybil / manipulation signals
    lines.append(_agg([d.get('coordinated_buy_ratio') for d in mints_data], '{:.4f}', 'Coordinated buy ratio'))
    lines.append(_agg([d.get('repeat_buyer_ratio') for d in mints_data], '{:.4f}', 'Repeat buyer ratio'))
    lines.append(_agg([d.get('sybil_cluster_size') for d in mints_data], '{:.0f}', 'Sybil cluster size'))
    lines.append(_agg([d.get('toxic_flow_indicator') for d in mints_data], '{:.4f}', 'Toxic flow indicator'))
    lines.append(_agg([d.get('wash_trade_ratio') for d in mints_data], '{:.4f}', 'Wash trade ratio'))
    lines.append(_agg([d.get('largest_buy_pct_of_vol') for d in mints_data], '{:.4f}', 'Largest buy pct of vol'))

    # Liquidity / capacity
    lines.append(_agg([d.get('liquidity_sol') for d in mints_data], '{:.4f} SOL', 'Liquidity'))
    lines.append(_agg([d.get('liquidity_change_5s_sol') for d in mints_data], '{:.4f} SOL', 'Liq change 5s'))

    lines.append("")

    text = '\n'.join(lines)
    return {
        'text': text,
        'metadata': {
            'regime': regime_name,
            'sample_size': n,
        },
    }


def cpt_laserstream_microstructure(state_id, l1_rows, l2_row, l3_compressed=None):
    """Generate a CPT text block for LaserStream per-mint microstructure.
    Incorporates the LATENCY CORRECTION: collapse duplicate latency rows,
    state latency_effect_observed=false.
    """
    lines = []
    lines.append(f"State ID: {state_id}")

    # L1 microstructure
    if len(l1_rows) > 0:
        r = l1_rows.iloc[0]
        lines.append(f"Microstructure (L1):")
        lines.append(f"  Venue: {r.get('venue', 'N/A')}")
        lines.append(f"  Event type: {r.get('event_type', 'N/A')}")
        lines.append(f"  Price: {r.get('price_lamports_per_rawtok', 'N/A')}")
        lines.append(f"  Curve virtual SOL: {r.get('curve_virtual_sol', 'N/A')}")
        lines.append(f"  Curve virtual token: {r.get('curve_virtual_token', 'N/A')}")
        lines.append(f"  Curve complete: {r.get('curve_complete', 'N/A')}")
        lines.append(f"  Right-censored 1s: {r.get('right_censored_1s', 'N/A')}")
        lines.append(f"  Right-censored 300s: {r.get('right_censored_300s', 'N/A')}")

    # L2 outcome
    if l2_row is not None:
        o = l2_row
        lines.append(f"Outcome (L2):")
        lines.append(f"  Return 1s: {o.get('return_1s', 'N/A')}, 5s: {o.get('return_5s', 'N/A')}, 300s: {o.get('return_300s', 'N/A')}")
        lines.append(f"  MFE: {o.get('mfe_pct', 'N/A')}, MAE: {o.get('mae_pct', 'N/A')}")
        lines.append(f"  Migration outcome: {o.get('migration_outcome', 'N/A')}")
        lines.append(f"  Venue outcome: {o.get('venue_outcome', 'N/A')}")

    # L3 execution surface (WITH LATENCY CORRECTION)
    if l3_compressed:
        lc = l3_compressed
        lines.append(f"Execution surface (L3):")
        lines.append(f"  Feasibility ratio: {lc.get('feasibility_ratio', 'N/A')}")
        lines.append(f"  Median return: {lc.get('median_net_return_pct', 'N/A')}%")

        # SIZE SENSITIVITY — retained across 5 sizes
        size_returns = lc.get('size_returns', {})
        if size_returns:
            lines.append(f"  Size sensitivity (5 sizes):")
            for sz in sorted(size_returns.keys()):
                lines.append(f"    {sz} SOL: {size_returns[sz]:.2f}%")

        # CAPACITY — genuine examples kept
        cap_sizes = lc.get('capacity_constrained_sizes', [])
        if cap_sizes:
            lines.append(f"  Capacity-constrained sizes: {cap_sizes}")

        # LATENCY CORRECTION — provenance preserved, effect observed=false
        lines.append(f"  Latency scenarios tested: {LS_LATENCIES}ms (0/100/500/1000/2000)")
        lat_sens = lc.get('latency_sensitivity')
        lines.append(f"  latency_sensitivity: {lat_sens if lat_sens is not None else 'NULL (infeasible)'}")
        lines.append(f"  latency_effect_observed: false (sim_return_pct invariant across all 5 latency scenarios in this simulator)")
        lines.append(f"  Note: This dataset does not provide empirical latency supervision. "
                     f"Real latency behavior requires live/shadow data or a later counterfactual simulator.")

    return '\n'.join(lines)


def cpt_rust_code_change(change_record):
    """Generate a CPT text block from a Rust code change record."""
    lines = []
    sha = change_record.get('commit_sha', 'unknown')[:12]
    lines.append(f"Commit: {sha}")
    lines.append(f"Message: {change_record.get('commit_message', 'N/A')[:200]}")
    lines.append(f"Subsystem: {change_record.get('subsystem', 'N/A')}")
    lines.append(f"File: {change_record.get('filepath', 'N/A')}")
    lines.append(f"Classification: {change_record.get('commit_classification', 'N/A')}")

    diff = change_record.get('diff', '')
    if diff:
        # Cap diff at ~2000 chars for token budget
        if len(diff) > 2000:
            diff = diff[:2000] + "\n... (truncated)"
        lines.append(f"Diff:\n{diff}")

    return '\n'.join(lines)


def cpt_rust_repair(repair_record):
    """Generate a CPT text block from a Rust repair record."""
    lines = []
    lines.append(f"Repair pattern:")
    lines.append(f"  Bug: {repair_record.get('bug_commit_message', 'N/A')[:150]}")
    lines.append(f"  Fix: {repair_record.get('fix_commit_message', 'N/A')[:150]}")
    lines.append(f"  Subsystem: {repair_record.get('subsystem', 'N/A')}")
    lines.append(f"  Repair type: {repair_record.get('repair_type', 'N/A')}")
    lines.append(f"  Time gap: {repair_record.get('time_gap_seconds', 'N/A')}s")
    lines.append(f"  Bug classification: {repair_record.get('bug_classification', 'N/A')}")
    lines.append(f"  Fix classification: {repair_record.get('fix_classification', 'N/A')}")

    return '\n'.join(lines)


def cpt_rust_trajectory(traj_record):
    """Generate a CPT text block from a Rust trajectory record."""
    lines = []
    lines.append(f"Engineering trajectory:")
    lines.append(f"  Problem: {traj_record.get('problem_message', 'N/A')[:200]}")
    lines.append(f"  Investigation: {traj_record.get('investigation', 'N/A')[:300] if traj_record.get('investigation') else 'N/A'}")
    lines.append(f"  Attempted: {traj_record.get('attempted_change', 'N/A')[:200] if traj_record.get('attempted_change') else 'N/A'}")
    lines.append(f"  Result: {traj_record.get('result', 'N/A')[:200] if traj_record.get('result') else 'N/A'}")
    lines.append(f"  Repair count: {traj_record.get('repair_count', 'N/A')}")
    lines.append(f"  Subsystems: {traj_record.get('repair_subsystems', [])}")

    return '\n'.join(lines)


def cpt_narrative_trajectory(rec):
    """Generate CPT text from a narrative trajectory record (A+B scope only)."""
    lines = []
    lines.append(f"Creator reasoning trajectory:")
    creator = rec.get('creator', 'N/A')
    lines.append(f"  Creator: {hashlib.sha256(str(creator).encode()).hexdigest()[:8] if creator != 'N/A' else 'N/A'}")
    lines.append(f"  Scope: {rec.get('scope_tier', 'N/A')}")
    lines.append(f"  Temporal class: {rec.get('temporal_class', 'N/A')}")
    lines.append(f"  Post count: {rec.get('post_count', 'N/A')}")
    lines.append(f"  Stages: {rec.get('stage_sequence', [])}")

    # Primary mint (hashed)
    mint = rec.get('primary_mint', '')
    if mint:
        lines.append(f"  Primary mint: {hashlib.sha256(str(mint).encode()).hexdigest()[:12]}")

    lines.append(f"  Admission status: {rec.get('admission_status', 'N/A')}")
    lines.append(f"  Note: This is human reasoning/context, NOT ground truth. "
                 f"Narrative provides strategy language, not causal labels.")

    return '\n'.join(lines)


def cpt_narrative_content(rec):
    """Generate CPT text from a narrative creator content record (A+B scope only)."""
    lines = []
    lines.append(f"Creator content:")
    lines.append(f"  Platform: {rec.get('platform', 'N/A')}")
    lines.append(f"  Scope: {rec.get('scope_tier', 'N/A')}")
    lines.append(f"  Temporal class: {rec.get('temporal_class', 'N/A')}")

    text = rec.get('normalized_text') or rec.get('raw_text', '')
    if text:
        if len(text) > 1500:
            text = text[:1500] + "..."
        lines.append(f"  Content: {text}")

    lines.append(f"  Note: Creator content is reasoning/context. NOT ground truth. "
                 f"AMBIGUOUS/RETROSPECTIVE entries never serve as causal decision targets.")

    return '\n'.join(lines)


def cpt_narrative_strategy_card(rec):
    """Generate CPT text from a narrative strategy card."""
    lines = []
    lines.append(f"Strategy card:")
    lines.append(f"  Setup type: {rec.get('setup_type', 'N/A')}")
    lines.append(f"  Narrative theme: {rec.get('narrative_theme', 'N/A')}")
    lines.append(f"  Direction: {rec.get('direction', 'N/A')}")
    lines.append(f"  Entry timing: {rec.get('entry_timing', 'N/A')}")
    lines.append(f"  Entry price avg: {rec.get('entry_price_sol_avg', 'N/A')}")
    lines.append(f"  Exit target avg: {rec.get('exit_target_sol_avg', 'N/A')}")
    lines.append(f"  Scope: {rec.get('scope_tier', 'N/A')}")
    lines.append(f"  Note: Strategy cards are thesis templates, NOT ground truth.")

    return '\n'.join(lines)


# ════════════════════════════════════════════════════════════════
# SFT TEXT GENERATORS (instruction-following format)
# ════════════════════════════════════════════════════════════════

SFT_INSTRUCTION_CROSS_SECTIONAL = (
    "You are a Pump.fun trading decision brain. Given this cross-sectional panel of "
    "live candidates with causal state only, rank them by robust executable utility and "
    "assign BUY/WATCH/SKIP. If no candidate has robust positive evidence, output NO-BUY."
)

SFT_INSTRUCTION_EXECUTION_SURFACE = (
    "You are a Pump.fun trading decision brain. This panel includes full execution-surface "
    "detail. Analyze feasibility, capacity constraints, and size sensitivity. Rank candidates "
    "by robust executable utility. Note: latency scenarios were tested (0/100/500/1000/2000ms) "
    "but sim_return_pct was invariant across all latencies in this simulator — do not infer "
    "that latency does not matter in real production."
)

SFT_INSTRUCTION_POSTMORTEM = (
    "You are a Pump.fun trading decision brain reviewing a postmortem. Compare two similar "
    "candidates with different outcomes. Identify the causal features that distinguished runner "
    "from rug. Explain what a robust utility function should weight differently."
)

SFT_INSTRUCTION_RUST = (
    "You are an engineer maintaining a high-frequency Solana Pump.fun/PumpSwap trading bot in Rust. "
    "Analyze this code change, repair, or trajectory. Explain the engineering decision and its rationale."
)

SFT_INSTRUCTION_NARRATIVE = (
    "You are a Pump.fun trading decision brain reviewing creator reasoning and strategy. "
    "Analyze this narrative as context/reasoning only — NOT as ground truth. Identify which "
    "causal features the narrative emphasizes and which it ignores. Never treat narrative as a "
    "causal decision target."
)

SFT_INSTRUCTION_HERMES = (
    "You are a Pump.fun trading decision brain interfacing with the Hermes agent system. "
    "Format your output as a continuous utility ranking with BUY/WATCH/SKIP labels. "
    "Report capacity and feasibility constraints in standardized format. "
    "Request additional causal state when inputs are incomplete."
)


def sft_cross_sectional_example(panel_candidates, outcome_details, panel_id, source='slinky',
                                l3_detail='compressed', narrative_context=None):
    """Build a cross-sectional decision SFT example.
    Incorporates LATENCY CORRECTION for LaserStream panels.
    """
    candidates_json = []
    for c in panel_candidates:
        cand = {
            'mint_id': hashlib.sha256(str(c.get('mint', c.get('mint_b58', ''))).encode()).hexdigest()[:12],
            'causal_state': {k: v for k, v in c.items()
                             if k in ['curve_pct_depleted', 'trade_velocity_30s', 'buy_pressure',
                                      'toxic_flow_indicator', 'holder_concentration_hhi',
                                      'unique_wallets_so_far', 'wash_trade_ratio', 'curve_complete',
                                      'venue', 'price_lamports_per_rawtok', 'curve_virtual_sol',
                                      'curve_virtual_token', 'right_censored_1s', 'right_censored_300s']
                             and v is not None and (not isinstance(v, float) or not math.isnan(v))},
        }
        # L3 compressed for LS
        l3c = c.get('l3_compressed')
        if l3c:
            source_specific = {
                'feasibility_ratio': l3c.get('feasibility_ratio'),
                'median_net_return_pct': l3c.get('median_net_return_pct'),
                'size_sensitivity': l3c.get('size_sensitivity'),
                'capacity_constrained_sizes': l3c.get('capacity_constrained_sizes', []),
                # LATENCY CORRECTION
                'latency_sensitivity': l3c.get('latency_sensitivity') if l3c.get('latency_sensitivity') is not None else None,
                'latency_effect_observed': False,
                'latency_scenarios_tested': LS_LATENCIES,
            }
            cand['source_specific'] = source_specific

        # Slinky CF compressed
        sfc = c.get('slinky_cf_compressed')
        if sfc:
            cand['source_specific'] = {
                'cf_250ms_mid': sfc.get('cf_250ms_mid'),
                'cf_250ms_large': sfc.get('cf_250ms_large'),
                'size_sensitivity': sfc.get('size_sensitivity'),
                'latency_sensitivity': None,  # Slinky only has 250ms
                'latency_effect_observed': None,  # Not applicable to Slinky
            }
        candidates_json.append(cand)

    # Output: continuous rankings
    rankings = []
    for i, c in enumerate(panel_candidates):
        od = outcome_details[i] if i < len(outcome_details) else {}
        mint_hash = hashlib.sha256(str(c.get('mint', c.get('mint_b58', ''))).encode()).hexdigest()[:12]
        decision = od.get('decision', 'WATCH')
        utility = od.get('robust_utility', 0.0)

        rationale_parts = []
        if od.get('mfe_bp'):
            rationale_parts.append(f"MFE {od['mfe_bp']:.0f}bp")
        if od.get('mae_bp'):
            rationale_parts.append(f"MAE {od['mae_bp']:.0f}bp")
        if od.get('feasibility_ratio'):
            rationale_parts.append(f"feasibility {od['feasibility_ratio']:.2f}")
        if od.get('capacity_constrained_sizes'):
            rationale_parts.append(f"capacity-constrained at {od['capacity_constrained_sizes']}")
        # LATENCY CORRECTION in rationale
        l3c = c.get('l3_compressed')
        if l3c:
            rationale_parts.append("latency-invariant (outcome does not vary with execution delay in this simulator)")

        rankings.append({
            'mint_id': mint_hash,
            'rank': i + 1,
            'robust_executable_utility': round(utility, 4),
            'feasibility_ratio': od.get('feasibility_ratio'),
            'decision': decision,
            'rationale': '; '.join(rationale_parts) if rationale_parts else 'Insufficient evidence for confident ranking.',
        })

    no_buy = all(r['decision'] == 'SKIP' or r['robust_executable_utility'] < 0 for r in rankings)

    instruction = SFT_INSTRUCTION_CROSS_SECTIONAL
    if l3_detail == 'full':
        instruction = SFT_INSTRUCTION_EXECUTION_SURFACE

    input_data = {
        'panel_timestamp': panel_candidates[0].get('timestamp_ms', panel_candidates[0].get('event_time_unix_ms', 0)) if panel_candidates else 0,
        'panel_source': source,
        'panel_size': len(panel_candidates),
        'candidates': candidates_json,
    }
    if narrative_context:
        input_data['narrative_context'] = narrative_context

    return {
        'instruction': instruction,
        'input': input_data,
        'output': {
            'continuous_rankings': rankings,
            'no_buy_flag': no_buy,
        },
        'token_count': 0,  # filled in later
    }


def sft_postmortem_example(pair, panel_a, panel_b, outcome_a, outcome_b, pair_id):
    """Build a postmortem/contrast SFT example.

    `pair` schema (from find_divergent_pairs): candidate_a_idx,
    candidate_b_idx, causal_similarity, divergence_measure, ret_300s_a,
    ret_300s_b. `panel_a`/`panel_b` are the matched candidate lists.
    (The old body read pair['mint_a']/['similarity_score'] — keys that
    never existed — so every call raised KeyError into a bare except.)
    """
    cand_a = panel_a[0] if panel_a else {}
    cand_b = panel_b[0] if panel_b else {}
    mint_a = hashlib.sha256(str(cand_a.get('mint_id', '')).encode()).hexdigest()[:12]
    mint_b = hashlib.sha256(str(cand_b.get('mint_id', '')).encode()).hexdigest()[:12]
    sim = pair.get('causal_similarity', 0.0)

    input_data = {
        'candidate_a': {
            'mint_id': mint_a,
            'causal_state': cand_a.get('causal_state', {}),
        },
        'candidate_b': {
            'mint_id': mint_b,
            'causal_state': cand_b.get('causal_state', {}),
        },
        'causal_similarity': sim,
    }

    # Distinguishing features: causal fields with largest normalized gap
    # (causal-state inputs only — never outcome fields).
    cs_a, cs_b = cand_a.get('causal_state', {}), cand_b.get('causal_state', {})
    gaps = []
    for k in cs_a:
        va, vb = cs_a.get(k), cs_b.get(k)
        if isinstance(va, (int, float)) and isinstance(vb, (int, float)) \
                and va == va and vb == vb:
            gaps.append((abs(va - vb) / (abs(va) + abs(vb) + 1e-9), k))
    gaps.sort(reverse=True)
    distinguishing = [k for _, k in gaps[:3]]

    output = {
        'outcome_a': {
            'mfe_bp': outcome_a.get('mfe_bp'),
            'mae_bp': outcome_a.get('mae_bp'),
            'graduated': outcome_a.get('graduated_after_state', False),
            'collapsed': outcome_a.get('collapsed_50pct_within_300s', False),
        },
        'outcome_b': {
            'mfe_bp': outcome_b.get('mfe_bp'),
            'mae_bp': outcome_b.get('mae_bp'),
            'graduated': outcome_b.get('graduated_after_state', False),
            'collapsed': outcome_b.get('collapsed_50pct_within_300s', False),
        },
        'divergence_measure': pair.get('divergence_measure'),
        'divergence_a': pair.get('ret_300s_a'),
        'divergence_b': pair.get('ret_300s_b'),
        'distinguishing_features': distinguishing,
        'lesson': f"Despite {sim:.2f} causal similarity, "
                  f"outcomes diverged. The utility function must weight {distinguishing[0] if distinguishing else 'unknown'} "
                  f"to distinguish runner from rug.",
    }

    return {
        'instruction': SFT_INSTRUCTION_POSTMORTEM,
        'input': input_data,
        'output': output,
        'token_count': 0,
    }


def sft_rust_example(record, record_type='code_change'):
    """Build a Rust engineering SFT example."""
    if record_type == 'code_change':
        instruction = SFT_INSTRUCTION_RUST
        diff = record.get('diff', '')
        if diff and len(diff) > 2000:
            diff = diff[:2000] + "\n... (truncated)"
        input_data = {
            'commit_sha': record.get('commit_sha', '')[:12],
            'commit_message': record.get('commit_message', ''),
            'filepath': record.get('filepath', ''),
            'subsystem': record.get('subsystem', ''),
            'diff': diff,
        }
        output = {
            'analysis': f"Change to {record.get('subsystem', 'unknown')} subsystem. "
                       f"Classification: {record.get('commit_classification', 'N/A')}. "
                       f"Hunk count: {record.get('hunk_count', 0)}. "
                       f"File: {record.get('filepath', 'N/A')}.",
        }
    elif record_type == 'repair':
        instruction = SFT_INSTRUCTION_RUST
        input_data = {
            'bug': record.get('bug_commit_message', ''),
            'fix': record.get('fix_commit_message', ''),
            'subsystem': record.get('subsystem', ''),
            'repair_type': record.get('repair_type', ''),
            'time_gap_seconds': record.get('time_gap_seconds', 0),
        }
        output = {
            'analysis': f"Bug in {record.get('subsystem', 'unknown')} was fixed via {record.get('repair_type', 'unknown')}. "
                       f"Bug classification: {record.get('bug_classification', 'N/A')}. "
                       f"Fix classification: {record.get('fix_classification', 'N/A')}. "
                       f"Time to fix: {record.get('time_gap_seconds', 0)}s.",
        }
    elif record_type == 'trajectory':
        instruction = SFT_INSTRUCTION_RUST
        input_data = {
            'problem': record.get('problem_message', ''),
            'investigation': record.get('investigation', ''),
            'attempted': record.get('attempted_change', ''),
            'result': record.get('result', ''),
            'repair_count': record.get('repair_count', 0),
        }
        output = {
            'analysis': f"Trajectory involved {record.get('repair_count', 0)} repairs across "
                       f"subsystems: {record.get('repair_subsystems', [])}. "
                       f"Result classification: {record.get('result_classification', 'N/A')}.",
        }

    return {
        'instruction': instruction,
        'input': input_data,
        'output': output,
        'token_count': 0,
    }


def sft_narrative_example(rec, record_type='trajectory'):
    """Build a narrative strategy SFT example."""
    if record_type == 'trajectory':
        input_data = {
            'creator': hashlib.sha256(str(rec.get('creator', '')).encode()).hexdigest()[:8],
            'scope_tier': rec.get('scope_tier', 'A'),
            'temporal_class': rec.get('temporal_class', 'AMBIGUOUS'),
            'post_count': rec.get('post_count', 0),
            'stage_sequence': rec.get('stage_sequence', []),
        }
        output = {
            'analysis': f"Creator reasoning follows {len(rec.get('stage_sequence', []))} stages. "
                       f"Temporal class: {rec.get('temporal_class', 'N/A')}. "
                       f"This is reasoning/context, NOT ground truth. "
                       f"AMBIGUOUS/RETROSPECTIVE entries never serve as causal decision targets.",
        }
    elif record_type == 'content':
        text = rec.get('normalized_text') or rec.get('raw_text', '')
        if text and len(text) > 1000:
            text = text[:1000] + "..."
        input_data = {
            'platform': rec.get('platform', ''),
            'scope_tier': rec.get('scope_tier', 'A'),
            'temporal_class': rec.get('temporal_class', 'AMBIGUOUS'),
            'content': text,
        }
        output = {
            'analysis': f"Creator content on {rec.get('platform', 'N/A')}. "
                       f"Scope: {rec.get('scope_tier', 'N/A')}, Temporal: {rec.get('temporal_class', 'N/A')}. "
                       f"This is reasoning/context. NOT ground truth.",
        }
    elif record_type == 'strategy_card':
        input_data = {
            'setup_type': rec.get('setup_type', ''),
            'narrative_theme': rec.get('narrative_theme', ''),
            'direction': rec.get('direction', ''),
            'entry_timing': rec.get('entry_timing', ''),
        }
        output = {
            'analysis': f"Strategy: {rec.get('setup_type', 'N/A')} with {rec.get('direction', 'N/A')} direction. "
                       f"Entry timing: {rec.get('entry_timing', 'N/A')}. "
                       f"Thesis template, NOT ground truth.",
        }

    # Apply narrative dropout: ~40% of examples have narrative_context stripped
    # (handled at selection time, not here)

    return {
        'instruction': SFT_INSTRUCTION_NARRATIVE,
        'input': input_data,
        'output': output,
        'token_count': 0,
    }


def sft_hermes_tool_example(template_id):
    """Build a Hermes/tool interface SFT example from templates."""
    templates = [
        {
            'instruction': SFT_INSTRUCTION_HERMES,
            'input': {
                'scenario': 'You receive causal state from LaserStream for 35 candidates. '
                           '5 have incomplete right_censored fields. Format your response.',
            },
            'output': {
                'response': 'For 30 complete candidates: rank by robust_executable_utility. '
                           'For 5 incomplete: flag as INSUFFICIENT_DATA and request '
                           'right_censored_5s and right_censored_300s. '
                           'No candidate receives BUY without complete causal state.',
            },
        },
        {
            'instruction': SFT_INSTRUCTION_HERMES,
            'input': {
                'scenario': 'A candidate has capacity_constrained at sizes >= 0.25 SOL. '
                           'Only feasible at 0.05 SOL. Report in standardized format.',
            },
            'output': {
                'response': 'max_feasible_size_sol: 0.05. capacity_constrained_sizes: [0.25, 0.50, 1.0]. '
                           'feasibility_ratio: 0.20 (1/5 sizes feasible). '
                           'Decision: WATCH (not BUY) due to severe capacity constraint.',
            },
        },
        {
            'instruction': SFT_INSTRUCTION_HERMES,
            'input': {
                'scenario': 'LaserStream L3 shows sim_return_pct invariant across 0/100/500/1000/2000ms. '
                           'How should the decision brain handle latency information?',
            },
            'output': {
                'response': 'latency_effect_observed: false. '
                           'This simulator does not provide empirical latency supervision. '
                           'Do not infer that 0ms = 2000ms in real production. '
                           'Real latency behavior requires live/shadow data or a later counterfactual simulator. '
                           'Retain size sensitivity (which DOES vary) for capacity awareness.',
            },
        },
        {
            'instruction': SFT_INSTRUCTION_HERMES,
            'input': {
                'scenario': 'All 35 candidates have negative robust utility. '
                           'No candidate has positive net-SOL economics. Output decision.',
            },
            'output': {
                'response': 'no_buy_flag: true. '
                           'All candidates assigned SKIP. '
                           'Panel-level decision: NO-BUY. '
                           'No position taken. Rationale: no robust positive evidence.',
            },
        },
    ]
    return templates[template_id % len(templates)]
