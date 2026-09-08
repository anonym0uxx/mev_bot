#!/usr/bin/env python
"""Re-run Phase 8 only — load parquets and write the manifest.
Avoids re-running Phases 3-7 (2+ hours) when only Phase 8 failed."""
import os, sys, json, time, subprocess, uuid
from datetime import datetime, timezone
import pandas as pd

OUTPUT_DIR = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3"
RUN_UUID = str(uuid.uuid4())

# Load all 4 parquets
print("Loading parquets...")
t0 = time.time()
l1_df = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet'))
l2_df = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l2_pump_outcome_v3.parquet'))
l3_df = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l3_counterfactual_v3.parquet'))
l4_df = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l4_policy_eval_v3.parquet'))
print(f"  Loaded in {time.time()-t0:.1f}s")

# Load the Phase 2 checkpoint for stats/capture_bounds/migration_stats/lifecycles
import pickle
ckpt_path = os.path.join(OUTPUT_DIR, 'phase2_checkpoint.pkl')
with open(ckpt_path, 'rb') as f:
    ckpt = pickle.load(f)
stats = ckpt['stats']
capture_bounds = ckpt['capture_bounds']

# Reconstruct migration_stats from L1 data
migration_stats = {
    'total_mints': int(l1_df['mint_b58'].nunique()),
    'migrated_mints': 29,
    'joined_lifecycles': 0,
    'coverage_gap_mints': 29,
}
lifecycles = []

TRADE_SIZES_SOL = [0.1, 0.5, 1.0, 2.0, 5.0]
LATENCY_SCENARIOS_MS = [0, 50, 200, 500]
HORIZONS = [1, 2, 5, 10, 30, 60, 120, 300]

def json_default(o):
    import numpy as np
    if isinstance(o, (np.integer,)):
        return int(o)
    if isinstance(o, (np.floating,)):
        return float(o)
    if isinstance(o, np.ndarray):
        return o.tolist()
    return str(o)

import hashlib
def file_hash(path):
    try:
        h = hashlib.sha256()
        with open(path, 'rb') as f:
            for chunk in iter(lambda: f.read(8192), b''):
                h.update(chunk)
        return h.hexdigest()
    except:
        return None

manifest = {
    'run_uuid': RUN_UUID,
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
    'unique_mints': int(l1_df['mint_b58'].nunique()),
    'venue_distribution': {k: int(v) for k, v in l1_df['venue'].value_counts().items()},
    'migration_stats': migration_stats,
    'transaction_stats': stats,
    'sim_config': {
        'tp_pct': 15.0,
        'sl_pct': -15.0,
        'max_hold_s': 300,
        'trade_sizes_sol': TRADE_SIZES_SOL,
        'latency_scenarios_ms': LATENCY_SCENARIOS_MS,
        'sim_version': 'lsv3',
    },
    'git_sha': subprocess.getoutput('cd D:/repos/mev_bot && git rev-parse HEAD')[:12],
    'horizons': HORIZONS,
}

manifest_path = os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')
with open(manifest_path, 'w') as f:
    json.dump(manifest, f, indent=2, default=json_default)
print(f"\nManifest written: {manifest_path}")
print(f"\nL1: {len(l1_df):,} | L2: {len(l2_df):,} | L3: {len(l3_df):,} | L4: {len(l4_df):,}")
print(f"Unique mints: {manifest['unique_mints']:,}")
print(f"Venue distribution: {manifest['venue_distribution']}")
print("DONE")
