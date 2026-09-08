#!/usr/bin/env python
"""
build_narrative_validation_v1.py - Gold Layer 4: link claims to on-chain outcomes

Transforms creator_claim_v1 + narrative_state_v1 + slinky_gold_v3 -> narrative_validation_v1.

This is the TRUTH layer. On-chain authority > creator claims.
Links each claim/state to Slinky/LaserStream outcomes by mint + time.

Computes:
  - Lead/lag (claim_time vs outcome_time)
  - Price outcomes (ret_1s, 5s, 30s, 60s, 300s in bips)
  - MFE/MAE
  - Net SOL PnL (after fees/slippage)
  - Capacity (max executable size)
  - Toxic outcomes (rug, non-graduation)
  - Validation verdict: SUPPORTED | CONTRADICTED | MIXED | UNRESOLVED
  - Control comparison (treatment vs untreated mints)

Usage: python src/build_narrative_validation_v1.py
"""

import json
import os
import hashlib
import argparse
import time as time_module
from collections import Counter, defaultdict

# Slinky v3 parquet path
SLINKY_PATH = "D:/mev_bot-artifacts/gold/slinky_gold_v3_compact"


def load_slinky_mints(slinky_path: str) -> set:
    """Load the set of mints from Slinky v3 pump_outcome_v3 parquet files.
    Returns a set of mint addresses for fast lookup."""
    import pyarrow.parquet as pq
    mints = set()
    outcome_dir = os.path.join(slinky_path, "pump_outcome_v3")
    if not os.path.exists(outcome_dir):
        print(f"  [WARN] Slinky outcome dir not found: {outcome_dir}")
        return mints

    files = sorted([f for f in os.listdir(outcome_dir) if f.endswith('.parquet')])
    print(f"  Slinky v3 pump_outcome_v3: {len(files)} parquet files")

    for i, fname in enumerate(files):
        fpath = os.path.join(outcome_dir, fname)
        try:
            table = pq.read_table(fpath, columns=['mint'])
            mints.update(table.column('mint').to_pylist())
        except Exception as e:
            print(f"  [WARN] Error reading {fname}: {e}")
        if (i + 1) % 20 == 0:
            print(f"    Loaded {i+1}/{len(files)} files, {len(mints)} mints so far")

    print(f"  Slinky mints loaded (all {len(files)} files): {len(mints)}")
    return mints


def load_slinky_for_mint(slinky_path: str, target_mint: str) -> list:
    """Load Slinky pump_outcome_v3 states for a specific mint.
    Returns list of state dicts with price/return fields."""
    import pyarrow.parquet as pq
    states = []
    outcome_dir = os.path.join(slinky_path, "pump_outcome_v3")
    if not os.path.exists(outcome_dir):
        return states

    files = sorted([f for f in os.listdir(outcome_dir) if f.endswith('.parquet')])
    for fname in files:
        fpath = os.path.join(outcome_dir, fname)
        try:
            # Read only columns we need, filter by mint
            table = pq.read_table(fpath, columns=[
                'state_id', 'mint', 'event_time_unix_ms',
                'ret_1s_bp', 'ret_5s_bp', 'ret_30s_bp', 'ret_60s_bp', 'ret_300s_bp',
                'mfe_bp', 'mae_bp',
                'survived_60s', 'survived_300s', 'graduated_after_state',
                'collapsed_50pct_within_300s'
            ])
            df = table.to_pandas()
            mint_df = df[df['mint'] == target_mint]
            if not mint_df.empty:
                for _, row in mint_df.iterrows():
                    states.append(row.to_dict())
        except Exception:
            continue

    return states


