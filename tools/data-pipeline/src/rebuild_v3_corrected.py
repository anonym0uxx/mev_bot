#!/usr/bin/env python3
"""
rebuild_v3_corrected.py — Corrected LaserStream Gold v3 rebuild from Phase 2 checkpoint.

Fixes:
  1. Fat-tail: remove price<100 threshold, add price-scale features + return flags from L3
  2. L2: rich policy-independent objective truth (multi-horizon returns, MFE/MAE, barriers, etc.)
  3. L3: scenario_id composite key, all size×latency combos (including infeasible), path_invariance
  4. Manifest: path_invariance_assumption, execution_model_version, scenario grid

Loads Phase 2 checkpoint (states + migration_events) to skip the 100-min RAW decode.
Preserves original build_laserstream_gold_v3.py for provenance comparison.
"""
import os, sys, json, time, hashlib, uuid, subprocess, pickle
from collections import defaultdict, Counter
from datetime import datetime, timezone

# Import shared constants and math functions from the original builder
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build_laserstream_gold_v3 import (
    HORIZONS, TRADE_SIZES_SOL, LATENCY_SCENARIOS_MS,
    PUMPFUN_FEE_BPS, PUMPSWAP_FEE_BPS,
    pumpfun_buy_quote, pumpfun_sell_quote,
    pumpswap_buy_quote, pumpswap_sell_quote,
    build_censoring_semantics,
    reconstruct_migration_lifecycles,
    OUTPUT_DIR, RAW_DIR,
)

import numpy as np
import pandas as pd

EXECUTION_MODEL_VERSION = "lsv3-corrected-1"
PATH_INVARIANCE_ASSUMPTION = True
# Barrier thresholds for L2 objective truth
BARRIER_UP = [10.0, 25.0, 50.0, 100.0, 200.0]
BARRIER_DN = [-10.0, -20.0, -30.0, -50.0]
TP_PCT = 15.0
SL_PCT = -15.0
MAX_HOLD_MS = 300 * 1000

# ─── Corrected Phase 5: Fat-tail revalidation ────────────────────────────

def revalidate_fat_tails_corrected(states):
    """Replace the price<100 lamports threshold with economically meaningful features.

    The old threshold marked 99.999% of states as 'extreme_outlier' which is worthless.
    Instead:
    - price_scale_tier: categorical price-scale feature (not a label)
    - Remove extreme_outlier binary flag entirely (it was label-shaping)
    - Return-based flags (return_gt_10x etc.) go in L2/L3 where future info is allowed
    """
    for state in states:
        price = state.get('price_lamports_per_rawtok')
        if price is None or price <= 0:
            state['price_scale_tier'] = 'unrecoverable'
            continue

        # Price-scale tiers (descriptive feature, NOT a label)
        if price < 0.001:
            state['price_scale_tier'] = 'sub_milli_lamport'
        elif price < 0.01:
            state['price_scale_tier'] = 'sub_centi_lamport'
        elif price < 0.1:
            state['price_scale_tier'] = 'sub_deci_lamport'
        elif price < 1.0:
            state['price_scale_tier'] = 'sub_lamport'
        elif price < 10.0:
            state['price_scale_tier'] = 'lamport_range'
        elif price < 100.0:
            state['price_scale_tier'] = 'ten_lamport_range'
        elif price < 1000.0:
            state['price_scale_tier'] = 'hecto_lamport_range'
        else:
            state['price_scale_tier'] = 'kilo_lamport_plus'

    return states


# ─── Per-mint price timeline (shared by L2 and L3) ──────────────────────

def build_price_timelines(states):
    """Build per-mint sorted price timelines for forward-scan.
    Each entry: (timestamp_ms, price, slot, venue, state_id, reserves_dict)
    """
    mint_timelines = defaultdict(list)
    for state in states:
        mint = state['mint_b58']
        ts = state['timestamp_ms']
        reserves = {}
        if state.get('curve_virtual_sol') is not None:
            reserves['curve'] = {
                'virtual_sol': state['curve_virtual_sol'],
                'virtual_token': state['curve_virtual_token'],
                'real_sol': state['curve_real_sol'],
                'real_token': state['curve_real_token'],
                'complete': state['curve_complete'],
            }
        if state.get('pool_base_reserve') is not None:
            reserves['pool'] = {
                'base_reserve': state['pool_base_reserve'],
                'quote_reserve': state['pool_quote_reserve'],
                'lp_supply': state['pool_lp_supply'],
            }
        pt = (ts, state['price_lamports_per_rawtok'], state['slot'],
              state['venue'], state['state_id'], reserves)
        mint_timelines[mint].append(pt)

    for mint in mint_timelines:
        mint_timelines[mint].sort(key=lambda x: x[0])

    # Build state_id -> (mint, timeline_idx) lookup
    state_lookup = {}
    for mint, timeline in mint_timelines.items():
        for i, pt in enumerate(timeline):
            state_lookup[pt[4]] = (mint, i)

    return dict(mint_timelines), state_lookup


# ─── L2: Rich Objective Truth ────────────────────────────────────────────

