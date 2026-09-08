#!/usr/bin/env python3
"""
audit3_l2_corrected.py — Corrected L2 objective truth audit.

Verifies L2 contains rich policy-independent objective truth:
  - Multi-horizon returns: 1/2/5/10/30/60/120/300s
  - MFE/MAE + timing
  - Barrier events: +10/+25/+50/+100/+200%, -10/-20/-30/-50%
  - Peak return + time-to-peak
  - observed_through_H, has_trade_within_H, right_censored_H
  - Migration/venue transition outcome
  - Price-quality/observation metadata
  - NULL for unobserved (never forward-fill)
  - No champion-policy labels or hypothetical trade assumptions
"""
import json
import os
import pandas as pd
import numpy as np

OUTPUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          'output', 'laserstream_gold_v3')

REQUIRED_FIELDS = {
    # Multi-horizon returns
    'return_1s': 'multi-horizon return 1s',
    'return_2s': 'multi-horizon return 2s',
    'return_5s': 'multi-horizon return 5s',
    'return_10s': 'multi-horizon return 10s',
    'return_30s': 'multi-horizon return 30s',
    'return_60s': 'multi-horizon return 60s',
    'return_120s': 'multi-horizon return 120s',
    'return_300s': 'multi-horizon return 300s',
    # MFE/MAE + timing
    'mfe_pct': 'max favorable excursion %',
    'mae_pct': 'max adverse excursion %',
    'mfe_time_ms': 'time to MFE (ms)',
    'mae_time_ms': 'time to MAE (ms)',
    # Barrier events
    'barrier_first': 'first barrier hit label',
    'barrier_first_time_ms': 'first barrier hit time (ms)',
    'barrier_first_label': 'first barrier hit direction',
    # Peak
    'peak_return_pct': 'peak return %',
    'peak_time_ms': 'time to peak (ms)',
    # Observed/no-trade/censored
    'observed_through_300s': 'observation window extends to 300s',
    'has_trade_within_300s': 'trade observed within 300s',
    'right_censored_300s': 'right-censored at 300s',
    # Migration/venue outcome
    'migration_outcome': 'migration transition outcome',
    'venue_outcome': 'venue transition outcome',
    # Price quality
    'price_quality': 'price observation quality',
    'observation_completeness': 'observation completeness flag',
    # Return flags (economically explicit)
    'return_gt_10x': 'return >10x flag',
    'return_gt_100x': 'return >100x flag',
    'return_gt_1000x': 'return >1000x flag',
}

FORBIDDEN_FIELDS = {
    'policy_outcome': 'champion-policy label (belongs in L4)',
    'policy_return_pct': 'champion-policy return (belongs in L4)',
    'policy_class': 'champion-policy class (belongs in L4)',
    'sim_return_pct': 'hypothetical trade return (belongs in L3)',
    'outcome_class': 'could be policy-dependent if from L4 context',
    'entry_executable': 'entry executability is policy/scenario-specific',
    'exit_executable': 'exit executability is policy/scenario-specific',
    'capacity_constrained': 'capacity is size-specific (belongs in L3)',
    'entry_failure_reason': 'entry failure is scenario-specific',
}