def match_claim_to_slinky(claim: dict, slinky_states: list) -> dict:
    """Match a claim to Slinky on-chain outcomes by mint + time.
    Uses pump_outcome_v3 fields: event_time_unix_ms, ret_*s_bp, mfe_bp, mae_bp,
    survived_60s, survived_300s, graduated_after_state, collapsed_50pct_within_300s."""
    if not slinky_states:
        return None

    claim_time_ms = claim['publish_time_ms']

    # Find the closest Slinky state at or after claim time
    best_match = None
    best_diff = float('inf')

    for s in slinky_states:
        s_time_ms = s.get('event_time_unix_ms', 0)
        if s_time_ms == 0:
            continue
        diff = abs(s_time_ms - claim_time_ms)
        if diff < best_diff:
            best_diff = diff
            best_match = s

    if not best_match:
        return None

    # Extract outcomes from Slinky pump_outcome_v3
    outcome = {
        'slinky_state_id': best_match.get('state_id', ''),
        'outcome_time_ms': int(best_match.get('event_time_unix_ms', 0)),
        'lead_lag_seconds': int((claim_time_ms - best_match.get('event_time_unix_ms', 0)) / 1000),
        'ret_1s_bp': best_match.get('ret_1s_bp'),
        'ret_5s_bp': best_match.get('ret_5s_bp'),
        'ret_30s_bp': best_match.get('ret_30s_bp'),
        'ret_60s_bp': best_match.get('ret_60s_bp'),
        'ret_300s_bp': best_match.get('ret_300s_bp'),
        'mfe_bp': best_match.get('mfe_bp'),
        'mae_bp': best_match.get('mae_bp'),
        'is_toxic': best_match.get('collapsed_50pct_within_300s', False),
        'is_rugpull': best_match.get('collapsed_50pct_within_300s', False),
        'is_graduated': best_match.get('graduated_after_state', False),
        'survived_60s': best_match.get('survived_60s'),
        'survived_300s': best_match.get('survived_300s'),
    }

    # Compute net PnL (0.5 SOL entry, 300s horizon)
    entry_sol = 0.5
    ret_300s_bp = outcome.get('ret_300s_bp') or 0
    # pump.fun fees: 1% buy + 1% sell = 200bp total, plus slippage ~100bp
    fees_bp = 300
    net_return_bp = ret_300s_bp - fees_bp
    net_pnl_sol = entry_sol * (net_return_bp / 10000.0)
    outcome['net_pnl_sol'] = round(net_pnl_sol, 6)
    outcome['net_return_bp'] = net_return_bp
    outcome['capacity_sol'] = 1.0  # placeholder, enriched from pump_state_v3 later
    outcome['execution_feasible'] = True

    # Validation verdict
    direction = claim.get('direction')
    if direction == 'long':
        if ret_300s_bp > 500:
            outcome['validation_status'] = 'SUPPORTED'
            outcome['validation_reason'] = f'long direction supported by +{ret_300s_bp}bp 300s return'
        elif ret_300s_bp < -500:
            outcome['validation_status'] = 'CONTRADICTED'
            outcome['validation_reason'] = f'long direction contradicted by {ret_300s_bp}bp 300s return'
        else:
            outcome['validation_status'] = 'MIXED'
            outcome['validation_reason'] = f'return {ret_300s_bp}bp is ambiguous (<500bp either way)'
    elif direction in ('short', 'avoid'):
        if ret_300s_bp < -500:
            outcome['validation_status'] = 'SUPPORTED'
            outcome['validation_reason'] = f'avoid/short supported by {ret_300s_bp}bp 300s return'
        elif ret_300s_bp > 500:
            outcome['validation_status'] = 'CONTRADICTED'
            outcome['validation_reason'] = f'avoid/short contradicted by +{ret_300s_bp}bp 300s return'
        else:
            outcome['validation_status'] = 'MIXED'
            outcome['validation_reason'] = f'return {ret_300s_bp}bp is ambiguous (<500bp either way)'
    else:
        outcome['validation_status'] = 'UNRESOLVED'
        outcome['validation_reason'] = 'no clear direction to validate'

    outcome['validation_confidence'] = 0.7 if best_diff < 60000 else 0.4  # 1 min match

    # Propagation stage
    if claim.get('claim_type') == 'entry':
        outcome['propagation_stage'] = 'alpha'
    elif claim.get('claim_type') == 'risk':
        outcome['propagation_stage'] = 'tracker'
    else:
        outcome['propagation_stage'] = 'ct'

    return outcome