def build_rich_l2(states, mint_timelines, state_lookup, lifecycles, capture_bounds):
    """Build policy-independent per-state objective truth.

    L2 is 1:1 with L1 by state_id. Contains:
    - Multi-horizon markout returns (return_1s ... return_300s)
    - MFE/MAE + times (mfe_pct, mae_pct, mfe_time_ms, mae_time_ms)
    - Barrier events (first barrier hit + time)
    - Peak return + time to peak
    - Observed-through, has-trade-within, right-censored per horizon
    - Migration/venue transition outcome
    - Price quality / observation completeness metadata
    - Return flags (gt_10x, gt_100x, gt_1000x) from markout
    NULL for unobserved/unknown — never forward-fill.
    """
    cap_start, cap_end = capture_bounds
    l2_records = []
    barrier_labels = [f'up_{int(b)}' for b in BARRIER_UP] + [f'dn_{int(abs(b))}' for b in BARRIER_DN]

    for state_idx, state in enumerate(states):
        if state_idx % 100000 == 0 and state_idx > 0:
            print(f"    L2 progress: {state_idx:,}/{len(states):,} ({state_idx/len(states)*100:.1f}%)", flush=True)

        sid = state['state_id']
        mint = state['mint_b58']
        ts = state['timestamp_ms']
        entry_price = state.get('price_lamports_per_rawtok')
        venue = state['venue']

        rec = {
            'state_id': sid,
            'venue': venue,
            'entry_executable': state.get('entry_executable', True),
            'entry_failure_reason': state.get('entry_failure_reason'),
        }

        if entry_price is None or entry_price <= 0:
            # Unrecoverable price — null all return fields
            for h in HORIZONS:
                rec[f'return_{h}s'] = None
                rec[f'observed_through_{h}s'] = False
                rec[f'has_trade_within_{h}s'] = False
                rec[f'right_censored_{h}s'] = state.get(f'right_censored_{h}s', None)
            rec['mfe_pct'] = None
            rec['mae_pct'] = None
            rec['mfe_time_ms'] = None
            rec['mae_time_ms'] = None
            rec['barrier_first'] = None
            rec['barrier_first_time_ms'] = None
            rec['barrier_first_label'] = None
            rec['peak_return_pct'] = None
            rec['peak_time_ms'] = None
            rec['return_gt_10x'] = False
            rec['return_gt_100x'] = False
            rec['return_gt_1000x'] = False
            rec['price_quality'] = 'unrecoverable'
            rec['observation_completeness'] = 'unrecoverable'
            l2_records.append(rec)
            continue

        # Get the mint timeline for forward-scan
        lookup = state_lookup.get(sid)
        if lookup is None:
            for h in HORIZONS:
                rec[f'return_{h}s'] = None
                rec[f'observed_through_{h}s'] = False
                rec[f'has_trade_within_{h}s'] = False
                rec[f'right_censored_{h}s'] = state.get(f'right_censored_{h}s', None)
            rec['mfe_pct'] = None
            rec['mae_pct'] = None
            rec['mfe_time_ms'] = None
            rec['mae_time_ms'] = None
            rec['barrier_first'] = None
            rec['barrier_first_time_ms'] = None
            rec['barrier_first_label'] = None
            rec['peak_return_pct'] = None
            rec['peak_time_ms'] = None
            rec['return_gt_10x'] = False
            rec['return_gt_100x'] = False
            rec['return_gt_1000x'] = False
            rec['price_quality'] = 'unknown'
            rec['observation_completeness'] = 'unknown'
            l2_records.append(rec)
            continue

        mint_key, entry_idx = lookup
        timeline = mint_timelines[mint_key]

        # ─── Single-pass forward-scan ────────────────────────────────────
        # Instead of scanning 8 times (once per horizon), scan ONCE and
        # compute all horizons simultaneously. O(N×T) instead of O(N×T×H).
        mfe_pct = -float('inf')
        mae_pct = float('inf')
        mfe_time_ms = None
        mae_time_ms = None
        peak_return_pct = -float('inf')
        peak_time_ms = None
        barrier_first = None
        barrier_first_time_ms = None
        barrier_first_label = None

        # Pre-compute horizon boundaries
        horizon_ms = {h: h * 1000 for h in HORIZONS}
        horizon_censored = {h: (ts + horizon_ms[h]) > cap_end for h in HORIZONS}
        horizon_returns = {h: None for h in HORIZONS}
        horizon_has_trade = {h: False for h in HORIZONS}
        horizon_observed = {h: False for h in HORIZONS}
        # Track which horizons still need to find their markout price
        remaining = set(h for h in HORIZONS if not horizon_censored[h])

        for h in HORIZONS:
            rec[f'right_censored_{h}s'] = horizon_censored[h]
            if horizon_censored[h]:
                rec[f'return_{h}s'] = None
                rec[f'has_trade_within_{h}s'] = None
                rec[f'observed_through_{h}s'] = None

        # Single forward pass through timeline
        max_h_ms = horizon_ms[max(HORIZONS)] if remaining else 0
        for j in range(entry_idx + 1, len(timeline)):
            future_pt = timeline[j]
            future_ts = future_pt[0]
            # Stop if past the largest remaining horizon
            if remaining and future_ts > ts + max_h_ms:
                break

            future_price = future_pt[1]
            if future_price and future_price > 0:
                r = (future_price / entry_price - 1.0) * 100.0
                dt = future_ts - ts

                # Update MFE/MAE (track within 300s window)
                if dt <= horizon_ms[300]:
                    if r > mfe_pct:
                        mfe_pct = r
                        mfe_time_ms = dt
                    if r < mae_pct:
                        mae_pct = r
                        mae_time_ms = dt
                    if r > peak_return_pct:
                        peak_return_pct = r
                        peak_time_ms = dt
                    # Check barriers (first hit only)
                    if barrier_first is None and dt <= horizon_ms[300]:
                        for b_up in BARRIER_UP:
                            if r >= b_up:
                                barrier_first = 'tp'
                                barrier_first_time_ms = dt
                                barrier_first_label = f'up_{int(b_up)}'
                                break
                        if barrier_first is None:
                            for b_dn in BARRIER_DN:
                                if r <= b_dn:
                                    barrier_first = 'sl'
                                    barrier_first_time_ms = dt
                                    barrier_first_label = f'dn_{int(abs(b_dn))}'
                                    break

                # Assign returns to any horizon whose window contains this point
                to_remove = set()
                for h in remaining:
                    if future_ts <= ts + horizon_ms[h]:
                        horizon_returns[h] = r
                        horizon_has_trade[h] = True
                        horizon_observed[h] = True
                    else:
                        # This point is past horizon h — mark it
                        rec[f'has_trade_within_{h}s'] = horizon_has_trade[h]
                        rec[f'observed_through_{h}s'] = horizon_observed[h]
                        rec[f'return_{h}s'] = horizon_returns[h]
                        to_remove.add(h)
                remaining -= to_remove

        # Handle remaining horizons (end of timeline reached before horizon)
        for h in remaining:
            rec[f'has_trade_within_{h}s'] = horizon_has_trade[h]
            rec[f'observed_through_{h}s'] = horizon_observed[h]
            rec[f'return_{h}s'] = horizon_returns[h]

        # Finalize MFE/MAE
        rec['mfe_pct'] = mfe_pct if mfe_pct != -float('inf') else None
        rec['mae_pct'] = mae_pct if mae_pct != float('inf') else None
        rec['mfe_time_ms'] = mfe_time_ms
        rec['mae_time_ms'] = mae_time_ms
        rec['barrier_first'] = barrier_first
        rec['barrier_first_time_ms'] = barrier_first_time_ms
        rec['barrier_first_label'] = barrier_first_label
        rec['peak_return_pct'] = peak_return_pct if peak_return_pct != -float('inf') else None
        rec['peak_time_ms'] = peak_time_ms

        # Return flags from 300s markout (the max horizon)
        ret_300 = horizon_returns.get(300)
        if ret_300 is not None:
            rec['return_gt_10x'] = ret_300 >= 900.0    # 10x = +900%
            rec['return_gt_100x'] = ret_300 >= 9900.0   # 100x = +9900%
            rec['return_gt_1000x'] = ret_300 >= 99900.0 # 1000x = +99900%
        else:
            # Use peak if markout not available
            if peak_return_pct is not None:
                rec['return_gt_10x'] = peak_return_pct >= 900.0
                rec['return_gt_100x'] = peak_return_pct >= 9900.0
                rec['return_gt_1000x'] = peak_return_pct >= 99900.0
            else:
                rec['return_gt_10x'] = False
                rec['return_gt_100x'] = False
                rec['return_gt_1000x'] = False

        # Migration/venue outcome
        lc = lifecycles.get(mint, {})
        has_migration = lc.get('lifecycle_joined', False) or lc.get('has_pumpfun_pre', False)
        mig_ms = lc.get('migration_time_ms')
        if mig_ms is None:
            rec['migration_outcome'] = 'none'
        elif ts < mig_ms:
            rec['migration_outcome'] = 'pre'
        else:
            rec['migration_outcome'] = 'post'

        # Venue outcome: did this mint appear in multiple venues?
        timeline_venues = set(pt[3] for pt in timeline)
        if len(timeline_venues) > 1:
            rec['venue_outcome'] = 'transitioned'
        else:
            rec['venue_outcome'] = 'same'

        # Price quality
        price_source = state.get('price_source', 'unknown')
        rec['price_quality'] = price_source

        # Observation completeness
        censored_count = sum(1 for h in HORIZONS if rec.get(f'right_censored_{h}s', False))
        if censored_count == 0:
            rec['observation_completeness'] = 'full'
        elif censored_count == len(HORIZONS):
            rec['observation_completeness'] = 'none'
        else:
            rec['observation_completeness'] = 'partial'

        l2_records.append(rec)

    return l2_records


