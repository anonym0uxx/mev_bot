# ═══ EXPERT REVIEW BUNDLE ═══
# This file is for review only - do NOT execute
# ═══ SECTION 1: BUILDER HEADER (imports + constants) ═══
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
from collections import Counter, defaultdict
import pyarrow.parquet as pq
import pandas as pd

# ════════════════════════════════════════════════════════════════
# CONFIG
# ════════════════════════════════════════════════════════════════

BASE = 'D:/repos/mev_bot/tools/data-pipeline/output'
OUT = os.path.join(BASE, 'qwen_curriculum_v1')
EVAL_DIR = os.path.join(OUT, 'eval')
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

# ═══ SECTION 2: cpt_sft_exporters.py (text generators) ═══
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


def estimate_tokens(text):
    """Rough token estimate: ~4 chars per token."""
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

def cpt_slinky_trajectory_summary(mint, state_rows, outcome_row, cf_rows=None):
    """Generate a CPT text block for a Slinky per-mint trajectory summary.
    Uses causal state + outcome targets (NOT leakage fields like ret_1s as inputs).
    """
    lines = []
    # Mint identity (hashed)
    mint_hash = hashlib.sha256(mint.encode()).hexdigest()[:12]
    lines.append(f"Token: {mint_hash}")
    lines.append(f"Venue: Pump.fun bonding curve")

    # Causal state summary (first/last row)
    if len(state_rows) > 0:
        row = state_rows.iloc[0]
        lines.append(f"Launch context:")
        lines.append(f"  Initial market cap: {row.get('initial_market_cap_sol', 'N/A')} SOL")
        lines.append(f"  Initial supply: {row.get('initial_supply_raw', 'N/A')}")
        lines.append(f"  Creator: {hashlib.sha256(str(row.get('creator', '')).encode()).hexdigest()[:8] if row.get('creator') else 'N/A'}")
        lines.append(f"  Creator past tokens: {row.get('creator_past_tokens', 'N/A')}, past rugs: {row.get('creator_past_rugs', 'N/A')}")

        # Final state
        last = state_rows.iloc[-1]
        lines.append(f"State at observation end:")
        lines.append(f"  Curve pct depleted: {last.get('curve_pct_depleted', 'N/A'):.4f}" if pd.notna(last.get('curve_pct_depleted')) else "  Curve pct depleted: N/A")
        lines.append(f"  Total trades: {last.get('trade_count_so_far', 'N/A')}")
        lines.append(f"  Unique wallets: {last.get('unique_wallets_so_far', 'N/A')}")
        lines.append(f"  Buy pressure: {last.get('buy_pressure', 'N/A'):.4f}" if pd.notna(last.get('buy_pressure')) else "  Buy pressure: N/A")
        lines.append(f"  Toxic flow: {last.get('toxic_flow_indicator', 'N/A'):.4f}" if pd.notna(last.get('toxic_flow_indicator')) else "  Toxic flow: N/A")
        lines.append(f"  Holder HHI: {last.get('holder_concentration_hhi', 'N/A'):.4f}" if pd.notna(last.get('holder_concentration_hhi')) else "  Holder HHI: N/A")
        lines.append(f"  Wash trade ratio: {last.get('wash_trade_ratio', 'N/A'):.4f}" if pd.notna(last.get('wash_trade_ratio')) else "  Wash trade ratio: N/A")

    # Outcome (TARGET evidence, not input)
    if outcome_row is not None:
        o = outcome_row
        lines.append(f"Outcome:")
        mfe = o.get('mfe_bp')
        mae = o.get('mae_bp')
        lines.append(f"  MFE: {mfe:.0f}bp, MAE: {mae:.0f}bp" if pd.notna(mfe) and pd.notna(mae) else "  MFE/MAE: N/A")
        grad = o.get('graduated_after_state', False)
        collapsed = o.get('collapsed_50pct_within_300s', False)
        survived_300s = o.get('survived_300s', False)
        lines.append(f"  Graduated: {grad}, Collapsed 50% within 300s: {collapsed}, Survived 300s: {survived_300s}")

    # Counterfactual (size sensitivity at 250ms)
    if cf_rows is not None and len(cf_rows) > 0:
        lines.append(f"Counterfactual economics (250ms execution, 5 sizes):")
        for _, cf in cf_rows.iterrows():
            sz = cf.get('size', cf.get('trade_size_sol', 'N/A'))
            ret = cf.get('cf_return', cf.get('cf_return_pct', 'N/A'))
            if pd.notna(ret):
                lines.append(f"  Size {sz} SOL: return {ret:.2f}%")
            else:
                lines.append(f"  Size {sz} SOL: infeasible")

    return '\n'.join(lines)


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
    """Generate a CPT text block for a regime pattern catalog."""
    lines = []
    lines.append(f"Regime: {regime_name}")
    lines.append(f"Sample size: {len(mints_data)} mints")
    lines.append("")

    # Aggregate stats
    total_mfe = sum(d.get('mfe_bp', 0) for d in mints_data if pd.notna(d.get('mfe_bp')))
    total_mae = sum(d.get('mae_bp', 0) for d in mints_data if pd.notna(d.get('mae_bp')))
    n_grad = sum(1 for d in mints_data if d.get('graduated_after_state'))
    n_collapsed = sum(1 for d in mints_data if d.get('collapsed_50pct_within_300s'))

    lines.append(f"Aggregate outcomes:")
    lines.append(f"  Mean MFE: {total_mfe/max(1,len(mints_data)):.0f}bp")
    lines.append(f"  Mean MAE: {total_mae/max(1,len(mints_data)):.0f}bp")
    lines.append(f"  Graduated: {n_grad}/{len(mints_data)} ({100*n_grad/max(1,len(mints_data)):.1f}%)")
    lines.append(f"  Collapsed 50%: {n_collapsed}/{len(mints_data)} ({100*n_collapsed/max(1,len(mints_data)):.1f}%)")

    # Typical causal patterns
    avg_velocity = np.mean([d.get('trade_velocity_30s', 0) for d in mints_data if pd.notna(d.get('trade_velocity_30s'))]) if mints_data else 0
    avg_hhi = np.mean([d.get('holder_concentration_hhi', 0) for d in mints_data if pd.notna(d.get('holder_concentration_hhi'))]) if mints_data else 0
    avg_toxic = np.mean([d.get('toxic_flow_indicator', 0) for d in mints_data if pd.notna(d.get('toxic_flow_indicator'))]) if mints_data else 0

    lines.append(f"Typical causal patterns:")
    lines.append(f"  Avg trade velocity (30s): {avg_velocity:.2f}")
    lines.append(f"  Avg holder HHI: {avg_hhi:.4f}")
    lines.append(f"  Avg toxic flow: {avg_toxic:.4f}")

    return '\n'.join(lines)


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
        lines.append(f"  Median return: {lc.get('median_return_pct', 'N/A')}%")

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
                'median_return_pct': l3c.get('median_return_pct'),
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
    """Build a postmortem/contrast SFT example."""
    mint_a = hashlib.sha256(str(pair['mint_a']).encode()).hexdigest()[:12]
    mint_b = hashlib.sha256(str(pair['mint_b']).encode()).hexdigest()[:12]

    sim_fields = pair.get('similarity_score', {})
    input_data = {
        'candidate_a': {
            'mint_id': mint_a,
            'causal_state': sim_fields.get('causal_state_a', {}),
        },
        'candidate_b': {
            'mint_id': mint_b,
            'causal_state': sim_fields.get('causal_state_b', {}),
        },
        'causal_similarity': pair.get('similarity_score', 0.0),
    }

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
        'distinguishing_features': pair.get('distinguishing_features', []),
        'lesson': f"Despite {pair.get('similarity_score', 0):.2f} causal similarity, "
                  f"outcomes diverged. The utility function must weight {pair.get('distinguishing_features', ['unknown'])[0] if pair.get('distinguishing_features') else 'unknown'} "
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

