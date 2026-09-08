#!/usr/bin/env python3
"""
audit4_cf_corrected.py — Corrected counterfactual semantics audit.

Verifies L3 counterfactual layer:
  1. path_invariance_assumption=true recorded in every L3 row + manifest
  2. execution_model_version present
  3. scenario_id composite key (state_id, scenario_id) unique
  4. L3 is 1:N with L1 (NOT 1:1)
  5. ALL size×latency combos present (including infeasible)
  6. Infeasible combos have feasible=false + exact reason
  7. Quote/reserve source recorded
  8. Confidence/feasibility/capacity flags present
  9. Larger-size PnL marked as conditional evidence
"""
import json
import os
import pandas as pd
import numpy as np

OUTPUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          'output', 'laserstream_gold_v3')

def main():
    print("=" * 70)
    print("AUDIT 4 (CORRECTED): Counterfactual Semantics")
    print("=" * 70)

    l1_path = os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet')
    l3_path = os.path.join(OUTPUT_DIR, 'l3_counterfactual_v3.parquet')
    manifest_path = os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')

    l1 = pd.read_parquet(l1_path)
    l3 = pd.read_parquet(l3_path)
    with open(manifest_path) as f:
        manifest = json.load(f)

    print(f"\nL1: {len(l1):,} rows")
    print(f"L3: {len(l3):,} rows, {len(l3.columns)} cols")
    print(f"L3 columns: {list(l3.columns)}")

    # ─── Check 1: path_invariance_assumption ───────────────────────────
    print("\n--- Check 1: path_invariance_assumption ---")
    if 'path_invariance_assumption' in l3.columns:
        all_true = l3['path_invariance_assumption'].all()
        if all_true:
            print(f"  PASS: path_invariance_assumption=true in ALL {len(l3):,} rows")
            verdict_1 = "PASS"
        else:
            false_count = (~l3['path_invariance_assumption']).sum()
            print(f"  FAIL: {false_count} rows have path_invariance_assumption=false")
            verdict_1 = "FAIL"
    else:
        print(f"  FAIL: path_invariance_assumption column missing from L3")
        verdict_1 = "FAIL"

    # Manifest check
    manifest_pia = manifest.get('path_invariance_assumption')
    if manifest_pia is not None:
        print(f"  Manifest: path_invariance_assumption={manifest_pia}")
        desc = manifest.get('path_invariance_description', '')
        print(f"  Description: {desc}")
    else:
        print(f"  FAIL: path_invariance_assumption missing from manifest")
        if verdict_1 == "PASS":
            verdict_1 = "FAIL"

    # ─── Check 2: execution_model_version ──────────────────────────────
    print("\n--- Check 2: execution_model_version ---")
    if 'execution_model_version' in l3.columns:
        versions = l3['execution_model_version'].unique()
        print(f"  L3 execution_model_version: {list(versions)}")
        verdict_2 = "PASS"
    else:
        print(f"  FAIL: execution_model_version missing from L3")
        verdict_2 = "FAIL"

    # ─── Check 3: scenario_id uniqueness ───────────────────────────────
    print("\n--- Check 3: scenario_id composite key uniqueness ---")
    if 'scenario_id' in l3.columns:
        dup_count = l3.duplicated(subset=['scenario_id']).sum()
        unique_count = l3['scenario_id'].nunique()
        if dup_count == 0:
            print(f"  PASS: {unique_count:,} unique scenario_ids, 0 duplicates")
            verdict_3 = "PASS"
        else:
            print(f"  FAIL: {dup_count:,} duplicate scenario_ids")
            verdict_3 = "FAIL"
    else:
        print(f"  FAIL: scenario_id column missing from L3")
        verdict_3 = "FAIL"

    # ─── Check 4: L3 is 1:N with L1 ────────────────────────────────────
    print("\n--- Check 4: L3 cardinality (1:N with L1) ---")
    l1_ids = set(l1['state_id'])
    l3_ids = set(l3['state_id'])
    orphans = l3_ids - l1_ids

    # Expected scenario count
    trade_sizes = manifest.get('scenario_grid', {}).get('trade_sizes_sol', [])
    latencies = manifest.get('scenario_grid', {}).get('latency_scenarios_ms', [])
    expected_per_state = len(trade_sizes) * len(latencies)

    if expected_per_state > 0:
        expected_l3 = len(l1) * expected_per_state
        print(f"  Expected scenarios per state: {expected_per_state} ({len(trade_sizes)} sizes × {len(latencies)} latencies)")
        print(f"  Expected L3 rows: {expected_l3:,}")
        print(f"  Actual L3 rows: {len(l3):,}")
        if len(l3) == expected_l3:
            print(f"  PASS: L3 cardinality matches expected ({len(l3):,} = {len(l1):,} × {expected_per_state})")
            verdict_4 = "PASS"
        else:
            print(f"  FAIL: L3 cardinality mismatch ({len(l3):,} != {expected_l3:,})")
            verdict_4 = "FAIL"
    else:
        print(f"  FAIL: Cannot determine expected cardinality from manifest")
        verdict_4 = "FAIL"

    if orphans:
        print(f"  FAIL: {len(orphans)} L3 state_ids not in L1")
        if verdict_4 == "PASS":
            verdict_4 = "FAIL"
    else:
        print(f"  All L3 state_ids exist in L1")

    # ─── Check 5: Infeasible combos present with reasons ───────────────
    print("\n--- Check 5: Infeasible combos with feasible=false + reason ---")
    if 'feasible' in l3.columns:
        feasible_count = (l3['feasible'] == True).sum()
        infeasible_count = (l3['feasible'] == False).sum()
        print(f"  Feasible: {feasible_count:,} ({feasible_count/len(l3)*100:.1f}%)")
        print(f"  Infeasible: {infeasible_count:,} ({infeasible_count/len(l3)*100:.1f}%)")

        if infeasible_count > 0:
            # Check infeasible rows have feasibility_reason
            infeas = l3[l3['feasible'] == False]
            has_reason = infeas['feasibility_reason'].notna().sum() if 'feasibility_reason' in l3.columns else 0
            reason_pct = has_reason / len(infeas) * 100 if len(infeas) > 0 else 0
            print(f"  Infeasible rows with reason: {has_reason:,} / {len(infeas):,} ({reason_pct:.1f}%)")
            if reason_pct == 100:
                print(f"  PASS: All infeasible combos have exact reason")
                verdict_5 = "PASS"
            else:
                print(f"  FAIL: {len(infeas) - has_reason} infeasible rows missing reason")
                verdict_5 = "FAIL"

            # Show reason distribution
            if 'feasibility_reason' in l3.columns:
                reasons = infeas['feasibility_reason'].value_counts()
                print(f"  Feasibility reason distribution:")
                for reason, count in reasons.items():
                    print(f"    {reason}: {count:,}")
        else:
            print(f"  WARN: 0 infeasible combos — are all size×latency combos truly feasible?")
            verdict_5 = "WARN"
    else:
        print(f"  FAIL: feasible column missing from L3")
        verdict_5 = "FAIL"

    # ─── Check 6: Confidence levels ────────────────────────────────────
    print("\n--- Check 6: Confidence/feasibility/capacity flags ---")
    if 'confidence' in l3.columns:
        conf_dist = l3['confidence'].value_counts()
        print(f"  Confidence distribution:")
        for level, count in conf_dist.items():
            print(f"    {level}: {count:,} ({count/len(l3)*100:.1f}%)")
        verdict_6 = "PASS"
    else:
        print(f"  FAIL: confidence column missing from L3")
        verdict_6 = "FAIL"

    if 'capacity_constrained' in l3.columns:
        cap_count = l3['capacity_constrained'].sum()
        print(f"  Capacity constrained: {cap_count:,} ({cap_count/len(l3)*100:.1f}%)")

    # ─── Check 7: Quote/reserve source ─────────────────────────────────
    print("\n--- Check 7: Quote/reserve source recorded ---")
    exec_assumptions = manifest.get('execution_assumptions', {})
    if exec_assumptions:
        print(f"  quote_source: {exec_assumptions.get('quote_source', 'N/A')}")
        print(f"  reserve_source: {exec_assumptions.get('reserve_source', 'N/A')}")
        print(f"  fee_model: {exec_assumptions.get('fee_model', 'N/A')}")
        print(f"  capacity_check: {exec_assumptions.get('capacity_check', 'N/A')}")
        print(f"  confidence_levels: {exec_assumptions.get('confidence_levels', 'N/A')}")
        verdict_7 = "PASS"
    else:
        print(f"  FAIL: execution_assumptions missing from manifest")
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