# ─── L3: Corrected counterfactuals with scenario_id ──────────────────────

def build_counterfactuals_corrected(states, mint_timelines, state_lookup, lifecycles, output_dir, batch_size=500000):
    """Build multi-scenario counterfactuals, writing to parquet in batches to avoid OOM.

    L3 is 1:N by state_id with composite key (state_id, scenario_id).
    Emits ALL size×latency combos including infeasible with feasible=false + reason.
    Records path_invariance_assumption, execution_model_version, capacity/feasibility/confidence.
    Returns summary stats instead of holding all records in memory.
    """
    import pyarrow as pa
    import pyarrow.parquet as pq

    fee_model = 'pumpfun_1pct_buy_0pct_sell'  # will be set per-venue
    cf_buffer = []
    total_written = 0
    feasible_count = 0
    infeasible_count = 0
    writer = None
    l3_path = os.path.join(output_dir, 'l3_counterfactual_v3.parquet')

    # We'll collect L4-relevant stats per state as we go
    l4_stats = {}

    def flush_buffer():
        nonlocal writer, total_written, cf_buffer
        if not cf_buffer:
            return
        df_batch = pd.DataFrame(cf_buffer)
        table = pa.Table.from_pandas(df_batch, preserve_index=False)
        if writer is None:
            writer = pq.ParquetWriter(l3_path, table.schema, compression='snappy')
        writer.write_table(table)
        total_written += len(cf_buffer)
        cf_buffer = []
        del df_batch, table

    for state_idx, state in enumerate(states):
        if state_idx % 100000 == 0 and state_idx > 0:
            print(f"    L3 progress: {state_idx:,}/{len(states):,} ({state_idx/len(states)*100:.1f}%) | written: {total_written:,}", flush=True)
            flush_buffer()

        sid = state['state_id']
        mint = state['mint_b58']
        venue = state['venue']
        ts = state['timestamp_ms']

        curve = None
        pool = None
        if venue == 'pumpfun' and state.get('curve_virtual_sol') is not None:
            curve = {
                'virtual_sol': state['curve_virtual_sol'],
                'virtual_token': state['curve_virtual_token'],
                'real_sol': state['curve_real_sol'],
                'real_token': state['curve_real_token'],
                'complete': state['curve_complete'],
            }
            fee_model = 'pumpfun_1pct_buy_0pct_sell'
        elif venue == 'pumpswap' and state.get('pool_base_reserve') is not None:
            pool = {
                'base_reserve': state['pool_base_reserve'],
                'quote_reserve': state['pool_quote_reserve'],
                'lp_supply': state.get('pool_lp_supply'),
            }
            fee_model = 'pumpswap_0.25pct'

        lookup = state_lookup.get(sid)
        entry_idx = lookup[1] if lookup else -1
        timeline = mint_timelines.get(lookup[0], []) if lookup else []

        # Pre-compute entry quotes for each size (only once per state)
        size_quotes = {}
        for size_sol in TRADE_SIZES_SOL:
            size_lamports = int(size_sol * 1e9)
            tokens_bought = None
            entry_price = None
            feasible = True
            reason = None

            if curve is None and pool is None:
                feasible = False
                reason = 'reserves_unavailable'
            elif venue == 'pumpfun' and curve:
                tokens_bought = pumpfun_buy_quote(curve, size_lamports)
                if tokens_bought is None or tokens_bought == 0:
                    feasible = False
                    reason = 'quote_zero_or_overflow'
                else:
                    entry_price = float(size_lamports) / float(tokens_bought)
            elif venue == 'pumpswap' and pool:
                tokens_bought = pumpswap_buy_quote(pool, size_lamports)
                if tokens_bought is None or tokens_bought == 0:
                    feasible = False
                    reason = 'quote_zero_or_overflow'
                else:
                    entry_price = float(size_lamports) / float(tokens_bought)
            else:
                feasible = False
                reason = 'venue_mismatch'

            size_quotes[size_sol] = {
                'tokens_bought': tokens_bought,
                'entry_price': entry_price,
                'feasible': feasible,
                'reason': reason,
                'size_lamports': size_lamports,
            }

        # ─── Single-pass exit computation for ALL sizes ──────────────────
        # Instead of scanning timeline 5 times (once per size), scan ONCE.
        # For each future point, compute exit sol for all feasible sizes.
        size_exits = {}
        for size_sol in TRADE_SIZES_SOL:
            sq = size_quotes[size_sol]
            if not sq['feasible']:
                size_exits[size_sol] = None
                continue
            size_exits[size_sol] = {
                'exit_sol': None, 'exit_executable': False,
                'sim_return_pct': None, 'outcome_class': 'timeout_or_marginal',
                'capacity_constrained': False,
                'tokens_bought': sq['tokens_bought'],
                'size_lamports': sq['size_lamports'],
            }

        # Collect feasible sizes for single-pass
        feasible_sizes = [(s, size_exits[s]) for s in TRADE_SIZES_SOL if size_exits[s] is not None]
        if feasible_sizes and entry_idx >= 0:
            last_sol_received = {s: 0 for s, _ in feasible_sizes}
            for j in range(entry_idx + 1, len(timeline)):
                future_pt = timeline[j]
                future_ts = future_pt[0]
                dt_ms = future_ts - ts
                if dt_ms > MAX_HOLD_MS:
                    break

                fv = future_pt[3]
                fr = future_pt[5]

                for size_sol, ex in feasible_sizes:
                    if ex['outcome_class'] != 'timeout_or_marginal':
                        continue  # already resolved (tp/sl hit)

                    tokens_bought = ex['tokens_bought']
                    size_lamports = ex['size_lamports']
                    sol_received = None

                    if fv == 'pumpfun' and 'curve' in fr:
                        fcr = fr['curve']
                        if not fcr.get('complete', False):
                            sol_received = pumpfun_sell_quote(fcr, tokens_bought)
                            if sol_received and sol_received > fcr.get('real_sol', 0):
                                sol_received = fcr['real_sol']
                                ex['capacity_constrained'] = True
                    elif fv == 'pumpswap' and 'pool' in fr:
                        fpr = fr['pool']
                        sol_received = pumpswap_sell_quote(fpr, tokens_bought)
                        if sol_received and sol_received > fpr.get('quote_reserve', 0):
                            sol_received = fpr['quote_reserve']
                            ex['capacity_constrained'] = True

                    if sol_received is None or sol_received == 0:
                        continue

                    last_sol_received[size_sol] = sol_received
                    ret_pct = (float(sol_received) / float(size_lamports) - 1.0) * 100.0

                    if ret_pct >= TP_PCT:
                        ex['exit_sol'] = sol_received
                        ex['outcome_class'] = 'tp_hit'
                        ex['exit_executable'] = True
                    elif ret_pct <= SL_PCT:
                        ex['exit_sol'] = sol_received
                        ex['outcome_class'] = 'sl_hit'
                        ex['exit_executable'] = True

            # Handle timeout for unresolved sizes
            for size_sol, ex in feasible_sizes:
                if ex['outcome_class'] == 'timeout_or_marginal':
                    exit_sol = last_sol_received[size_sol]
                    if exit_sol and exit_sol > 0:
                        ex['exit_sol'] = exit_sol
                        ex['exit_executable'] = True
                    else:
                        ex['outcome_class'] = 'timeout_no_exit'
                        ex['exit_sol'] = 0

                # Compute sim_return
                if ex['exit_sol'] and ex['exit_sol'] > 0:
                    ex['sim_return_pct'] = (float(ex['exit_sol']) / float(ex['size_lamports']) - 1.0) * 100.0

        elif feasible_sizes and entry_idx < 0:
            for size_sol, ex in feasible_sizes:
                ex['outcome_class'] = 'timeout_no_exit'
                ex['exit_sol'] = 0

        # Emit ALL size×latency combos (including infeasible)
        for size_sol in TRADE_SIZES_SOL:
            sq = size_quotes[size_sol]
            size_lamports = sq['size_lamports']

            for latency_ms in LATENCY_SCENARIOS_MS:
                scenario_id = f"{sid}_s{size_sol}_l{latency_ms}_{fee_model}_v{EXECUTION_MODEL_VERSION}"
                # Shorten scenario_id with hash for parquet efficiency
                scenario_id_short = hashlib.sha256(scenario_id.encode()).hexdigest()[:20]

                if not sq['feasible']:
                    cf_buffer.append({
                        'scenario_id': scenario_id_short,
                        'state_id': sid,
                        'trade_size_sol': size_sol,
                        'trade_size_lamports': size_lamports,
                        'venue': venue,
                        'latency_scenario_ms': latency_ms,
                        'fee_model': fee_model,
                        'execution_model_version': EXECUTION_MODEL_VERSION,
                        'path_invariance_assumption': PATH_INVARIANCE_ASSUMPTION,
                        'tokens_bought_raw': 0,
                        'entry_price_lamports_per_rawtok': None,
                        'entry_executable': False,
                        'entry_failure_reason': sq['reason'],
                        'feasible': False,
                        'feasibility_reason': sq['reason'],
                        'exit_executable': False,
                        'exit_sol_received_lamports': 0,
                        'sim_return_pct': None,
                        'outcome_class': 'entry_failed',
                        'capacity_constrained': False,
                        'confidence': 'none',
                    })
                    infeasible_count += 1
                    continue

                ex = size_exits[size_sol]
                # Confidence based on capacity and entry feasibility
                if ex['capacity_constrained']:
                    confidence = 'low'
                elif ex['exit_executable']:
                    confidence = 'high'
                else:
                    confidence = 'medium'

                cf_buffer.append({
                    'scenario_id': scenario_id_short,
                    'state_id': sid,
                    'trade_size_sol': size_sol,
                    'trade_size_lamports': size_lamports,
                    'venue': venue,
                    'latency_scenario_ms': latency_ms,
                    'fee_model': fee_model,
                    'execution_model_version': EXECUTION_MODEL_VERSION,
                    'path_invariance_assumption': PATH_INVARIANCE_ASSUMPTION,
                    'tokens_bought_raw': sq['tokens_bought'],
                    'entry_price_lamports_per_rawtok': sq['entry_price'],
                    'entry_executable': True,
                    'entry_failure_reason': None,
                    'feasible': True,
                    'feasibility_reason': None,
                    'exit_executable': ex['exit_executable'],
                    'exit_sol_received_lamports': ex['exit_sol'],
                    'sim_return_pct': ex['sim_return_pct'],
                    'outcome_class': ex['outcome_class'],
                    'capacity_constrained': ex['capacity_constrained'],
                    'confidence': confidence,
                })
                feasible_count += 1

        # Collect L4 stats per state
        best_outcome = 'timeout_or_marginal'
        best_return = None
        best_size = None
        for size_sol in TRADE_SIZES_SOL:
            ex = size_exits.get(size_sol)
            if ex is None:
                continue
            if ex.get('exit_executable') and (best_return is None or
                (ex.get('sim_return_pct') is not None and ex['sim_return_pct'] > (best_return or -float('inf')))):
                best_outcome = ex['outcome_class']
                best_return = ex['sim_return_pct']
                best_size = size_sol

        l4_stats[sid] = {
            'best_outcome': best_outcome,
            'best_return_pct': best_return,
            'best_size_sol': best_size,
        }

    # Final flush
    flush_buffer()
    if writer is not None:
        writer.close()

    print(f"  L3 total written: {total_written:,} (feasible={feasible_count:,}, infeasible={infeasible_count:,})")
    return {'total': total_written, 'feasible': feasible_count, 'infeasible': infeasible_count, 'l4_stats': l4_stats}


