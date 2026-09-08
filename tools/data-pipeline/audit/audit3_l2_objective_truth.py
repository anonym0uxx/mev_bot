#!/usr/bin/env python
"""
AUDIT 3: L2 Objective Truth — Verify rich policy-independent fields are preserved.

The user requires L2 to preserve rich policy-independent truth:
  - Returns at 1/2/5/10/30/60/120/300s horizons
  - MFE/MAE + timing
  - Barrier-first events
  - Peak/time
  - No-trade flags
  - Observed-through / right-censored per horizon
  - Migration/venue outcome
  - Executable-observation metadata

L3 remains separate counterfactual economics.

This audit checks the L2 parquet schema and reports what's present vs missing.
"""
import os, sys, json

OUTPUT_DIR = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3"

REQUIRED_L2_FIELDS = {
    # Returns per horizon
    'return_1s': 'Returns at 1s horizon',
    'return_2s': 'Returns at 2s horizon',
    'return_5s': 'Returns at 5s horizon',
    'return_10s': 'Returns at 10s horizon',
    'return_30s': 'Returns at 30s horizon',
    'return_60s': 'Returns at 60s horizon',
    'return_120s': 'Returns at 120s horizon',
    'return_300s': 'Returns at 300s horizon',
    # MFE/MAE + timing
    'mfe_pct': 'Maximum Favorable Excursion (%)',
    'mae_pct': 'Maximum Adverse Excursion (%)',
    'mfe_time_ms': 'Time to MFE (ms)',
    'mae_time_ms': 'Time to MAE (ms)',
    # Barrier-first events
    'barrier_first': 'First barrier hit (tp/sl/timeout)',
    'barrier_first_time_ms': 'Time to first barrier (ms)',
    # Peak/time
    'peak_return_pct': 'Peak return within horizon',
    'peak_time_ms': 'Time to peak (ms)',
    # No-trade flags (already in L1 but should be reflected in L2)
    'observed_no_trade': 'No trade observed in horizon',
    # Censoring
    'right_censored': 'Right-censored (coverage boundary)',
    'observed_through': 'Observed through full horizon',
    # Migration/venue outcome
    'migration_outcome': 'Migration outcome (pre/post/none)',
    'venue_outcome': 'Venue outcome (same/transitioned)',
    # Executable-observation metadata
    'entry_executable': 'Entry was executable',
    'exit_executable': 'Exit was executable',
    'capacity_constrained': 'Capacity constrained',
    # Basic
    'state_id': 'State ID',
    'return_pct': 'Return percentage (benchmark)',
    'outcome_class': 'Outcome class',
}

def run():
    try:
        import pandas as pd
    except ImportError:
        print("FATAL: pandas not available")
        return

    print("=" * 75)
    print("AUDIT 3: L2 Objective Truth — Schema Verification")
    print("=" * 75)

    l2_path = os.path.join(OUTPUT_DIR, 'l2_pump_outcome_v3.parquet')
    if not os.path.exists(l2_path):
        print(f"  L2 not found at {l2_path}")
        print("  Build 3 must complete before this audit can run.")
        return

    l2 = pd.read_parquet(l2_path)
    print(f"\n  L2 rows: {len(l2):,}")
    print(f"  L2 columns ({len(l2.columns)}): {list(l2.columns)}")

    print("\n  === REQUIRED FIELD PRESENCE ===")
    present = 0
    missing = 0
    for field, desc in REQUIRED_L2_FIELDS.items():
        if field in l2.columns:
            present += 1
            # Check if field has non-null values
            non_null = l2[field].notna().sum()
            print(f"  ✓ {field}: {non_null:,} non-null ({desc})")
        else:
            missing += 1
            print(f"  ✗ {field}: MISSING ({desc})")

    print(f"\n  Summary: {present}/{len(REQUIRED_L2_FIELDS)} present, {missing} missing")
    if missing > 0:
        print(f"\n  *** FAIL: L2 is missing {missing} required policy-independent fields ***")
        print("  The current L2 only has return_pct/outcome_class — this is insufficient.")
        print("  L2 must preserve rich policy-independent truth as specified.")
    else:
        print("  PASS: All required fields present")

    # Also check L1 for censoring/no-trade fields (these should be in L1 and
    # joined into L2)
    print("\n  === L1 CENSORING/NO-TRADE FIELDS (should exist in L1) ===")
    l1_path = os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet')
    if os.path.exists(l1_path):
        l1 = pd.read_parquet(l1_path)
        for h in [1, 2, 5, 10, 30, 60, 120, 300]:
            rc = f'right_censored_{h}s'
            nt = f'observed_no_trade_{h}s'
            rc_present = rc in l1.columns
            nt_present = nt in l1.columns
            rc_nonnull = l1[rc].notna().sum() if rc_present else 0
            nt_nonnull = l1[nt].notna().sum() if nt_present else 0
            print(f"  {h}s: right_censored={'✓' if rc_present else '✗'} ({rc_nonnull:,}) | no_trade={'✓' if nt_present else '✗'} ({nt_nonnull:,})")

    print("\n  === AUDIT 3 COMPLETE ===")

if __name__ == '__main__':
    run()
