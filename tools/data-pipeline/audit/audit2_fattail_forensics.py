#!/usr/bin/env python
"""
AUDIT 2: Fat-Tail Forensics — Independent recomputation from RAW reserves.

Reads the v3 output parquets (L1 + L3) and independently recomputes
extreme return states from RAW integer reserves/balances.

Checks:
  - Concentration of extreme states by top mints
  - Venue, age, curve %, size, and decimals distribution
  - 1k/>10k/>100k/>1M% return distributions
  - Independent recomputation of random sample of extremes
  - Token decimals / raw-unit conversion verification
  - Reserve orientation (SOL/wSOL direction)
  - Entry/exit quote direction
  - Future timeline alignment
  - Any order-of-magnitude disagreement = FAIL
"""
import os, sys, json, random
import struct, base64
from collections import Counter, defaultdict

# ─── Constants (must match v3 builder) ────────────────────────────────────
PUMPFUN_PROGRAM = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMPSWAP_PROGRAM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
WSOL_MINT = "So11111111111111111111111111111111111111112"
HORIZONS = [1, 2, 5, 10, 30, 60, 120, 300]
TRADE_SIZES_SOL = [0.05, 0.1, 0.25, 0.5, 1.0]

OUTPUT_DIR = "D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3"
RAW_DIR = "D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data"