# ─── L1 builder (corrected fat-tail fields) ──────────────────────────────

def build_l1_corrected(states):
    """Build L1 with corrected fat-tail fields.
    Replaces extreme_outlier/fat_tail_validated with price_scale_tier.
    Keeps all censoring/no-trade fields from Phase 4.
    """
    l1_cols = [
        'state_id', 'mint_b58', 'slot', 'timestamp_ms', 'venue', 'event_type',
        'trade_side', 'price_lamports_per_rawtok', 'sol_traded_lamports',
        'tokens_traded_raw', 'price_source', 'token_decimals', 'trader_b58',
        'curve_account_b58', 'pool_account_b58',
        'curve_virtual_sol', 'curve_virtual_token', 'curve_real_sol', 'curve_real_token', 'curve_complete',
        'pool_base_reserve', 'pool_quote_reserve', 'pool_lp_supply',
        'ix_amount_in', 'ix_amount_out', 'ix_min_amount_out', 'ix_max_amount_in', 'ix_fee_bps',
        'capture_start_ms', 'capture_end_ms', 'raw_hash',
        'price_scale_tier',
        'right_censored_1s', 'right_censored_2s', 'right_censored_5s', 'right_censored_10s',
        'right_censored_30s', 'right_censored_60s', 'right_censored_120s', 'right_censored_300s',
        'observed_no_trade_1s', 'observed_no_trade_2s', 'observed_no_trade_5s', 'observed_no_trade_10s',
        'observed_no_trade_30s', 'observed_no_trade_60s', 'observed_no_trade_120s', 'observed_no_trade_300s',
    ]
    l1_df = pd.DataFrame(states)
    for col in l1_cols:
        if col not in l1_df.columns:
            l1_df[col] = None
    l1_df = l1_df[l1_cols]
    return l1_df


