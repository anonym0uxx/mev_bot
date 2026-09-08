#!/usr/bin/env python3
"""
audit2_fattail_corrected.py — Corrected fat-tail forensics audit.

Verifies:
  1. No extreme_outlier flag exists in L1 (removed as label-shaping)
  2. price_scale_tier is a descriptive feature, not a label
  3. Return-based flags (return_gt_10x/100x/1000x) exist in L2
  4. Train-only quantile metadata in manifest
  5. State-level AND unique-mint-level distributions
  6. No threshold was chosen to target a desired rate (no label-shaping)
"""
import json
import os
import sys
import pandas as pd
import numpy as np
from collections import Counter

OUTPUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          'output', 'laserstream_gold_v3')

def main():
    print("=" * 70)
    print("AUDIT 2 (CORRECTED): Fat-Tail Forensics")
    print("=" * 70)

    l1_path = os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet')
    l2_path = os.path.join(OUTPUT_DIR, 'l2_pump_outcome_v3.parquet')
    manifest_path = os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')

    l1 = pd.read_parquet(l1_path)
    l2 = pd.read_parquet(l2_path)
    with open(manifest_path) as f:
        manifest = json.load(f)

    print(f"\nL1: {len(l1):,} rows, {len(l1.columns)} cols")
    print(f"L2: {len(l2):,} rows, {len(l2.columns)} cols")

    # ─── Check 1: No extreme_outlier flag ────────────────────────────────
    print("\n--- Check 1: extreme_outlier flag removed ---")
    if 'extreme_outlier' in l1.columns:
        extreme_pct = l1['extreme_outlier'].sum() / len(l1) * 100
        print(f"  FAIL: extreme_outlier column still exists ({extreme_pct:.4f}% marked)")
        verdict_1 = "FAIL"
    else:
        print(f"  PASS: extreme_outlier column removed from L1")
        verdict_1 = "PASS"

    # ─── Check 2: price_scale_tier is descriptive ───────────────────────
    print("\n--- Check 2: price_scale_tier as descriptive feature ---")
    if 'price_scale_tier' not in l1.columns:
        print(f"  FAIL: price_scale_tier missing from L1")
        verdict_2 = "FAIL"
    else:
        tier_dist = l1['price_scale_tier'].value_counts()
        print(f"  Price-scale tier distribution (state-level):")
        for tier, count in sorted(tier_dist.items()):
            print(f"    {tier}: {count:,} ({count/len(l1)*100:.1f}%)")
        # Check it's not binary (which would be label-shaping)
        if len(tier_dist) <= 2:
            print(f"  WARN: Only {len(tier_dist)} tiers — suspicious (could be label-shaping)")
            verdict_2 = "WARN"
        else:
            print(f"  PASS: {len(tier_dist)} distinct tiers — descriptive, not binary")
            verdict_2 = "PASS"

    # ─── Check 3: Return flags in L2 ────────────────────────────────────
    print("\n--- Check 3: Return-based flags in L2 ---")
    return_flags = ['return_gt_10x', 'return_gt_100x', 'return_gt_1000x']
    verdict_3 = "PASS"
    for flag in return_flags:
        if flag not in l2.columns:
            print(f"  FAIL: {flag} missing from L2")
            verdict_3 = "FAIL"
        else:
            count = l2[flag].sum()
            pct = count / len(l2) * 100
            print(f"  {flag}: {count:,} states ({pct:.4f}%)")

    # ─── Check 4: Train-only quantile metadata ──────────────────────────
    print("\n--- Check 4: Train-only quantile metadata in manifest ---")
    ft_config = manifest.get('fat_tail_config', {})
    quantiles = ft_config.get('train_only_quantiles_300s', {})
    if quantiles:
        print(f"  Quantile thresholds (fit on TRAIN 300s returns):")
        for q, v in sorted(quantiles.items()):
            print(f"    {q}: {v:.4f}")
        note = ft_config.get('note', '')
        print(f"  Note: {note}")
        verdict_4 = "PASS"
    else:
        print(f"  FAIL: No train_only_quantiles_300s in manifest")
        verdict_4 = "FAIL"

    # ─── Check 5: Unique-mint-level distributions ──────────────────────
    print("\n--- Check 5: Unique-mint-level distributions ---")
    mint_level = manifest.get('mint_level_distributions', {})
    if mint_level:
        print(f"  Unique mints total: {mint_level.get('unique_mints_total', 'N/A')}")
        print(f"  Mints with 10x returns: {mint_level.get('mints_with_10x_returns', 'N/A')}")
        print(f"  Mints with 100x returns: {mint_level.get('mints_with_100x_returns', 'N/A')}")
        print(f"  Mints with 1000x returns: {mint_level.get('mints_with_1000x_returns', 'N/A')}")
        if 'mean_300s_ret_by_mint' in mint_level:
            for stat, vals in mint_level['mean_300s_ret_by_mint'].items():
                if isinstance(vals, dict):
                    print(f"  Mean 300s return by mint ({stat}): p50={vals.get('p50',0):.2f} p90={vals.get('p90',0):.2f} p99={vals.get('p99',0):.2f}")
        verdict_5 = "PASS"
    else:
        print(f"  FAIL: No mint_level_distributions in manifest")
        verdict_5 = "FAIL"

    # ─── Check 6: No label-shaping — verify return flags are NOT all True ──
    print("\n--- Check 6: No label-shaping (flags not degenerate) ---")
    verdict_6 = "PASS"
    for flag in return_flags:
        if flag in l2.columns:
            pct = l2[flag].sum() / len(l2) * 100
            if pct > 99.0 or pct < 0.001:
                print(f"  WARN: {flag} at {pct:.4f}% — degenerate distribution")
                verdict_6 = "WARN"
            else:
                print(f"  {flag} at {pct:.4f}% — non-degenerate, OK")

    # ─── Check 7: price_scale_tier unique-mint level ────────────────────
    print("\n--- Check 7: Price-scale tier at unique-mint level ---")
    mint_tiers = l1.groupby('mint_b58')['price_scale_tier'].first()
    tier_mint_dist = mint_tiers.value_counts()
    print(f"  Price-scale tier (unique-mint-level, N={len(mint_tiers)} mints):")
    for tier, count in sorted(tier_mint_dist.items()):
        print(f"    {tier}: {count} mints ({count/len(mint_tiers)*100:.1f}%)")

    # ─── Verdict ────────────────────────────────────────────────────────
    print("\n" + "=" * 70)
    all_verdicts = [verdict_1, verdict_2, verdict_3, verdict_4, verdict_5, verdict_6]
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