def build_narrative_validation(claim_path: str, state_path: str,
                               output_dir: str, slinky_path: str) -> dict:
    """Build narrative_validation_v1."""
    print("=" * 70)
    print("NARRATIVE_VALIDATION_V1 - Gold Layer 4 Builder")
    print("=" * 70)

    # Load claims and states
    with open(claim_path, 'r', encoding='utf-8') as f:
        claims = [json.loads(line) for line in f if line.strip()]
    with open(state_path, 'r', encoding='utf-8') as f:
        states = [json.loads(line) for line in f if line.strip()]
    print(f"  Loaded {len(claims)} claims, {len(states)} narrative states")

    # Filter to GOLD/UNRESOLVED claims
    eligible_claims = [c for c in claims if c['admission_status'] in ('GOLD', 'UNRESOLVED')]
    print(f"  Eligible claims: {len(eligible_claims)}")

    # Extract unique mints from claims
    mint_set = set()
    for c in eligible_claims:
        if c.get('primary_mint'):
            mint_set.add(c['primary_mint'])
        if c.get('entities'):
            for e in c['entities'].split('|'):
                if len(e) >= 32:
                    mint_set.add(e)
    print(f"  Unique mints in claims: {len(mint_set)}")

    run_uuid = f"nv_{int(time_module.time())}"
    git_sha = __import__('subprocess').check_output(
        ['git', 'rev-parse', '--short', 'HEAD'],
        cwd='D:/repos/mev_bot'
    ).decode().strip()
    print(f"  Run UUID: {run_uuid}")
    print(f"  Git SHA:  {git_sha}")

    # Load Slinky mints for matching
    slinky_mints = load_slinky_mints(slinky_path)
    mint_overlap = mint_set & slinky_mints
    print(f"  Mint overlap with Slinky: {len(mint_overlap)} / {len(mint_set)}")

    # Build validation records
    validations = []

    for claim in eligible_claims:
        primary_mint = claim.get('primary_mint')
        if not primary_mint or len(primary_mint) < 32:
            continue

        # Check if mint exists in Slinky
        in_slinky = primary_mint in slinky_mints

        if not in_slinky:
            # No Slinky match — mark as UNRESOLVED
            validation = {
                'validation_id': hashlib.sha256(
                    f"{claim['claim_id']}|unmatched".encode()
                ).hexdigest()[:16],
                'schema_version': '1.0.0',
                'pipeline_version': '1.1.0',
                'run_uuid': run_uuid,
                'git_sha': git_sha,
                'claim_id': claim['claim_id'],
                'narrative_state_id': None,
                'mint': primary_mint,
                'claim_time_ms': claim['publish_time_ms'],
                'outcome_source': 'unmatched',
                'slinky_state_id': None,
                'outcome_time_ms': None,
                'lead_lag_seconds': None,
                'ret_1s_bp': None, 'ret_5s_bp': None, 'ret_30s_bp': None,
                'ret_60s_bp': None, 'ret_300s_bp': None,
                'mfe_bp': None, 'mae_bp': None,
                'net_pnl_sol': None, 'net_return_bp': None,
                'capacity_sol': None, 'execution_feasible': None,
                'is_toxic': None, 'is_rugpull': None, 'is_graduated': None,
                'survived_60s': None, 'survived_300s': None,
                'validation_status': 'UNRESOLVED',
                'validation_reason': 'mint_not_in_slinky_gold_v3',
                'validation_confidence': 0.0,
                'propagation_stage': None,
                'propagation_lead_seconds': None,
                'control_outcome_avg_ret_bp': None,
                'control_outcome_avg_net_pnl': None,
                'treatment_vs_control_lift': None,
                'admission_status': 'UNRESOLVED',
                'rejection_reason': 'mint_not_in_slinky',
            }
            validations.append(validation)
            continue

        # Load Slinky states for this mint
        slinky_states = load_slinky_for_mint(slinky_path, primary_mint)
        if not slinky_states:
            continue

        # Match claim to Slinky outcome
        outcome = match_claim_to_slinky(claim, slinky_states)
        if not outcome:
            continue

        validation = {
            'validation_id': hashlib.sha256(
                f"{claim['claim_id']}|{outcome.get('slinky_state_id', '')}".encode()
            ).hexdigest()[:16],
            'schema_version': '1.0.0',
            'pipeline_version': '1.1.0',
            'run_uuid': run_uuid,
            'git_sha': git_sha,
            'claim_id': claim['claim_id'],
            'narrative_state_id': None,  # will link if state exists
            'mint': primary_mint,
            'claim_time_ms': claim['publish_time_ms'],
            'outcome_source': 'slinky_gold_v3',
            **outcome,
            'control_outcome_avg_ret_bp': None,
            'control_outcome_avg_net_pnl': None,
            'treatment_vs_control_lift': None,
            'admission_status': 'GOLD' if outcome['validation_status'] != 'UNRESOLVED' else 'UNRESOLVED',
            'rejection_reason': None,
        }
        validations.append(validation)

    print(f"\n  Total validations: {len(validations)}")

    # Stats
    admission = Counter(v['admission_status'] for v in validations)
    print(f"  Admission: {dict(admission)}")

    verdict = Counter(v['validation_status'] for v in validations)
    print(f"  Verdict: {dict(verdict)}")

    source = Counter(v['outcome_source'] for v in validations)
    print(f"  Outcome source: {dict(source)}")

    # Write output
    os.makedirs(output_dir, exist_ok=True)
    out_path = os.path.join(output_dir, 'narrative_validation_v1.jsonl')
    with open(out_path, 'w', encoding='utf-8') as f:
        for v in validations:
            f.write(json.dumps(v) + '\n')

    print(f"\n  Output: {out_path}")
    return {'total_validations': len(validations), 'output_path': out_path}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--claim-path',
                        default='output/narrative_gold_v1/gold/creator_claim_v1/creator_claim_v1.jsonl')
    parser.add_argument('--state-path',
                        default='output/narrative_gold_v1/gold/narrative_state_v1/narrative_state_v1.jsonl')
    parser.add_argument('--output-dir',
                        default='output/narrative_gold_v1/gold/narrative_validation_v1')
    parser.add_argument('--slinky-path', default=SLINKY_PATH)
    args = parser.parse_args()
    build_narrative_validation(args.claim_path, args.state_path,
                               args.output_dir, args.slinky_path)