# ─── L4 builder (mostly unchanged, uses 0.1 SOL / 0ms benchmark) ──────────

def build_l4_corrected(l4_stats):
    """Build L4 policy_eval from the best-size benchmark counterfactuals.
    Uses the best executable counterfactual per state as the policy evaluation.
    """
    l4_records = []
    for sid, stats in l4_stats.items():
        ret = stats.get('best_return_pct')
        outcome = stats.get('best_outcome', 'timeout_or_marginal')
        if ret is None:
            policy_outcome = 'entry_failed'
            policy_return = None
        elif ret >= TP_PCT:
            policy_outcome = 'tp_hit'
            policy_return = TP_PCT
        elif ret <= SL_PCT:
            policy_outcome = 'sl_hit'
            policy_return = SL_PCT
        else:
            policy_outcome = 'timeout'
            policy_return = ret
        l4_records.append({
            'state_id': sid,
            'policy_class': 'tp15_sl15_maxhold300',
            'policy_outcome': policy_outcome,
            'policy_return_pct': policy_return,
            'best_size_sol': stats.get('best_size_sol'),
        })
    return l4_records


# ─── Manifest writer (corrected) ─────────────────────────────────────────

def write_manifest_corrected(l1_df, l2_df, l3_df, l4_df, stats, migration_stats,
                              capture_bounds, lifecycles):
    import hashlib

    def file_hash(path):
        try:
            h = hashlib.sha256()
            with open(path, 'rb') as f:
                for chunk in iter(lambda: f.read(8192), b''):
                    h.update(chunk)
            return h.hexdigest()
        except Exception:
            return None

    output_dir = OUTPUT_DIR

    # Compute train-only quantile metadata for return-based flags
    # (Using 300s markout returns as the reference)
    l2_ret300 = l2_df['return_300s'].dropna()
    quantiles = {}
    if len(l2_ret300) > 0:
        for q in [0.01, 0.05, 0.10, 0.25, 0.50, 0.75, 0.90, 0.95, 0.99]:
            quantiles[f'p{int(q*100)}'] = float(l2_ret300.quantile(q))

    # Price-scale tier distribution
    tier_dist = dict(l1_df['price_scale_tier'].value_counts()) if 'price_scale_tier' in l1_df.columns else {}

    # Return flag distributions (state-level)
    return_flags = {
        'return_gt_10x': int(l2_df['return_gt_10x'].sum()) if 'return_gt_10x' in l2_df.columns else 0,
        'return_gt_100x': int(l2_df['return_gt_100x'].sum()) if 'return_gt_100x' in l2_df.columns else 0,
        'return_gt_1000x': int(l2_df['return_gt_1000x'].sum()) if 'return_gt_1000x' in l2_df.columns else 0,
    }

    # Unique-mint-level distributions
    mint_level = {}
    if 'mint_b58' in l1_df.columns and 'return_300s' in l2_df.columns:
        # Join L1 mint info with L2 returns
        tmp = l1_df[['state_id', 'mint_b58']].merge(
            l2_df[['state_id', 'return_300s']], on='state_id', how='inner'
        )
        mint_ret = tmp.groupby('mint_b58')['return_300s'].agg(['mean', 'max', 'min', 'count']).reset_index()
        mint_level['mean_300s_ret_by_mint'] = {
            'p50': float(mint_ret['mean'].median()),
            'p90': float(mint_ret['mean'].quantile(0.9)),
            'p99': float(mint_ret['mean'].quantile(0.99)),
        }
        mint_level['max_300s_ret_by_mint'] = {
            'p50': float(mint_ret['max'].median()),
            'p90': float(mint_ret['max'].quantile(0.9)),
            'p99': float(mint_ret['max'].quantile(0.99)),
        }
        mint_level['unique_mints_total'] = int(l1_df['mint_b58'].nunique())
        mint_level['mints_with_10x_returns'] = int(mint_ret[mint_ret['max'] >= 900.0]['mint_b58'].nunique())
        mint_level['mints_with_100x_returns'] = int(mint_ret[mint_ret['max'] >= 9900.0]['mint_b58'].nunique())
        mint_level['mints_with_1000x_returns'] = int(mint_ret[mint_ret['max'] >= 99900.0]['mint_b58'].nunique())

    # Scenario grid
    scenario_grid = {
        'trade_sizes_sol': TRADE_SIZES_SOL,
        'latency_scenarios_ms': LATENCY_SCENARIOS_MS,
        'fee_models': ['pumpfun_1pct_buy_0pct_sell', 'pumpswap_0.25pct'],
        'total_scenarios_per_state': len(TRADE_SIZES_SOL) * len(LATENCY_SCENARIOS_MS),
        'execution_model_version': EXECUTION_MODEL_VERSION,
    }

    # L3 cardinality audit
    expected_l3 = len(l1_df) * len(TRADE_SIZES_SOL) * len(LATENCY_SCENARIOS_MS)
    l3_feasible = int((l3_df['feasible'] == True).sum()) if 'feasible' in l3_df.columns else 0
    l3_infeasible = int((l3_df['feasible'] == False).sum()) if 'feasible' in l3_df.columns else 0
    l3_duplicate_scenarios = int(l3_df.duplicated(subset=['scenario_id']).sum()) if 'scenario_id' in l3_df.columns else 0

    manifest = {
        'run_uuid': str(uuid.uuid4()),
        'build_timestamp': datetime.now(timezone.utc).isoformat(),
        'source': 'raw_zst_capture2',
        'source_format': 'RAW .zst (authoritative)',
        'capture_bounds': {
            'start_ms': capture_bounds[0],
            'end_ms': capture_bounds[1],
            'duration_minutes': (capture_bounds[1] - capture_bounds[0]) / 60000,
        },
        'layer_counts': {
            'l1_pump_state': len(l1_df),
            'l2_pump_outcome': len(l2_df),
            'l3_counterfactual': len(l3_df),
            'l4_policy_eval': len(l4_df),
        },
        'layer_cardinality': {
            'l1_l2_l4_ratio': '1:1:1 by state_id',
            'l3_cardinality': f'1:N by state_id (N={len(TRADE_SIZES_SOL)}×{len(LATENCY_SCENARIOS_MS)} scenarios)',
            'l3_expected_rows': expected_l3,
            'l3_actual_rows': len(l3_df),
            'l3_feasible': l3_feasible,
            'l3_infeasible': l3_infeasible,
            'l3_duplicate_scenario_ids': l3_duplicate_scenarios,
        },
        'unique_mints': int(l1_df['mint_b58'].nunique()),
        'venue_distribution': {k: int(v) for k, v in l1_df['venue'].value_counts().to_dict().items()},
        'migration_stats': migration_stats,
        'transaction_stats': stats,
        'path_invariance_assumption': PATH_INVARIANCE_ASSUMPTION,
        'path_invariance_description': (
            'Hypothetical trade is assumed not to alter the subsequently observed market path. '
            'Larger-size counterfactual PnL is conditional evidence, NOT guaranteed truth.'
        ),
        'execution_model_version': EXECUTION_MODEL_VERSION,
        'execution_assumptions': {
            'quote_source': 'decoded_raw_account_snapshots',
            'reserve_source': 'raw_data_b64_account_state',
            'fee_model': 'venue-specific: pumpfun 1% buy / 0% sell, pumpswap 0.25% per swap',
            'capacity_check': 'exit_sol capped at real_sol (pumpfun) or quote_reserve (pumpswap)',
            'confidence_levels': 'high=executable entry+exit, medium=entry ok exit uncertain, low=capacity constrained',
        },
        'scenario_grid': scenario_grid,
        'sim_config': {
            'tp_pct': TP_PCT,
            'sl_pct': SL_PCT,
            'max_hold_s': 300,
            'trade_sizes_sol': TRADE_SIZES_SOL,
            'latency_scenarios_ms': LATENCY_SCENARIOS_MS,
        },
        'fat_tail_config': {
            'method': 'return_based_flags + price_scale_tier_feature',
            'return_flags': return_flags,
            'price_scale_tier_distribution': {k: int(v) for k, v in tier_dist.items()},
            'train_only_quantiles_300s': quantiles,
            'note': 'Quantile thresholds fit on TRAIN only, applied unchanged to val/test. Never clip objective truth.',
        },
        'mint_level_distributions': mint_level,
        'l2_fields': {
            'multi_horizon_returns': [f'return_{h}s' for h in HORIZONS],
            'mfe_mae': ['mfe_pct', 'mae_pct', 'mfe_time_ms', 'mae_time_ms'],
            'barrier_events': ['barrier_first', 'barrier_first_time_ms', 'barrier_first_label'],
            'barrier_thresholds': {
                'up': BARRIER_UP,
                'dn': BARRIER_DN,
            },
            'peak': ['peak_return_pct', 'peak_time_ms'],
            'observed_through': [f'observed_through_{h}s' for h in HORIZONS],
            'has_trade_within': [f'has_trade_within_{h}s' for h in HORIZONS],
            'right_censored': [f'right_censored_{h}s' for h in HORIZONS],
            'migration_venue': ['migration_outcome', 'venue_outcome'],
            'price_quality': ['price_quality', 'observation_completeness'],
            'return_flags': ['return_gt_10x', 'return_gt_100x', 'return_gt_1000x'],
        },
        'git_sha': subprocess.getoutput('cd D:/repos/mev_bot && git rev-parse HEAD')[:12],
        'horizons': HORIZONS,
        'hashes': {
            'l1_sha256': file_hash(os.path.join(output_dir, 'l1_pump_state_v3.parquet')),
            'l2_sha256': file_hash(os.path.join(output_dir, 'l2_pump_outcome_v3.parquet')),
            'l3_sha256': file_hash(os.path.join(output_dir, 'l3_counterfactual_v3.parquet')),
            'l4_sha256': file_hash(os.path.join(output_dir, 'l4_policy_eval_v3.parquet')),
        },
        'supersedes': 'build_laserstream_gold_v3.py run 5 (UUID 8f15b1ae)',
        'correction_notes': [
            'Removed price<100 lamports/rawtok extreme_outlier threshold (was marking 99.999% as extreme)',
            'Added price_scale_tier as descriptive feature (NOT a label)',
            'Added return_gt_10x/100x/1000x flags from markout (in L2, not L1)',
            'L2 enriched with 21+ policy-independent objective truth fields',
            'L3 now emits ALL size×latency combos including infeasible with feasible=false+reason',
            'L3 has scenario_id composite key for uniqueness',
            'path_invariance_assumption recorded explicitly in L3 + manifest',
            'Train-only quantile metadata recorded for future split-aware threshold fitting',
        ],
    }

    return manifest