# ═══ SECTION 3: build_cpt_v1() orchestration ═══
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

    unique_mints = lw_idx['mint'].unique() if 'mint' in lw_idx.columns else []
    n_traj = min(3000, len(unique_mints))
    rng = random.Random(42)
    traj_mints = rng.sample(list(unique_mints), n_traj) if len(unique_mints) > n_traj else list(unique_mints)

    for mint in traj_mints:
        mint_rows = lw_idx[lw_idx['mint'] == mint]
        if len(mint_rows) == 0:
            continue
        file = mint_rows.iloc[0]['source_file']
        mints_by_file = {file: {mint}}
        causal = load_slinky_causal_for_mints(mints_by_file)
        state_rows = causal.get(file, {}).get(mint, None)
        if state_rows is None:
            continue
        outcome = _SLINKY_OUTCOMES_BY_MINT.get(mint) if _SLINKY_OUTCOMES_BY_MINT else None
        state_id = mint_rows.iloc[0].get('state_id', '')
        cf = _SLINKY_CF_BY_STATE.get(state_id) if _SLINKY_CF_BY_STATE else None

        text = _cpt_sft.cpt_slinky_trajectory_summary(mint, state_rows, outcome, cf)
        tokens = _cpt_sft.estimate_tokens(text)
        rec = {
            'id': f"cpt_slinky_{len(cpt_records):05d}",
            'format': 'packed_text',
            'source_corpus': 'slinky_gold_v3',
            'source_layer': 'pump_state_v3+pump_outcome_v3+counterfactual_trade_v3',
            'source_ids': [str(state_id)],
            'content': text,
            'provenance': _cpt_sft.make_cpt_provenance(
                'slinky_gold_v3', [str(state_id)], 'trajectory_summary',
                SLINKY_FREEZE_UUID, mint_ids=[mint]
            ),
            'token_count': tokens,
        }
        cpt_records.append(rec)
        source_counts['slinky_trajectory'] += 1
        token_total += tokens

    # Slinky panel snapshots
    time_idx = get_slinky_time_index()
    if time_idx:
        n_panels = min(500, len(time_idx))
        anchor_step = max(1, len(time_idx) // n_panels)
        for i in range(0, len(time_idx), anchor_step):
            if len(cpt_records) >= 7000:
                break
            file, min_ts, max_ts = time_idx[i]
            anchor_ms = int((min_ts + max_ts) / 2)
            try:
                panel = assemble_slinky_panel(anchor_ms, i, lw_idx=lw_idx)
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
    l1_path = os.path.join(LS, 'l1_microstructure_v3.parquet')
    l2_path = os.path.join(LS, 'l2_outcome_v3.parquet')
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
        l1_rows = l1_by_state.get(sid)
        l2_row = l2_by_state.get(sid)
        l3c = l3_by_state.get(sid)
        if l1_rows is None and l2_row is None:
            continue
        text = _cpt_sft.cpt_laserstream_microstructure(sid, l1_rows or pd.DataFrame(), l2_row, l3c)
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



# ═══ SECTION 4: build_sft_v1() orchestration ═══
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

    n_cross = targets['cross_sectional_compressed']
    time_idx = get_slinky_time_index()
    cross_built = 0
    if time_idx and len(time_idx) > 0:
        step = max(1, len(time_idx) // n_cross)
        for i in range(0, len(time_idx), step):
            if cross_built >= n_cross:
                break
            file, min_ts, max_ts = time_idx[i]
            anchor_ms = int((min_ts + max_ts) / 2)
            try:
                panel = assemble_slinky_panel(anchor_ms, i, lw_idx=lw_idx)
                if panel and len(panel.get('candidates', [])) >= 3:
                    outcome_details = []
                    for c in panel['candidates']:
                        mint = c.get('mint', '')
                        od = _SLINKY_OUTCOMES_BY_MINT.get(mint) if _SLINKY_OUTCOMES_BY_MINT else {}
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
        l1_path = os.path.join(LS, 'l1_microstructure_v3.parquet')
        l1_pq = pq.read_table(l1_path, columns=['state_id', 'timestamp_ms'])
        ls_state_ids = list(set(l1_pq.column('state_id').to_pylist()))
        needed = n_cross - cross_built
        rng3 = random.Random(77)
        ls_sample = rng3.sample(ls_state_ids, min(needed, len(ls_state_ids)))
        for sid in ls_sample:
            if cross_built >= n_cross:
                break
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
        scenarios = l3_df[l3_df['state_id'] == sid].to_dict('records')
        l3c = compute_l3_compressed(scenarios)
        candidates = [{'mint': str(sid), 'state_id': str(sid), 'l3_compressed': l3c, 'venue': 'pump.fun'}]
        outcome_details = [{'robust_utility': (l3c.get('median_return_pct', 0) or 0) / 100,
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
    if time_idx and len(time_idx) > 0:
        step = max(1, len(time_idx) // (n_post * 2))
        for i in range(0, len(time_idx), step):
            if post_built >= n_post:
                break
            file, min_ts, max_ts = time_idx[i]
            anchor_ms = int((min_ts + max_ts) / 2)
            try:
                panel = assemble_slinky_panel(anchor_ms, i, lw_idx=lw_idx)
                if panel and len(panel.get('candidates', [])) >= 5:
                    pairs = find_divergent_pairs(panel)
                    for pair in pairs[:2]:
                        if post_built >= n_post:
                            break
                        mint_a = pair['mint_a']
                        mint_b = pair['mint_b']
                        od_a = _SLINKY_OUTCOMES_BY_MINT.get(mint_a, {}) if _SLINKY_OUTCOMES_BY_MINT else {}
                        od_b = _SLINKY_OUTCOMES_BY_MINT.get(mint_b, {}) if _SLINKY_OUTCOMES_BY_MINT else {}
                        cands_a = [c for c in panel['candidates'] if c.get('mint') == mint_a]
                        cands_b = [c for c in panel['candidates'] if c.get('mint') == mint_b]
                        ex = _cpt_sft.sft_postmortem_example(
                            pair, cands_a, cands_b, od_a, od_b,
                            f"postmortem_{post_built}"
                        )
                        ex['token_count'] = _cpt_sft.estimate_tokens(json.dumps(ex))
                        ex['id'] = f"sft_post_{post_built:04d}"
                        ex['source_corpus'] = 'slinky_gold_v3'
                        ex['provenance'] = _cpt_sft.make_sft_provenance(
                            'slinky_gold_v3', [mint_a, mint_b], SLINKY_FREEZE_UUID,
                            contrast_pair_id=f"pair_{post_built}", eval_split='train'
                        )
                        sft_records.append(ex)
                        source_counts['postmortem'] += 1
                        token_total += ex['token_count']
                        post_built += 1
            except Exception:
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