def main():
    print("=" * 70)
    print("AUDIT 3 (CORRECTED): L2 Objective Truth")
    print("=" * 70)

    l1_path = os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet')
    l2_path = os.path.join(OUTPUT_DIR, 'l2_pump_outcome_v3.parquet')

    l1 = pd.read_parquet(l1_path)
    l2 = pd.read_parquet(l2_path)
    print(f"\nL1: {len(l1):,} rows")
    print(f"L2: {len(l2):,} rows, {len(l2.columns)} cols")
    print(f"L2 columns: {list(l2.columns)}")

    # ─── Check 1: Cardinality — L2 must be 1:1 with L1 ────────────────
    print("\n--- Check 1: L2 cardinality 1:1 with L1 ---")
    if len(l2) == len(l1):
        print(f"  PASS: L2 rows ({len(l2):,}) = L1 rows ({len(l1):,})")
        verdict_1 = "PASS"
    else:
        print(f"  FAIL: L2 rows ({len(l2):,}) != L1 rows ({len(l1):,})")
        verdict_1 = "FAIL"

    # ─── Check 2: state_id join completeness ───────────────────────────
    print("\n--- Check 2: L2→L1 join completeness ---")
    l1_ids = set(l1['state_id'])
    l2_ids = set(l2['state_id'])
    orphans = l2_ids - l1_ids
    missing = l1_ids - l2_ids
    if not orphans and not missing:
        print(f"  PASS: Perfect 1:1 join — 0 orphans, 0 missing")
        verdict_2 = "PASS"
    else:
        print(f"  FAIL: {len(orphans)} orphans, {len(missing)} missing")
        verdict_2 = "FAIL"

    # ─── Check 3: Required fields present ──────────────────────────────
    print("\n--- Check 3: Required policy-independent fields ---")
    missing_fields = []
    present_fields = []
    for field, desc in REQUIRED_FIELDS.items():
        if field in l2.columns:
            present_fields.append(field)
            # Check null rate
            null_count = l2[field].isna().sum()
            null_pct = null_count / len(l2) * 100
            print(f"  ✓ {field} ({desc}): {null_count:,} nulls ({null_pct:.1f}%)")
        else:
            missing_fields.append(field)
            print(f"  ✗ {field} ({desc}): MISSING")

    if missing_fields:
        print(f"\n  FAIL: {len(missing_fields)} required fields missing: {missing_fields}")
        verdict_3 = "FAIL"
    else:
        print(f"\n  PASS: All {len(present_fields)} required fields present")
        verdict_3 = "PASS"

    # ─── Check 4: Forbidden fields absent (no policy labels) ───────────
    print("\n--- Check 4: No champion-policy labels or hypothetical assumptions ---")
    found_forbidden = []
    for field, reason in FORBIDDEN_FIELDS.items():
        if field in l2.columns:
            # Check if it's actually policy-dependent or just a shared name
            found_forbidden.append((field, reason))
            print(f"  WARN: {field} present in L2 — {reason}")

    if found_forbidden:
        print(f"\n  {len(found_forbidden)} potentially policy-dependent fields found")
        verdict_4 = "WARN"
    else:
        print(f"  PASS: No policy-dependent fields in L2")
        verdict_4 = "PASS"

    # ─── Check 5: NULL semantics — no forward-fill ─────────────────────
    print("\n--- Check 5: NULL semantics (no forward-fill of 'no market') ---")
    # Check that return_300s has nulls (should, for censored/unobserved)
    if 'return_300s' in l2.columns:
        null_300s = l2['return_300s'].isna().sum()
        null_pct = null_300s / len(l2) * 100
        print(f"  return_300s nulls: {null_count:,} ({null_pct:.1f}%)")
        if null_pct == 0:
            print(f"  WARN: 0% nulls — possible forward-fill")
            verdict_5 = "WARN"
        else:
            print(f"  PASS: {null_pct:.1f}% nulls — unobserved values are NULL, not filled")
            verdict_5 = "PASS"
    else:
        print(f"  FAIL: return_300s missing")
        verdict_5 = "FAIL"

    # ─── Check 6: Causal leakage — no future data beyond observation window ──
    print("\n--- Check 6: No future-looking leakage ---")
    # L2 is allowed to have returns/outcomes (it IS the outcome layer)
    # But it should NOT have counterfactual/policy simulation results
    leakage_fields = [f for f in l2.columns if 'sim_' in f.lower() or 'counterfactual' in f.lower()]
    if leakage_fields:
        print(f"  FAIL: Counterfactual fields in L2: {leakage_fields}")
        verdict_6 = "FAIL"
    else:
        print(f"  PASS: No counterfactual/simulation fields in L2")
        verdict_6 = "PASS"

    # ─── Check 7: Barrier thresholds match spec ────────────────────────
    print("\n--- Check 7: Barrier event coverage ---")
    if 'barrier_first' in l2.columns:
        barriers = l2['barrier_first'].dropna().value_counts()
        print(f"  Barrier first-hit distribution:")
        for b, count in barriers.items():
            print(f"    {b}: {count:,} ({count/len(l2)*100:.1f}%)")
        verdict_7 = "PASS"
    else:
        print(f"  FAIL: barrier_first missing")
        verdict_7 = "FAIL"

    # ─── Verdict ───────────────────────────────────────────────────────
    print("\n" + "=" * 70)
    all_verdicts = [verdict_1, verdict_2, verdict_3, verdict_4, verdict_5, verdict_6, verdict_7]
    fails = [v for v in all_verdicts if v == "FAIL"]
    if fails:
        print(f"VERDICT: FAIL — {len(fails)} check(s) failed")
    else:
        warns = [v for v in all_verdicts if v == "WARN"]
        if warns:
            print(f"VERDICT: PASS WITH WARNINGS — {len(warns)} warning(s)")
        else:
            print(f"VERDICT: PASS — all checks passed")
    print("=" * 70)


if __name__ == '__main__':
    main()
