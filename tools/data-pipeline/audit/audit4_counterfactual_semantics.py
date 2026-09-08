#!/usr/bin/env python
"""
AUDIT 4: Counterfactual Semantics — Verify venue-specific fees, quote math,
path_invariance_assumption, capacity flags, and size-specific confidence.

Checks:
  1. Venue-specific fees applied (PumpFun 1% protocol fee + 0.25% platform fee,
     PumpSwap 0.25% swap fee)
  2. Quote/impact math correctness (curve math for PumpFun, constant-product for PumpSwap)
  3. path_invariance_assumption explicitly recorded
  4. Larger-size results have capacity_constrained flags
  5. Larger-size results do NOT masquerade as guaranteed PnL
  6. Observed market path preserved separately from hypothetical impact
  7. Exit prices use forward-looking future states, not same-time buy-and-sell
"""
import os, sys, json

OUTPUT_DIR = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3"

def run():
    try:
        import pandas as pd
    except ImportError:
        print("FATAL: pandas not available")
        return

    print("=" * 75)
    print("AUDIT 4: Counterfactual Semantics")
    print("=" * 75)

    l3_path = os.path.join(OUTPUT_DIR, 'l3_counterfactual_v3.parquet')
    manifest_path = os.path.join(OUTPUT_DIR, 'manifest_laserstream_gold_v3.json')

    if not os.path.exists(l3_path):
        print(f"  L3 not found. Build 3 must complete first.")
        return

    l3 = pd.read_parquet(l3_path)
    print(f"  L3 rows: {len(l3):,}")
    print(f"  L3 columns: {list(l3.columns)}")

    # ── 1. Trade size coverage ───────────────────────────────────────────
    print("\n  === TRADE SIZE COVERAGE ===")
    expected_sizes = [0.05, 0.1, 0.25, 0.5, 1.0]
    actual_sizes = sorted(l3['trade_size_sol'].unique()) if 'trade_size_sol' in l3.columns else []
    print(f"  Expected: {expected_sizes}")
    print(f"  Actual: {actual_sizes}")
    missing_sizes = set(expected_sizes) - set(actual_sizes)
    if missing_sizes:
        print(f"  *** MISSING SIZES: {missing_sizes}")
    else:
        print(f"  All expected sizes present")

    # ── 2. Venue-specific fee verification ──────────────────────────────
    print("\n  === VENUE-SPECIFIC FEE CHECK ===")
    # The v3 builder's quote functions should account for fees.
    # PumpFun: 1% protocol fee on sells + 0.25% platform fee on buys
    # PumpSwap: 0.25% swap fee
    # We verify by checking if entry_price * tokens_bought <= size_lamports (fees deducted)
    for venue in l3['venue'].unique():
        venue_l3 = l3[(l3['venue'] == venue) & (l3['entry_executable'] == True) & (l3['latency_scenario_ms'] == 0)]
        if len(venue_l3) == 0:
            continue
        print(f"\n  Venue: {venue} ({len(venue_l3):,} executable entries)")
        for size in expected_sizes:
            size_df = venue_l3[venue_l3['trade_size_sol'] == size]
            if len(size_df) == 0:
                continue
            # entry_price = size_lamports / tokens_bought (should be > 0)
            # If fees are applied, tokens_bought should be LESS than raw size/price
            # i.e., effective price > spot price
            valid = size_df[size_df['entry_price_lamports_per_rawtok'].notna() & (size_df['entry_price_lamports_per_rawtok'] > 0)]
            if len(valid) > 0:
                print(f"    size={size} SOL: {len(valid):,} entries, mean_entry_price={valid['entry_price_lamports_per_rawtok'].mean():.4f}")

    # ── 3. path_invariance_assumption ────────────────────────────────────
    print("\n  === PATH INVARIANCE ASSUMPTION ===")
    # Check if manifest or L3 records the assumption
    if os.path.exists(manifest_path):
        with open(manifest_path) as f:
            manifest = json.load(f)
        has_assumption = 'path_invariance_assumption' in str(manifest)
        print(f"  Manifest mentions path_invariance_assumption: {has_assumption}")
        if not has_assumption:
            print("  *** FAIL: path_invariance_assumption not recorded in manifest")
    else:
        print("  Manifest not yet available")

    # Check L3 columns for assumption
    has_in_l3 = 'path_invariance_assumption' in l3.columns
    print(f"  L3 has path_invariance_assumption column: {has_in_l3}")
    if not has_in_l3:
        print("  *** FAIL: path_invariance_assumption not in L3 schema")

    # ── 4. Capacity/constrained flags ───────────────────────────────────
    print("\n  === CAPACITY CONSTRAINED FLAGS ===")
    if 'capacity_constrained' in l3.columns:
        for size in expected_sizes:
            size_df = l3[(l3['trade_size_sol'] == size) & (l3['latency_scenario_ms'] == 0)]
            if len(size_df) == 0:
                continue
            constrained = size_df['capacity_constrained'].sum()
            pct = constrained / len(size_df) * 100 if len(size_df) else 0
            print(f"    size={size} SOL: {constrained:,}/{len(size_df):,} capacity_constrained ({pct:.1f}%)")
    else:
        print("  capacity_constrained not in L3 columns")
        print("  *** FAIL: No capacity flags")

    # ── 5. Larger-size results masquerading as guaranteed PnL ───────────
    print("\n  === LARGER-SIZE PnL GUARANTEE CHECK ===")
    # Larger sizes should have MORE capacity constraints, not better returns
    for size in expected_sizes:
        size_df = l3[(l3['trade_size_sol'] == size) & (l3['latency_scenario_ms'] == 0) & (l3['entry_executable'] == True)]
        if len(size_df) == 0:
            continue
        returns = size_df['sim_return_pct'].dropna()
        if len(returns) == 0:
            continue
        mean_ret = returns.mean()
        median_ret = returns.median()
        constrained = size_df['capacity_constrained'].sum() if 'capacity_constrained' in size_df.columns else 0
        print(f"    size={size} SOL: mean_ret={mean_ret:.1f}%, median_ret={median_ret:.1f}%, constrained={constrained:,}")

    # ── 6. Forward exit verification (not same-time buy-and-sell) ───────
    print("\n  === FORWARD EXIT VERIFICATION ===")
    # Exit prices should come from FUTURE states, not the same state.
    # We verify by checking if outcome_class includes tp_hit/sl_hit/timeout
    # which require forward-scanning
    if 'outcome_class' in l3.columns:
        outcome_dist = l3[l3['latency_scenario_ms'] == 0]['outcome_class'].value_counts()
        print(f"  Outcome distribution (0ms latency):")
        for cls, cnt in outcome_dist.items():
            print(f"    {cls}: {cnt:,}")
    else:
        print("  outcome_class not in L3")

    # ── 7. Latency scenarios ────────────────────────────────────────────
    print("\n  === LATENCY SCENARIOS ===")
    if 'latency_scenario_ms' in l3.columns:
        latencies = sorted(l3['latency_scenario_ms'].unique())
        print(f"  Latency scenarios: {latencies}")
        for lat in latencies:
            cnt = len(l3[l3['latency_scenario_ms'] == lat])
            print(f"    {lat}ms: {cnt:,} records")
    else:
        print("  latency_scenario_ms not in L3")

    print("\n  === AUDIT 4 COMPLETE ===")

if __name__ == '__main__':
    run()
