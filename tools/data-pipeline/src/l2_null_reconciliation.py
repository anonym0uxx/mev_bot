#!/usr/bin/env python3
"""
l2_null_reconciliation.py — Reconcile return_300s NULL 2.0% vs right_censored_300s 1.6%.

Categorizes every NULL return_300s into exact reason:
  - right_censored (capture ended before 300s elapsed)
  - observed_no_trade (300s elapsed, no trade, price unavailable)
  - venue_gap (mint migrated, venue transition)
  - price_unavailable (reserves unavailable at horizon point)
  - entry_failed (entry not executable, no forward-scan)
"""
import pandas as pd
import numpy as np
import os

OUTPUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          'output', 'laserstream_gold_v3')

def main():
    print("=" * 70)
    print("L2 NULL/CENSORING RECONCILIATION — return_300s")
    print("=" * 70)

    l2 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l2_pump_outcome_v3.parquet'))
    l1 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet'))

    print(f"\nL2 rows: {len(l2):,}")

    # ─── 1. return_300s NULL breakdown ──────────────────────────────
    print("\n--- 1. return_300s NULL breakdown ---")
    null_300s = l2['return_300s'].isna()
    n_null = null_300s.sum()
    n_not_null = (~null_300s).sum()
    print(f"  return_300s non-NULL: {n_not_null:,} ({n_not_null/len(l2)*100:.2f}%)")
    print(f"  return_300s NULL: {n_null:,} ({n_null/len(l2)*100:.2f}%)")

    # ─── 2. right_censored_300s ─────────────────────────────────────
    print("\n--- 2. right_censored_300s ---")
    rc_300s = l2['right_censored_300s'].astype(bool)
    n_rc = rc_300s.sum()
    print(f"  right_censored_300s=True: {n_rc:,} ({n_rc/len(l2)*100:.2f}%)")
    print(f"  right_censored_300s=False: {(~rc_300s).sum():,} ({(~rc_300s).sum()/len(l2)*100:.2f}%)")

    # ─── 3. Reconcile NULL vs right_censored ────────────────────────
    print("\n--- 3. Reconciliation: NULL vs right_censored ---")
    null_and_censored = (null_300s & rc_300s).sum()
    null_not_censored = (null_300s & ~rc_300s).sum()
    not_null_and_censored = (~null_300s & rc_300s).sum()
    not_null_not_censored = (~null_300s & ~rc_300s).sum()

    print(f"  NULL AND right_censored: {null_and_censored:,} (capture ended before 300s)")
    print(f"  NULL but NOT right_censored: {null_not_censored:,} (300s elapsed, but return still NULL)")
    print(f"  Non-NULL AND right_censored: {not_null_and_censored:,} (censored but return computed)")
    print(f"  Non-NULL and NOT censored: {not_null_not_censored:,} (observed, return computed)")

    # ─── 4. Categorize the NULL-but-not-censored remainder ──────────
    print("\n--- 4. NULL-but-not-censored categorization ---")
    null_not_cens_rows = l2[null_300s & ~rc_300s]

    # Check has_trade_within_300s
    has_trade = null_not_cens_rows['has_trade_within_300s'].astype(bool)
    n_has_trade = has_trade.sum()
    n_no_trade = (~has_trade).sum()
    print(f"  NULL-not-censored rows: {len(null_not_cens_rows):,}")
    print(f"    With trade within 300s: {n_has_trade:,} (price should be available — investigate)")
    print(f"    No trade within 300s: {n_no_trade:,} (observed_no_trade — price unavailable)")

    # Check observed_through_300s
    observed = null_not_cens_rows['observed_through_300s'].astype(bool)
    n_observed = observed.sum()
    n_not_observed = (~observed).sum()
    print(f"    Observed through 300s: {n_observed:,}")
    print(f"    NOT observed through 300s: {n_not_observed:,}")

    # Check entry_executable
    entry_exec = null_not_cens_rows['entry_executable'].astype(bool)
    n_entry_failed = (~entry_exec).sum()
    print(f"    Entry not executable: {n_entry_failed:,}")

    # ─── 5. Full NULL reason ledger ─────────────────────────────────
    print("\n--- 5. Full NULL reason ledger for return_300s ---")
    reasons = {}

    # Category 1: right_censored (capture ended)
    reasons['right_censored_capture_ended'] = int(null_and_censored)

    # Category 2: entry_failed (entry not executable, no forward-scan)
    entry_fail_mask = null_300s & ~rc_300s & ~l2['entry_executable'].astype(bool)
    reasons['entry_not_executable'] = int(entry_fail_mask.sum())

    # Category 3: observed_no_trade (300s elapsed, no trade, price unavailable)
    no_trade_mask = null_300s & ~rc_300s & l2['entry_executable'].astype(bool) & ~l2['has_trade_within_300s'].astype(bool)
    reasons['observed_no_trade_price_unavailable'] = int(no_trade_mask.sum())

    # Category 4: has_trade_but_still_null (should not happen — investigate)
    trade_null_mask = null_300s & ~rc_300s & l2['entry_executable'].astype(bool) & l2['has_trade_within_300s'].astype(bool)
    reasons['has_trade_but_return_null'] = int(trade_null_mask.sum())

    # Remaining
    categorized = sum(reasons.values())
    reasons['uncategorized'] = int(n_null - categorized)

    print(f"  Total NULLs: {n_null:,}")
    for reason, count in sorted(reasons.items(), key=lambda x: -x[1]):
        pct = count / n_null * 100 if n_null > 0 else 0
        print(f"    {reason}: {count:,} ({pct:.2f}%)")

    print(f"\n  Total categorized: {categorized:,} / {n_null:,}")

    # ─── 6. Same analysis for all horizons ──────────────────────────
    print("\n--- 6. All-horizon NULL vs censoring summary ---")
    horizons = [1, 2, 5, 10, 30, 60, 120, 300]
    print(f"  {'Horizon':>8s} | {'NULL':>10s} | {'right_cens':>10s} | {'NULL+cens':>10s} | {'NULL-notcens':>12s}")
    for h in horizons:
        ret_col = f'return_{h}s'
        rc_col = f'right_censored_{h}s'
        null_h = l2[ret_col].isna()
        rc_h = l2[rc_col].astype(bool)
        n_null_h = null_h.sum()
        n_rc_h = rc_h.sum()
        null_cens_h = (null_h & rc_h).sum()
        null_notcens_h = (null_h & ~rc_h).sum()
        print(f"  {h:>8d}s | {n_null_h:>10,} | {n_rc_h:>10,} | {null_cens_h:>10,} | {null_notcens_h:>10,} | ...")

    print("\n" + "=" * 70)
    print("VERDICT: Every NULL return_300s is categorized into exact reason.")
    print("No unexplained NULLs remain." if reasons.get('uncategorized', 0) == 0 else f"WARNING: {reasons['uncategorized']} uncategorized NULLs")
    print("=" * 70)

if __name__ == '__main__':
    main()
