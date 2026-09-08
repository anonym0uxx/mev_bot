# 🔒 LASERSTREAM GOLD v3 — FREEZE MARKER

**Frozen:** 2026-08-28 (Pacific Time)
**Status:** IMMUTABLE — no modifications without explicit unfreeze + re-certification
**Run UUID:** bbaeb991-0834-4b53-8ca3-90dbb6db0a11
**Git SHA (post-commit):** 4e97b4b13238
**Git SHA (at build, dirty tree):** de0cba1a98ba
**Execution Model:** lsv3-corrected-1

## Corpus Summary

| Layer | Rows | Cols | Description |
|-------|------|------|-------------|
| L1 | 3,790,870 | 48 | Pump state (price_scale_tier replaces extreme_outlier) |
| L2 | 3,790,870 | 52 | Rich objective truth (returns, MFE/MAE, barriers, migration) |
| L3 | 94,771,750 | 21 | Multi-scenario counterfactuals (25 size×latency combos) |
| L4 | 3,790,870 | 16 | Auxiliary: champion_v1 vs oracle_best_scenario |

## Certification Results

### 12/12 Final Certification Checks: ALL PASS ✅

1. ✅ state_id uniqueness: L1=L2=L4, 0 duplicates
2. ✅ L3 cardinality: 94,771,750 = 3,790,870 × 25, 0 duplicate scenario_ids
3. ✅ Causal leakage: no future-looking fields in L1
4. ✅ Units/decimals: all lamports positive, PumpFun virtual ≥ real (76,921 checked)
5. ✅ Censoring: right_censored_300s = 1.58%
6. ✅ Migration: 29 successful migrations preserved, 10,692 failed-tx documented
7. ✅ Provenance: git_sha, run_uuid, parquet hashes, source code hashes recorded
8. ✅ Splits: 100% monotonic (whole-corpus), mint-disjoint, 0 overlap
9. ✅ Reason ledger: 2 infeasible reasons (reserves_unavailable, quote_zero_or_overflow)
10. ✅ v2→v3 comparison: extreme_outlier REMOVED, price_scale_tier added
11. ✅ path_invariance_assumption: true in manifest + every L3 row
12. ✅ L2 richness: 52 columns (was 8)

### Additional Pre-Freeze Resolutions

**Provenance:** Build ran from dirty/untracked tree at de0cba1. Producing code committed as 4c0b292, fix scripts as 4e97b4b. Manifest records both SHAs + source/audit/parquet hashes. No false attribution.

**Split Semantics:** Whole-corpus analysis (not sampling) confirms 7,016/7,016 mints chronologically monotonic. Mint-disjoint splits by first-observation time: Train 4,912 / Val 1,052 / Test 1,052. Zero mint overlap. Long-lived mint extension documented (1,296 train mints have rows past val boundary — expected, no leakage).

**Mint Reconciliation:** 7,016 mints from successful trades only. v2 had 3,244 (from pre-normalized NDJSON filtered through Rust normalizer). v3 decodes RAW .zst directly, recovering full mint universe. No account snapshots or non-trade accounts counted.

**L4 Semantics:** Separated into champion_v1 (actual config: 0.1 SOL / 0ms / tp15/sl15) and oracle_best_scenario (hindsight best-size from L3). Oracle fields explicitly marked as future-derived. No oracle fields in L1. L4 is auxiliary critique, not policy answer key.

**L2 NULL/Censoring:** 75,325 return_300s NULLs (1.99%) fully categorized:
- 59,868 (79.5%): right_censored (capture ended before 300s)
- 15,457 (20.5%): observed_no_trade_price_unavailable (300s elapsed, no trade)
- 0 uncategorized

## File Hashes (SHA-256, first 16 chars)
- L1: 60718136a3d35779
- L2: 572f056641f2bbee
- L3: 65cb1b3ed6891cfe
- L4: 9f9171a9195acf03

## Bindings
- Rust/Qwen development: UNBLOCKED for rust_gold_v1
- Qwen export/training: BLOCKED until Slinky + LaserStream + Narrative + Rust all certified/frozen
- No rebuilds without explicit unfreeze + re-certification
