#!/usr/bin/env python
"""
AUDIT 5: state_id Uniqueness — Whole-corpus uniqueness proof.

state_id = mint+slot+signature+ix_index is acceptable ONLY if exact whole-corpus
uniqueness proves one event/state per ID. If multiple inner/events can share
ix_index, include inner/event/log index or deterministic sequence.

Checks:
  1. state_id is present in ALL layers (L1, L2, L3, L4)
  2. state_id has NO duplicates in L1 (whole-corpus uniqueness)
  3. If duplicates exist, determine if inner_ix_index collisions are the cause
  4. Check if multiple inner instructions or events can share ix_index
  5. Verify NO row-index joins are used
"""
import os, sys, json
from collections import Counter

OUTPUT_DIR = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3"

def run():
    try:
        import pandas as pd
    except ImportError:
        print("FATAL: pandas not available")
        return

    print("=" * 75)
    print("AUDIT 5: state_id Uniqueness — Whole-Corpus Proof")
    print("=" * 75)

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
            print(f"  {name}: {len(layers[name]):,} rows")
        else:
            print(f"  {name}: NOT FOUND at {path}")

    if 'L1' not in layers:
        print("  Cannot proceed without L1")
        return

    l1 = layers['L1']

    # ── 1. state_id presence ─────────────────────────────────────────────
    print("\n  === state_id PRESENCE IN ALL LAYERS ===")
    for name, df in layers.items():
        has_sid = 'state_id' in df.columns
        if has_sid:
            nonnull = df['state_id'].notna().sum()
            print(f"  {name}: state_id present, {nonnull:,} non-null")
        else:
            print(f"  {name}: state_id MISSING ***")

    # ── 2. Whole-corpus uniqueness in L1 ─────────────────────────────────
    print("\n  === L1 WHOLE-CORPUS UNIQUENESS ===")
    total = len(l1)
    unique_ids = l1['state_id'].nunique()
    dup_count = total - unique_ids
    print(f"  Total rows: {total:,}")
    print(f"  Unique state_ids: {unique_ids:,}")
    print(f"  Duplicate state_ids: {dup_count:,}")
    if dup_count == 0:
        print(f"  PASS: state_id is globally unique in L1")
    else:
        print(f"  *** FAIL: {dup_count:,} duplicate state_ids detected")
        # Find the duplicates
        sid_counts = Counter(l1['state_id'].tolist())
        dups = [(sid, cnt) for sid, cnt in sid_counts.items() if cnt > 1]
        dups.sort(key=lambda x: -x[1])
        print(f"\n  Top 10 duplicate state_ids:")
        for sid, cnt in dups[:10]:
            print(f"    {sid[:60]}: {cnt} occurrences")
            # Show the duplicate rows
            dup_rows = l1[l1['state_id'] == sid]
            for _, row in dup_rows.head(3).iterrows():
                print(f"      mint={row.get('mint_b58', 'N/A')[:20]}, slot={row.get('slot', 'N/A')}, "
                      f"sig={row.get('signature', 'N/A')[:20]}, ix_idx={row.get('ix_index', 'N/A')}, "
                      f"venue={row.get('venue', 'N/A')}, side={row.get('trade_side', 'N/A')}")

    # ── 3. state_id composition check ────────────────────────────────────
    print("\n  === state_id COMPOSITION ===")
    # Sample a few state_ids to verify they contain mint+slot+signature+ix_index
    sample = l1['state_id'].dropna().head(5).tolist()
    for sid in sample:
        parts = sid.split('_') if '_' in sid else sid.split('-')
        print(f"  {sid[:80]}: {len(parts)} parts")

    # ── 4. ix_index collision check ──────────────────────────────────────
    print("\n  === ix_index COLLISION CHECK ===")
    # The L1 parquet may NOT carry 'signature' and 'ix_index' as separate columns
    # (only the composite state_id). If so, we check uniqueness via state_id only.
    needed_cols = ['slot', 'signature', 'ix_index']
    has_all = all(col in l1.columns for col in needed_cols)
    if has_all:
        key_cols = needed_cols
        grouped = l1.groupby(key_cols).size()
        multi = grouped[grouped > 1]
        print(f"  (slot, signature, ix_index) groups with >1 row: {len(multi):,}")
        if len(multi) > 0:
            print(f"  *** COLLISION: same (slot, sig, ix_index) produces multiple states")
            print(f"  This means state_id=mint+slot+signature+ix_index is INSUFFICIENT")
        else:
            print(f"  PASS: (slot, signature, ix_index) is unique — no collisions")
    else:
        missing = [c for c in needed_cols if c not in l1.columns]
        print(f"  Cannot decompose state_id — missing columns: {missing}")
        print(f"  L1 only has composite state_id, not raw components.")
        print(f"  This is a FINDING: raw join-key components (signature, ix_index)")
        print(f"  are NOT preserved in L1, preventing independent uniqueness verification.")
        print(f"  Whole-corpus uniqueness via state_id only: ", end="")
        if dup_count == 0:
            print(f"PASS ({unique_ids:,} unique)")
        else:
            print(f"FAIL ({dup_count:,} duplicates)")

    # ── 5. No row-index joins ────────────────────────────────────────────
    print("\n  === NO ROW-INDEX JOINS ===")
    # Check that no layer has a 'row_index' or 'l1_row_idx' column
    for name, df in layers.items():
        row_idx_cols = [c for c in df.columns if 'row_idx' in c.lower() or 'row_index' in c.lower() or c == 'index']
        if row_idx_cols:
            print(f"  *** FAIL: {name} has row-index column(s): {row_idx_cols}")
        else:
            print(f"  {name}: no row-index columns (PASS)")

    # ── 6. Cross-layer join integrity ────────────────────────────────────
    print("\n  === CROSS-LAYER JOIN INTEGRITY ===")
    if 'L2' in layers and 'state_id' in layers.get('L2', pd.DataFrame()).columns:
        l2 = layers['L2']
        l1_ids = set(l1['state_id'].unique())
        l2_ids = set(l2['state_id'].unique())
        l2_not_in_l1 = l2_ids - l1_ids
        print(f"  L2 state_ids not in L1: {len(l2_not_in_l1):,}")
        if len(l2_not_in_l1) > 0:
            print(f"  *** FAIL: L2 has state_ids not in L1 (orphan records)")
        else:
            print(f"  PASS: All L2 state_ids exist in L1 (1:1 join)")

    if 'L3' in layers and 'state_id' in layers.get('L3', pd.DataFrame()).columns:
        l3 = layers['L3']
        l1_ids = set(l1['state_id'].unique())
        l3_ids = set(l3['state_id'].unique())
        l3_not_in_l1 = l3_ids - l1_ids
        print(f"  L3 state_ids not in L1: {len(l3_not_in_l1):,}")
        if len(l3_not_in_l1) > 0:
            print(f"  *** FAIL: L3 has state_ids not in L1 (orphan records)")
            # Could be legitimate (L3 has multiple sizes per state_id, but all should trace to L1)
            sample_orphans = list(l3_not_in_l1)[:3]
            print(f"  Sample orphans: {sample_orphans}")
        else:
            print(f"  PASS: All L3 state_ids exist in L1")

    print("\n  === AUDIT 5 COMPLETE ===")

if __name__ == '__main__':
    run()
