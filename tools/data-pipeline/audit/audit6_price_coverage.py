#!/usr/bin/env python
"""
AUDIT 6: Price/Reserve Coverage — Authoritative reserve population and
exact-price recovery by venue.

Reports:
  1. Total states per venue
  2. States with valid price (price_lamports_per_rawtok > 0)
  3. States with null/zero price and the reason
  4. Exact-price recovery rate by venue
  5. Reserve population: how many states have full reserve data
  6. Explanation of every unrecoverable state
  7. Never force a low-confidence value to reach 100%
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
    print("AUDIT 6: Price/Reserve Coverage")
    print("=" * 75)

    l1_path = os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet')
    if not os.path.exists(l1_path):
        print(f"  L1 not found. Build 3 must complete first.")
        return

    l1 = pd.read_parquet(l1_path)
    print(f"  L1 rows: {len(l1):,}")
    print(f"  L1 columns: {list(l1.columns)}")

    # ── 1. Price coverage by venue ───────────────────────────────────────
    print("\n  === PRICE COVERAGE BY VENUE ===")
    for venue in sorted(l1['venue'].unique()):
        venue_df = l1[l1['venue'] == venue]
        total = len(venue_df)
        has_price = venue_df['price_lamports_per_rawtok'].notna() & (venue_df['price_lamports_per_rawtok'] > 0) if 'price_lamports_per_rawtok' in venue_df.columns else 0
        no_price = total - has_price.sum()
        pct = has_price.sum() / total * 100 if total else 0
        print(f"\n  Venue: {venue}")
        print(f"    Total states: {total:,}")
        print(f"    With valid price: {has_price.sum():,} ({pct:.1f}%)")
        print(f"    No price (null/zero): {no_price:,} ({100-pct:.1f}%)")

        # Reason for no price
        if no_price > 0 and 'price_confidence' in venue_df.columns:
            no_price_df = venue_df[~(venue_df['price_lamports_per_rawtok'].notna() & (venue_df['price_lamports_per_rawtok'] > 0))]
            conf_dist = no_price_df['price_confidence'].value_counts()
            print(f"    Price confidence distribution for no-price states:")
            for conf, cnt in conf_dist.items():
                print(f"      {conf}: {cnt:,}")

    # ── 2. Reserve population ────────────────────────────────────────────
    print("\n  === RESERVE POPULATION ===")
    reserve_cols = [c for c in l1.columns if 'reserve' in c.lower()]
    print(f"  Reserve columns: {reserve_cols}")
    for col in reserve_cols:
        nonnull = l1[col].notna().sum()
        print(f"    {col}: {nonnull:,} non-null ({nonnull/len(l1)*100:.1f}%)")

    # ── 3. Exact-price recovery by venue ─────────────────────────────────
    print("\n  === EXACT-PRICE RECOVERY BY VENUE ===")
    # Exact price recovery = price derived from trade data (sol_traded / tokens_traded)
    if 'sol_traded_lamports' in l1.columns and 'tokens_traded_raw' in l1.columns:
        for venue in sorted(l1['venue'].unique()):
            venue_df = l1[l1['venue'] == venue]
            has_trade_data = (venue_df['sol_traded_lamports'] > 0) & (venue_df['tokens_traded_raw'] > 0)
            has_derived_price = has_trade_data & venue_df['price_lamports_per_rawtok'].notna() & (venue_df['price_lamports_per_rawtok'] > 0)
            total = len(venue_df)
            print(f"  {venue}: {has_derived_price.sum():,}/{total:,} ({has_derived_price.sum()/total*100:.1f}%) have exact price from trade data")
    else:
        print("  sol_traded_lamports or tokens_traded_raw not in L1 columns")

    # ── 4. Unrecoverable states ──────────────────────────────────────────
    print("\n  === UNRECOVERABLE STATES ===")
    if 'price_lamports_per_rawtok' in l1.columns:
        no_price = l1[~(l1['price_lamports_per_rawtok'].notna() & (l1['price_lamports_per_rawtok'] > 0))]
        print(f"  Total unrecoverable: {len(no_price):,} / {len(l1):,} ({len(no_price)/len(l1)*100:.1f}%)")
        # Break down by venue
        for venue in no_price['venue'].value_counts().index:
            venue_np = no_price[no_price['venue'] == venue]
            print(f"    {venue}: {len(venue_np):,}")

        # Check if any have forced low-confidence values
        if 'price_confidence' in no_price.columns:
            conf_dist = no_price['price_confidence'].value_counts()
            print(f"  Price confidence for unrecoverable:")
            for conf, cnt in conf_dist.items():
                print(f"    {conf}: {cnt:,}")
        if 'entry_failure_reason' in no_price.columns:
            reason_dist = no_price['entry_failure_reason'].value_counts()
            print(f"  Entry failure reasons:")
            for reason, cnt in reason_dist.head(10).items():
                print(f"    {reason}: {cnt:,}")

    print("\n  === AUDIT 6 COMPLETE ===")

if __name__ == '__main__':
    run()