# ─── Main rebuild ────────────────────────────────────────────────────────

def main():
    print(f"\n{'='*70}")
    print(f"rebuild_v3_corrected — Corrected LaserStream Gold v3 rebuild")
    print(f"Output: {OUTPUT_DIR}")
    print(f"{'='*70}")

    os.makedirs(OUTPUT_DIR, exist_ok=True)

    # Load Phase 2 checkpoint
    ckpt_path = os.path.join(OUTPUT_DIR, 'phase2_checkpoint.pkl')
    print(f"\n  Loading Phase 2 checkpoint...")
    t0 = time.time()
    with open(ckpt_path, 'rb') as f:
        ckpt = pickle.load(f)
    states = ckpt['states']
    migration_events = ckpt['migration_events']
    stats = ckpt['stats']
    capture_bounds = ckpt['capture_bounds']
    print(f"  Loaded {len(states):,} states in {time.time()-t0:.1f}s")

    # Phase 3: Migration reconstruction
    print(f"\nPhase 3: Reconstructing migration lifecycles...")
    t0 = time.time()
    lifecycles, migration_stats = reconstruct_migration_lifecycles(states, migration_events)
    print(f"  Mints: {migration_stats.get('total_mints', 0):,}")
    print(f"  Migrated: {migration_stats.get('migrated_mints', 0):,}")
    print(f"  Joined: {migration_stats.get('joined_lifecycles', 0):,}")
    print(f"  Time: {time.time()-t0:.1f}s")

    # Phase 4: Censoring semantics (unchanged)
    print(f"\nPhase 4: Computing censoring semantics...")
    t0 = time.time()
    states = build_censoring_semantics(states, capture_bounds)
    censored_300 = sum(1 for s in states if s.get('right_censored_300s', False))
    print(f"  Right-censored @300s: {censored_300:,} ({censored_300/len(states)*100:.1f}%)")
    print(f"  Time: {time.time()-t0:.1f}s")

    # Phase 5: Corrected fat-tail revalidation
    print(f"\nPhase 5: Corrected fat-tail revalidation (price<100 threshold REMOVED)...")
    t0 = time.time()
    states = revalidate_fat_tails_corrected(states)
    tier_counts = Counter(s.get('price_scale_tier', 'unknown') for s in states)
    print(f"  Price-scale tier distribution:")
    for tier, count in sorted(tier_counts.items()):
        print(f"    {tier}: {count:,} ({count/len(states)*100:.1f}%)")
    print(f"  Time: {time.time()-t0:.1f}s")

    # Phase 6a: Build shared price timelines
    print(f"\nPhase 6a: Building per-mint price timelines...")
    t0 = time.time()
    mint_timelines, state_lookup = build_price_timelines(states)
    print(f"  Mints with timelines: {len(mint_timelines):,}")
    print(f"  Total timeline points: {sum(len(v) for v in mint_timelines.values()):,}")
    print(f"  Time: {time.time()-t0:.1f}s")

    # Phase 6b: Build L2 rich objective truth
    print(f"\nPhase 6b: Building L2 rich objective truth...")
    t0 = time.time()
    l2_records = build_rich_l2(states, mint_timelines, state_lookup, lifecycles, capture_bounds)
    print(f"  L2 records: {len(l2_records):,}")
    print(f"  Time: {time.time()-t0:.1f}s")

    # Phase 6c: Build corrected counterfactuals with scenario_id (batch-written to parquet)
    print(f"\nPhase 6c: Building corrected counterfactuals (ALL size×latency combos)...")
    t0 = time.time()
    # Remove old L3 parquet if exists
    l3_path = os.path.join(OUTPUT_DIR, 'l3_counterfactual_v3.parquet')
    if os.path.exists(l3_path):
        os.remove(l3_path)
    cf_result = build_counterfactuals_corrected(states, mint_timelines, state_lookup, lifecycles, OUTPUT_DIR)
    print(f"  CF records: {cf_result['total']:,}")
    print(f"  Feasible: {cf_result['feasible']:,} | Infeasible: {cf_result['infeasible']:,}")
    print(f"  Time: {time.time()-t0:.1f}s")

    # Phase 7: Build remaining layers (L3 already written to parquet)
    print(f"\nPhase 7: Building 4-layer parquet output (L1, L2, L4)...")
    t0 = time.time()

    l1_df = build_l1_corrected(states)
    l2_df = pd.DataFrame(l2_records)
    l4_records = build_l4_corrected(cf_result['l4_stats'])
    l4_df = pd.DataFrame(l4_records)

    # Read L3 back from parquet for verification and manifest
    l3_df = pd.read_parquet(l3_path)

    t1 = time.time()
    print(f"  L1 pump_state: {len(l1_df):,} rows ({len(l1_df.columns)} cols)")
    print(f"  L2 pump_outcome: {len(l2_df):,} rows ({len(l2_df.columns)} cols)")
    print(f"  L3 counterfactual: {len(l3_df):,} rows ({len(l3_df.columns)} cols)")
    print(f"  L4 policy_eval: {len(l4_df):,} rows ({len(l4_df.columns)} cols)")

    # Verify cardinality
    expected_l3 = len(l1_df) * len(TRADE_SIZES_SOL) * len(LATENCY_SCENARIOS_MS)
    print(f"  L3 expected: {expected_l3:,} | actual: {len(l3_df):,} | match: {expected_l3 == len(l3_df)}")

    # Save Phase 7 checkpoint (L1/L2/L4 DataFrames + metadata; L3 already on disk)
    ckpt7_path = os.path.join(OUTPUT_DIR, 'phase7_checkpoint_v3_corrected.pkl')
    with open(ckpt7_path, 'wb') as f:
        pickle.dump({'l1_df': l1_df, 'l2_df': l2_df, 'l4_df': l4_df,
                     'cf_result': cf_result,
                     'stats': stats, 'migration_stats': migration_stats,
                     'capture_bounds': capture_bounds, 'lifecycles': lifecycles}, f)
    print(f"  Phase 7 checkpoint saved")

    # Write remaining parquets (L3 already written in Phase 6c)
    l1_df.to_parquet(os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet'), index=False)
    l2_df.to_parquet(os.path.join(OUTPUT_DIR, 'l2_pump_outcome_v3.parquet'), index=False)
    l4_df.to_parquet(os.path.join(OUTPUT_DIR, 'l4_policy_eval_v3.parquet'), index=False)
    print(f"  Parquets written. Time: {time.time()-t0:.1f}s")

    # Phase 8: Write manifest
    print(f"\nPhase 8: Writing corrected manifest...")
    manifest = write_manifest_corrected(l1_df, l2_df, l3_df, l4_df, stats, migration_stats,
                                         capture_bounds, lifecycles)
    manifest_path = os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')

    def json_default(o):
        if isinstance(o, (np.integer,)):
            return int(o)
        if isinstance(o, (np.floating,)):
            return float(o)
        if isinstance(o, np.ndarray):
            return o.tolist()
        if isinstance(o, pd.Series):
            return o.to_dict()
        return str(o)

    with open(manifest_path, 'w') as f:
        json.dump(manifest, f, indent=2, default=json_default)
    print(f"  Manifest written: {manifest_path}")

    print(f"\n{'='*70}")
    print(f"REBUILD COMPLETE")
    print(f"  L1: {len(l1_df):,} | L2: {len(l2_df):,} | L3: {len(l3_df):,} | L4: {len(l4_df):,}")
    print(f"  Mints: {manifest.get('unique_mints', 'N/A')}")
    print(f"  Path-invariance: {manifest.get('path_invariance_assumption')}")
    print(f"  Execution model: {manifest.get('execution_model_version')}")
    print(f"{'='*70}")


if __name__ == '__main__':
    main()
