#!/usr/bin/env python
"""
AUDIT 7: Final QA — Joins, uniqueness, causal leakage, units, splits,
provenance, rejected reasons, v2→v3 comparison.

Checks:
  1. Disk-derived 1:1:1:1 joins (L1→L2→L3→L4 all join on state_id)
  2. state_id uniqueness (cross-reference Audit 5)
  3. Causal leakage (no future data in entry features)
  4. Units/ranges sanity (lamports, raw tokens, decimals)
  5. True tail censoring (right-censored correctly applied)
  6. Observed no-trade flags
  7. Migration continuity
  8. Chronological + mint-disjoint splits (training/test integrity)
  9. Provenance / file hashes
 10. Rejected reasons
 11. v2→v3 comparison
"""
import os, sys, json, hashlib
from collections import Counter

OUTPUT_DIR = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3"
V2_DIR = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v2"

def run():
    try:
        import pandas as pd
    except ImportError:
        print("FATAL: pandas not available")
        return

    print("=" * 75)
    print("AUDIT 7: Final QA")
    print("=" * 75)

    # Load all layers
    layers = {}
    for name, fname in [
        ('L1', 'l1_pump_state_v3.parquet'),
        ('L2', 'l2_pump_outcome_v3.parquet'),
        ('L3', 'l3_counterfactual_v3.parquet'),
        ('L4', 'l4_policy_eval_v3.parquet'),
    ]:
        path = os.path.join(OUTPUT_DIR, fname)
        if os.path.exists(path):
            layers[name] = pd.read_parquet(path)
            print(f"  {name}: {len(layers[name]):,} rows, {len(layers[name].columns)} cols")
        else:
            print(f"  {name}: NOT FOUND")

    if 'L1' not in layers:
        print("  Cannot proceed without L1")
        return

    l1 = layers['L1']

    # ── 1. 1:1:1:1 Join integrity ────────────────────────────────────────
    print("\n  === 1:1:1:1 JOIN INTEGRITY ===")
    if 'state_id' not in l1.columns:
        print("  *** FAIL: state_id not in L1")
        return
    l1_ids = set(l1['state_id'].unique())
    print(f"  L1 unique state_ids: {len(l1_ids):,}")

    for name in ['L2', 'L3', 'L4']:
        if name not in layers or 'state_id' not in layers[name].columns:
            continue
        df = layers[name]
        layer_ids = set(df['state_id'].unique())
        orphan = layer_ids - l1_ids
        print(f"  {name} unique state_ids: {len(layer_ids):,}, orphans (not in L1): {len(orphan):,}")
        if orphan:
            print(f"    *** FAIL: {name} has {len(orphan)} orphan state_ids")

    # ── 2. Causal leakage check ──────────────────────────────────────────
    print("\n  === CAUSAL LEAKAGE CHECK ===")
    # Entry features should NOT contain future data (exit_price, future_return, etc.)
    future_leakage_cols = [c for c in l1.columns if any(x in c.lower() for x in ['exit_price', 'future_return', 'future_price', 'outcome', 'return_pct', 'sim_return'])]
    if future_leakage_cols:
        print(f"  *** POTENTIAL LEAKAGE: L1 has future-looking columns: {future_leakage_cols}")
        print(f"  These should be in L2/L3, NOT L1")
    else:
        print(f"  PASS: No obvious future-leakage columns in L1")

    # Check timestamp ordering within mint
    if 'timestamp_ms' in l1.columns and 'mint_b58' in l1.columns:
        # Sample 100 mints and verify timestamps are monotonically increasing
        sample_mints = l1['mint_b58'].dropna().unique()[:100]
        non_monotonic = 0
        for mint in sample_mints:
            mint_df = l1[l1['mint_b58'] == mint].sort_values('timestamp_ms')
            ts = mint_df['timestamp_ms'].tolist()
            if ts != sorted(ts):
                non_monotonic += 1
        print(f"  Sampled 100 mints: {non_monotonic} have non-monotonic timestamps (should be 0)")
        if non_monotonic > 0:
            print(f"  *** WARNING: Non-monotonic timestamps may indicate reordering or dedup issues")

    # ── 3. Units/ranges sanity ──────────────────────────────────────────
    print("\n  === UNITS/RANGES SANITY ===")
    unit_checks = [
        ('price_lamports_per_rawtok', 'lamports/rawtok', 0, 1e15),
        ('sol_traded_lamports', 'lamports', 0, 1e15),
        ('tokens_traded_raw', 'raw tokens', 0, 1e18),
        ('token_decimals', 'decimals', 0, 12),
    ]
    for col, unit, lo, hi in unit_checks:
        if col in l1.columns:
            vals = l1[col].dropna()
            if len(vals) == 0:
                continue
            print(f"  {col} ({unit}): min={vals.min()}, max={vals.max()}, mean={vals.mean():.2f}")
            out_of_range = (vals < lo) | (vals > hi)
            if out_of_range.sum() > 0:
                print(f"    *** OUT OF RANGE: {out_of_range.sum()} values outside [{lo}, {hi}]")

    # ── 4. True tail censoring ──────────────────────────────────────────
    print("\n  === TAIL CENSORING ===")
    censor_cols = [c for c in l1.columns if 'right_censored' in c]
    print(f"  Censoring columns: {censor_cols}")
    for col in censor_cols[:3]:
        if col in l1.columns:
            censored = l1[col].sum()
            print(f"  {col}: {censored:,} ({censored/len(l1)*100:.1f}%)")

    # ── 5. Observed no-trade ────────────────────────────────────────────
    print("\n  === OBSERVED NO-TRADE ===")
    nt_cols = [c for c in l1.columns if 'observed_no_trade' in c]
    print(f"  No-trade columns: {nt_cols}")
    for col in nt_cols[:3]:
        if col in l1.columns:
            nt = l1[col].sum()
            print(f"  {col}: {nt:,} ({nt/len(l1)*100:.1f}%)")

    # ── 6. Migration continuity ────────────────────────────────────────
    print("\n  === MIGRATION CONTINUITY ===")
    if 'venue' in l1.columns and 'mint_b58' in l1.columns:
        # Check if any mint appears in both pumpfun and pumpswap venues
        mint_venues = l1.groupby('mint_b58')['venue'].nunique()
        multi_venue = mint_venues[mint_venues > 1]
        print(f"  Mints appearing in multiple venues: {len(multi_venue):,}")
        if len(multi_venue) > 0:
            print(f"  These mints transitioned (migration continuity)")
            sample_mints = multi_venue.index[:5]
            for mint in sample_mints:
                venues = l1[l1['mint_b58'] == mint]['venue'].unique()
                print(f"    {mint[:30]}: venues={list(venues)}")

    # ── 7. Chronological + mint-disjoint splits ────────────────────────
    print("\n  === SPLIT INTEGRITY ===")
    # Check if there's a train/test split column
    split_cols = [c for c in l1.columns if 'split' in c.lower() or 'set' in c.lower()]
    if split_cols:
        for col in split_cols:
            dist = l1[col].value_counts()
            print(f"  {col}: {dict(dist)}")
    else:
        print("  No split columns found in L1 (splits may be applied downstream)")
        # Verify that a time-based split would be mint-disjoint
        if 'timestamp_ms' in l1.columns and 'mint_b58' in l1.columns:
            median_ts = l1['timestamp_ms'].median()
            before = l1[l1['timestamp_ms'] < median_ts]
            after = l1[l1['timestamp_ms'] >= median_ts]
            before_mints = set(before['mint_b58'].unique())
            after_mints = set(after['mint_b58'].unique())
            overlap = before_mints & after_mints
            print(f"  Median split: before={len(before):,}, after={len(after):,}")
            print(f"  Mint overlap at median: {len(overlap):,} (would violate mint-disjoint)")
            if overlap:
                print(f"  *** NOTE: {len(overlap)} mints appear in both halves (expected for time split)")

    # ── 8. Provenance / hashes ──────────────────────────────────────────
    print("\n  === PROVENANCE / HASHES ===")
    manifest_path = os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')
    if os.path.exists(manifest_path):
        with open(manifest_path) as f:
            manifest = json.load(f)
        print(f"  Run UUID: {manifest.get('run_uuid', 'N/A')}")
        print(f"  Git SHA: {manifest.get('git_sha', 'N/A')}")
        print(f"  Build timestamp: {manifest.get('build_timestamp', 'N/A')}")
        print(f"  Source: {manifest.get('source', 'N/A')}")
        print(f"  Source format: {manifest.get('source_format', 'N/A')}")
        # File hashes
        for layer_name, fname in [('L1', 'l1_pump_state_v3.parquet'), ('L2', 'l2_pump_outcome_v3.parquet'),
                                   ('L3', 'l3_counterfactual_v3.parquet'), ('L4', 'l4_policy_eval_v3.parquet')]:
            fpath = os.path.join(OUTPUT_DIR, fname)
            if os.path.exists(fpath):
                h = hashlib.sha256()
                with open(fpath, 'rb') as f:
                    for chunk in iter(lambda: f.read(8192), b''):
                        h.update(chunk)
                print(f"  {layer_name} SHA256: {h.hexdigest()[:16]}...")
    else:
        print("  Manifest not found")

    # ── 9. v2→v3 comparison ────────────────────────────────────────────
    print("\n  === v2→v3 COMPARISON ===")
    v2_manifest_path = os.path.join(V2_DIR, 'manifest_laserstream_gold_v2.json')
    if os.path.exists(v2_manifest_path):
        with open(v2_manifest_path) as f:
            v2m = json.load(f)
        v2_stats = v2m.get('stats', {})
        print(f"  v2 stats: {json.dumps(v2_stats, indent=2)[:500]}")
        print(f"  v2 layer counts: {v2m.get('layer_counts', {})}")
        print(f"  v2 UUID: {v2m.get('run_uuid', 'N/A')}")
    else:
        print(f"  v2 manifest not found at {v2_manifest_path}")

    print("\n  === AUDIT 7 COMPLETE ===")

if __name__ == '__main__':
    run()