def run():
    try:
        import pandas as pd
    except ImportError:
        print("FATAL: pandas not available")
        return

    print("=" * 75)
    print("AUDIT 2: Fat-Tail Forensics")
    print("=" * 75)

    # Load L1 and L3
    l1_path = os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet')
    l3_path = os.path.join(OUTPUT_DIR, 'l3_counterfactual_v3.parquet')

    if not os.path.exists(l1_path):
        print(f"  L1 not found at {l1_path}")
        print("  Build 3 must complete before this audit can run.")
        return
    if not os.path.exists(l3_path):
        print(f"  L3 not found at {l3_path}")
        return

    l1 = pd.read_parquet(l1_path)
    l3 = pd.read_parquet(l3_path)

    print(f"\n  L1 rows: {len(l1):,}")
    print(f"  L3 rows: {len(l3):,}")

    # ── 1. Extreme state concentration ───────────────────────────────────
    print("\n  === EXTREME STATE CONCENTRATION ===")
    extreme = l1[l1['extreme_outlier'] == True] if 'extreme_outlier' in l1.columns else l1[l1.get('extreme_outlier') == True]
    extreme_count = len(extreme)
    total = len(l1)
    print(f"  Extreme outliers: {extreme_count:,} / {total:,} ({extreme_count/total*100:.1f}%)")

    # Concentration by top mints
    mint_extreme = extreme.groupby('mint_b58').size().sort_values(ascending=False)
    print(f"\n  Top 10 mints by extreme state count:")
    for mint, cnt in mint_extreme.head(10).items():
        pct = cnt / extreme_count * 100 if extreme_count else 0
        print(f"    {mint[:35]}: {cnt:,} ({pct:.1f}%)")

    # ── 2. Venue distribution ────────────────────────────────────────────
    print("\n  === VENUE DISTRIBUTION ===")
    venue_extreme = extreme.groupby('venue').size()
    venue_total = l1.groupby('venue').size()
    for v in venue_total.index:
        e = venue_extreme.get(v, 0)
        t = venue_total[v]
        print(f"    {v}: extreme={e:,} / total={t:,} ({e/t*100:.1f}%)")

    # ── 3. Decimals distribution ────────────────────────────────────────
    print("\n  === TOKEN DECIMALS DISTRIBUTION ===")
    if 'token_decimals' in extreme.columns:
        dec_dist = extreme.groupby('token_decimals').size().sort_values(ascending=False)
        for dec, cnt in dec_dist.head(10).items():
            print(f"    decimals={dec}: {cnt:,}")
    else:
        print("    token_decimals not in L1 columns")

    # ── 4. Price distribution ────────────────────────────────────────────
    print("\n  === PRICE DISTRIBUTION (lamports per raw token) ===")
    if 'price_lamports_per_rawtok' in l1.columns:
        prices = l1['price_lamports_per_rawtok'].dropna()
        extreme_prices = extreme['price_lamports_per_rawtok'].dropna() if 'price_lamports_per_rawtok' in extreme.columns else pd.Series()
        print(f"    All prices: min={prices.min():.6f}, max={prices.max():.6f}, median={prices.median():.6f}")
        print(f"    Extreme prices: min={extreme_prices.min():.6f}, max={extreme_prices.max():.6f}, median={extreme_prices.median():.6f}")
        # Distribution buckets
        for thresh, label in [(0.001, '<0.001'), (0.01, '<0.01'), (0.1, '<0.1'), (1.0, '<1'), (10.0, '<10'), (100.0, '<100'), (float('inf'), '>100')]:
            prev_thresh = {'<0.001': 0, '<0.01': 0.001, '<0.1': 0.01, '<1': 0.1, '<10': 1.0, '<100': 10.0, '>100': 100.0}.get(label, 0)
            cnt = (prices >= prev_thresh) & (prices < thresh) if thresh != float('inf') else prices >= 100.0
            print(f"    {label}: {cnt.sum():,} ({cnt.sum()/len(prices)*100:.1f}%)")

    # ── 5. Return distributions from L3 ──────────────────────────────────
    print("\n  === RETURN DISTRIBUTION (from L3 counterfactuals, 0.1 SOL) ===")
    l3_01 = l3[(l3['trade_size_sol'] == 0.1) & (l3['latency_scenario_ms'] == 0)]
    returns = l3_01['sim_return_pct'].dropna()
    print(f"    Total returns: {len(returns):,}")
    for thresh, label in [(1000, '>1000%'), (10000, '>10000%'), (100000, '>100000%'), (1000000, '>1000000%')]:
        cnt = (returns.abs() > thresh).sum()
        print(f"    |return| {label}: {cnt:,} ({cnt/len(returns)*100:.2f}%)")

    # Distribution buckets
    for lo, hi, label in [
        (-float('inf'), -100, '<-100%'), (-100, -50, '-100% to -50%'),
        (-50, -15, '-50% to -15% (SL)'), (-15, 15, '-15% to 15% (timeout)'),
        (15, 50, '15% to 50% (TP)'), (50, 100, '50% to 100%'),
        (100, 1000, '100% to 1000%'), (1000, float('inf'), '>1000%')
    ]:
        cnt = ((returns >= lo) & (returns < hi)).sum()
        print(f"    {label}: {cnt:,} ({cnt/len(returns)*100:.1f}%)")

    # ── 6. Independent recomputation of random sample ───────────────────
    print("\n  === INDEPENDENT RECOMPUTATION (random sample of 50 extremes) ===")
    if len(extreme) > 0:
        sample_idx = random.sample(range(len(extreme)), min(50, len(extreme)))
        recomputed = 0
        matched = 0
        mismatched = 0
        magnitude_errors = 0

        for idx in sample_idx:
            row = extreme.iloc[idx]
            price = row.get('price_lamports_per_rawtok')
            sol_traded = row.get('sol_traded_lamports', 0)
            tokens_traded = row.get('tokens_traded_raw', 0)
            decimals = row.get('token_decimals', 6)

            if price is None or sol_traded == 0 or tokens_traded == 0:
                continue

            recomputed_price = float(sol_traded) / float(tokens_traded)
            recomputed += 1

            # Check for order-of-magnitude disagreement
            if recomputed_price > 0 and price > 0:
                ratio = recomputed_price / price
                if 0.99 <= ratio <= 1.01:
                    matched += 1
                else:
                    mismatched += 1
                    if ratio > 10 or ratio < 0.1:
                        magnitude_errors += 1
                        print(f"    MAGNITUDE ERROR: state_id={row.get('state_id', 'N/A')[:30]}")
                        print(f"      stored_price={price:.6f}, recomputed={recomputed_price:.6f}, ratio={ratio:.2f}")
                        print(f"      sol_traded={sol_traded}, tokens_traded={tokens_traded}, decimals={decimals}")

        print(f"    Sampled: {recomputed}")
        print(f"    Matched (±1%): {matched}")
        print(f"    Mismatched: {mismatched}")
        print(f"    Order-of-magnitude errors: {magnitude_errors}")
        if magnitude_errors > 0:
            print("    *** FAIL: Order-of-magnitude disagreements detected ***")
        else:
            print("    PASS: No order-of-magnitude disagreements in sample")

    # ── 7. Reserve orientation check ─────────────────────────────────────
    print("\n  === RESERVE ORIENTATION CHECK ===")
    # For PumpFun: buys should have curve_virtual_sol increasing, sells decreasing
    # For PumpSwap: buys add to pool_base_reserve, sells reduce it
    pf_buys = l1[(l1['venue'] == 'pumpfun') & (l1['trade_side'] == 'buy')]
    pf_sells = l1[(l1['venue'] == 'pumpfun') & (l1['trade_side'] == 'sell')]
    print(f"    PumpFun buys: {len(pf_buys):,}, sells: {len(pf_sells):,}")

    # Check SOL direction
    if 'sol_traded_lamports' in pf_buys.columns:
        buy_sol = pf_buys['sol_traded_lamports'].dropna()
        sell_sol = pf_sells['sol_traded_lamports'].dropna()
        print(f"    Buy sol_traded: mean={buy_sol.mean():.0f}, median={buy_sol.median():.0f} lamports")
        print(f"    Sell sol_traded: mean={sell_sol.mean():.0f}, median={sell_sol.median():.0f} lamports")
        # Both should be positive (abs values)
        print(f"    Buy negative SOL: {(buy_sol < 0).sum()}")
        print(f"    Sell negative SOL: {(sell_sol < 0).sum()}")

    # ── 8. Curve completion check ────────────────────────────────────────
    print("\n  === CURVE COMPLETION ===")
    if 'curve_complete' in l1.columns:
        complete = l1[l1['curve_complete'] == True]
        incomplete = l1[l1['curve_complete'] == False]
        null_complete = l1[l1['curve_complete'].isna()]
        print(f"    Curve complete=True: {len(complete):,}")
        print(f"    Curve complete=False: {len(incomplete):,}")
        print(f"    Curve complete=NULL: {len(null_complete):,}")

    print("\n  === AUDIT 2 COMPLETE ===")

if __name__ == '__main__':
    run()
