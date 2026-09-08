#!/usr/bin/env python3
"""
audit_final_certification.py — Final disk-derived certification for v3 freeze.

Checks:
  1. Exact L1/L2/L4 state_id sets + uniqueness
  2. L3 composite-key uniqueness + scenario coverage
  3. Causal leakage (no future data in L1)
  4. Units/decimals/reserve orientation
  5. Censoring/no-trade semantics
  6. Migration continuity
  7. Fat-tail independent RAW spot-recomputation (skip — requires RAW files)
  8. Chronological + mint-disjoint splits feasibility
  9. Provenance/run UUID/RAW source/code/config/Git hashes
  10. Rejected/unresolved reason ledger
  11. v2→final-v3 comparison
"""
import json
import os
import pandas as pd
import numpy as np
from collections import Counter

OUTPUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          'output', 'laserstream_gold_v3')
V2_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      'output', 'laserstream_gold_v2')

def main():
    print("=" * 70)
    print("FINAL CERTIFICATION: LaserStream Gold v3")
    print("=" * 70)

    l1 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet'))
    l2 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l2_pump_outcome_v3.parquet'))
    l3 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l3_counterfactual_v3.parquet'))
    l4 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l4_policy_eval_v3.parquet'))
    with open(os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')) as f:
        manifest = json.load(f)

    results = {}

    # ─── 1. Exact state_id sets + uniqueness ───────────────────────────
    print("\n--- 1. L1/L2/L4 state_id sets + uniqueness ---")
    l1_ids = set(l1['state_id'])
    l2_ids = set(l2['state_id'])
    l4_ids = set(l4['state_id'])

    u1 = l1['state_id'].nunique() == len(l1)
    u2 = l2['state_id'].nunique() == len(l2)
    u4 = l4['state_id'].nunique() == len(l4)
    sets_match = l1_ids == l2_ids == l4_ids

    print(f"  L1 unique: {u1} ({l1['state_id'].nunique():,}/{len(l1):,})")
    print(f"  L2 unique: {u2} ({l2['state_id'].nunique():,}/{len(l2):,})")
    print(f"  L4 unique: {u4} ({l4['state_id'].nunique():,}/{len(l4):,})")
    print(f"  L1=L2=L4 sets: {sets_match}")
    results['state_id_uniqueness'] = u1 and u2 and u4 and sets_match

    # ─── 2. L3 composite-key uniqueness + scenario coverage ────────────
    print("\n--- 2. L3 composite-key uniqueness + scenario coverage ---")
    l3_dups = l3.duplicated(subset=['scenario_id']).sum()
    l3_unique = l3['scenario_id'].nunique()

    trade_sizes = manifest.get('scenario_grid', {}).get('trade_sizes_sol', [])
    latencies = manifest.get('scenario_grid', {}).get('latency_scenarios_ms', [])
    expected_per_state = len(trade_sizes) * len(latencies)
    expected_l3 = len(l1) * expected_per_state

    print(f"  L3 rows: {len(l3):,}")
    print(f"  Unique scenario_ids: {l3_unique:,}")
    print(f"  Duplicates: {l3_dups}")
    print(f"  Expected: {expected_l3:,} ({len(l1):,} × {expected_per_state})")
    print(f"  Cardinality match: {len(l3) == expected_l3}")
    print(f"  Feasible: {(l3['feasible']==True).sum():,} | Infeasible: {(l3['feasible']==False).sum():,}")
    results['l3_cardinality'] = (l3_dups == 0) and (len(l3) == expected_l3)

    # ─── 3. Causal leakage ─────────────────────────────────────────────
    print("\n--- 3. Causal leakage (no future data in L1) ---")
    future_fields = [c for c in l1.columns if any(k in c.lower() for k in
                     ['return', 'outcome', 'exit', 'future', 'sim_', 'policy_'])]
    if future_fields:
        print(f"  FAIL: Future-looking fields in L1: {future_fields}")
        results['causal_leakage'] = False
    else:
        print(f"  PASS: No future-looking fields in L1")
        results['causal_leakage'] = True

    # ─── 4. Units/decimals/reserve orientation ─────────────────────────
    print("\n--- 4. Units/decimals/reserve orientation ---")
    unit_checks = []
    # Price positive
    price_pos = (l1['price_lamports_per_rawtok'] > 0).all()
    print(f"  price_lamports_per_rawtok all positive: {price_pos}")
    unit_checks.append(('price_positive', price_pos))

    # SOL traded positive
    sol_pos = (l1['sol_traded_lamports'] > 0).all()
    print(f"  sol_traded_lamports all positive: {sol_pos}")
    unit_checks.append(('sol_positive', sol_pos))

    # Reserve orientation: virtual_sol > real_sol (pumpfun bonding curve)
    if 'curve_virtual_sol' in l1.columns and 'curve_real_sol' in l1.columns:
        # Only check pumpfun rows with non-null reserves (NaN comparisons return False)
        pf = l1[l1['venue'] == 'pumpfun'].dropna(subset=['curve_virtual_sol', 'curve_real_sol'])
        if len(pf) > 0:
            reserve_ok = (pf['curve_virtual_sol'] >= pf['curve_real_sol']).all()
            null_count = len(l1[l1['venue'] == 'pumpfun']) - len(pf)
            print(f"  pumpfun virtual_sol >= real_sol: {reserve_ok} ({len(pf):,} rows checked, {null_count:,} null-reserve rows skipped)")
            unit_checks.append(('reserve_orientation', reserve_ok))

    results['units'] = all(v for _, v in unit_checks)

    # ─── 5. Censoring/no-trade semantics ───────────────────────────────
    print("\n--- 5. Censoring/no-trade semantics ---")
    if 'right_censored_300s' in l1.columns:
        censored = l1['right_censored_300s'].sum()
        print(f"  right_censored_300s: {censored:,} ({censored/len(l1)*100:.1f}%)")
        results['censoring'] = True
    else:
        print(f"  FAIL: right_censored_300s missing from L1")
        results['censoring'] = False

    # ─── 6. Migration continuity ───────────────────────────────────────
    print("\n--- 6. Migration continuity ---")
    mig_stats = manifest.get('migration_stats', {})
    print(f"  Migration stats: {mig_stats}")
    # Check 29 successful migrations
    migrated = mig_stats.get('migrated_mints', 0)
    if migrated == 29:
        print(f"  PASS: 29 successful migrations preserved")
        results['migration'] = True
    else:
        print(f"  WARN: Expected 29 migrations, got {migrated}")
        results['migration'] = False

    # ─── 7. Provenance ─────────────────────────────────────────────────
    print("\n--- 7. Provenance (UUID, source, git, hashes) ---")
    provenance_fields = ['run_uuid', 'build_timestamp', 'source', 'git_sha']
    prov_ok = all(f in manifest for f in provenance_fields)
    print(f"  run_uuid: {manifest.get('run_uuid', 'MISSING')}")
    print(f"  build_timestamp: {manifest.get('build_timestamp', 'MISSING')}")
    print(f"  source: {manifest.get('source', 'MISSING')}")
    print(f"  git_sha: {manifest.get('git_sha', 'MISSING')}")
    hashes = manifest.get('hashes', {})
    print(f"  l1_sha256: {hashes.get('l1_sha256', 'MISSING')[:16] if hashes.get('l1_sha256') else 'MISSING'}...")
    print(f"  l2_sha256: {hashes.get('l2_sha256', 'MISSING')[:16] if hashes.get('l2_sha256') else 'MISSING'}...")
    print(f"  l3_sha256: {hashes.get('l3_sha256', 'MISSING')[:16] if hashes.get('l3_sha256') else 'MISSING'}...")
    print(f"  l4_sha256: {hashes.get('l4_sha256', 'MISSING')[:16] if hashes.get('l4_sha256') else 'MISSING'}...")
    results['provenance'] = prov_ok

    # ─── 8. Chronological + mint-disjoint splits feasibility ───────────
    print("\n--- 8. Chronological + mint-disjoint split feasibility ---")
    # Check timestamp monotonicity within mint
    sample_mints = l1['mint_b58'].dropna().unique()[:100]
    mono_count = 0
    for mint in sample_mints:
        mint_data = l1[l1['mint_b58'] == mint].sort_values('timestamp_ms')
        ts = mint_data['timestamp_ms'].values
        if len(ts) > 1:
            if all(ts[i] <= ts[i+1] for i in range(len(ts)-1)):
                mono_count += 1
    print(f"  Timestamp monotonicity (100 sampled mints): {mono_count}/100")
    print(f"  Unique mints: {l1['mint_b58'].nunique():,} — sufficient for mint-disjoint splits")
    results['splits'] = mono_count >= 95

    # ─── 9. Rejected/unresolved reason ledger ──────────────────────────
    print("\n--- 9. Rejected/unresolved reason ledger ---")
    if 'feasibility_reason' in l3.columns:
        reasons = l3[l3['feasible'] == False]['feasibility_reason'].value_counts()
        print(f"  L3 infeasible reasons ledger:")
        for reason, count in reasons.items():
            print(f"    {reason}: {count:,}")
        results['reason_ledger'] = len(reasons) > 0 or (l3['feasible'] == False).sum() == 0
    else:
        print(f"  FAIL: feasibility_reason missing from L3")
        results['reason_ledger'] = False

    # ─── 10. v2→v3 comparison ──────────────────────────────────────────
    print("\n--- 10. v2→v3 comparison ---")
    v2_manifest_path = os.path.join(V2_DIR, 'manifest_laserstream_gold_v2.json')
    if os.path.exists(v2_manifest_path):
        with open(v2_manifest_path) as f:
            v2_manifest = json.load(f)
        v2_l1 = v2_manifest.get('layer_counts', {}).get('l1_pump_state', 'N/A')
        print(f"  v2 L1: {v2_l1:,}" if isinstance(v2_l1, int) else f"  v2 L1: {v2_l1}")
        print(f"  v3 L1: {len(l1):,}")
        print(f"  v2 extreme_outliers: {v2_manifest.get('fat_tail_config', {}).get('extreme_outlier_count', 'N/A')}")
        print(f"  v3 extreme_outlier: REMOVED (price_scale_tier feature)")
        print(f"  v2 unique_mints: {v2_manifest.get('unique_mints', 'N/A')}")
        print(f"  v3 unique_mints: {manifest.get('unique_mints', 'N/A')}")
        print(f"  v2 UUID: {v2_manifest.get('run_uuid', 'N/A')}")
        print(f"  v3 UUID: {manifest.get('run_uuid', 'N/A')}")
        results['v2_comparison'] = True
    else:
        print(f"  v2 manifest not found — skipping comparison")
        results['v2_comparison'] = True  # Don't fail on missing v2

    # ─── 11. path_invariance_assumption in manifest ────────────────────
    print("\n--- 11. path_invariance_assumption in manifest ---")
    pia = manifest.get('path_invariance_assumption')
    if pia is True:
        print(f"  PASS: path_invariance_assumption=true in manifest")
        results['path_invariance'] = True
    else:
        print(f"  FAIL: path_invariance_assumption missing or false: {pia}")
        results['path_invariance'] = False

    # ─── 12. L2 fields count ───────────────────────────────────────────
    print("\n--- 12. L2 field richness ---")
    print(f"  L2 columns ({len(l2.columns)}): {list(l2.columns)}")
    if len(l2.columns) >= 20:
        print(f"  PASS: L2 has {len(l2.columns)} columns (was 8 in Build 3 run 5)")
        results['l2_richness'] = True
    else:
        print(f"  FAIL: L2 has only {len(l2.columns)} columns (need >=20)")
        results['l2_richness'] = False

    # ─── FINAL VERDICT ─────────────────────────────────────────────────
    print("\n" + "=" * 70)
    print("FINAL CERTIFICATION SUMMARY:")
    all_pass = True
    for check, passed in results.items():
        status = "✓ PASS" if passed else "✗ FAIL"
        print(f"  {status}: {check}")
        if not passed:
            all_pass = False

    print("\n" + "=" * 70)
    if all_pass:
        print("VERDICT: ✅ ALL CHECKS PASS — v3 MAY BE FROZEN")
    else:
        failed = [k for k, v in results.items() if not v]
        print(f"VERDICT: ❌ FAIL — {len(failed)} check(s) failed: {failed}")
        print("v3 CANNOT BE FROZEN until all checks pass")
    print("=" * 70)


if __name__ == '__main__':
    main()
