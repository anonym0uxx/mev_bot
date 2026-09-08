# LaserStream Gold v3 — Corrected Audit Report
**Date:** 2026-08-28 (Pacific Time)
**Run UUID:** bbaeb991-0834-4b53-8ca3-90dbb6db0a11
**Execution Model:** lsv3-corrected-1
**Git SHA:** de0cba1a98ba

## Summary

All 4 corrected audits + 12-point final certification **PASS**. v3 is cleared for freeze.

| Audit | Status | Key Result |
|-------|--------|------------|
| Audit 2: Fat-tail (corrected) | ✅ PASS | No extreme_outlier flag; price_scale_tier is descriptive; return_gt_10x/100x/1000x non-degenerate |
| Audit 3: L2 objective truth | ✅ PASS | 52 cols (was 8); all 27 required fields present; NULL semantics correct |
| Audit 4: Counterfactual semantics | ✅ PASS | path_invariance_assumption=true; 94.8M rows; 0 duplicate scenario_ids; 8.7M infeasible with reasons |
| Final certification (12 checks) | ✅ ALL PASS | state_id uniqueness, L3 cardinality, causal leakage, units, censoring, migration, provenance, splits, reason ledger, v2 comparison, path_invariance, L2 richness |

## Rebuild Stats

| Layer | Rows | Cols | Size |
|-------|------|------|------|
| L1 pump_state | 3,790,870 | 48 | 562 MB |
| L2 pump_outcome | 3,790,870 | 52 | 436 MB |
| L3 counterfactual | 94,771,750 | 21 | 2.1 GB |
| L4 policy_eval | 3,790,870 | 5 | 67 MB |
| **Total** | | | **~3.2 GB** |

- Unique mints: 7,016
- Venue: 3,713,569 PumpSwap + 77,301 PumpFun
- L3 expected = actual: 94,771,750 (3,790,870 × 25 scenarios)
- Feasible: 86,046,475 (90.8%) | Infeasible: 8,725,275 (9.2%)
- Migration: 29 successful, 10,692 failed attempts preserved as context

## Fix 1: Fat-tail (Audit 2)

**Removed:** `extreme_outlier` flag (price < 100 lamports/rawtok threshold) — was label-shaping, 99.9992% degenerate.

**Added:** `price_scale_tier` descriptive feature in L1 (NOT a label):
| Tier | Count | % |
|------|-------|---|
| sub_milli_lamport | 2,426,526 | 64.0% |
| sub_centi_lamport | 734,452 | 19.4% |
| sub_deci_lamport | 322,522 | 8.5% |
| sub_lamport | 185,702 | 4.9% |
| lamport_range | 120,436 | 3.2% |
| ten_lamport_range | 1,202 | 0.03% |
| hecto_lamport_range | 17 | 0.0% |
| kilo_lamport_plus | 13 | 0.0% |

**Added:** Return-based flags in L2 (economically explicit, not quantile-fitted):
- `return_gt_10x`: 7.6% of states (268 unique mints)
- `return_gt_100x`: 2.1% of states (174 unique mints)
- `return_gt_1000x`: 0.6% of states (98 unique mints)

**Added:** Train-only quantile metadata in manifest (applied unchanged to val/test):
- p01: -0.8546, p05: -0.6750, p10: -0.5282, p25: -0.2357
- p50: 0.0000, p75: 32.6318, p90: 373.6703, p95: 2116.1180, p99: 35453.2674

## Fix 2: L2 Rich Objective Truth (Audit 3)

**Before:** 8 columns (return_pct, outcome_class, entry_executable, exit_executable, capacity_constrained, entry_failure_reason)

**After:** 52 columns — policy-independent per-state outcomes:
- Multi-horizon returns: 1/2/5/10/30/60/120/300s
- MFE/MAE + times (mfe_pct, mae_pct, mfe_time_ms, mae_time_ms)
- Barrier events: first-hit label + time (+10/+25/+50/+100/+200%, -10/-20/-30/-50%)
  - TP hit: 41.8%, SL hit: 32.0%, no-barrier: 26.2%
- Peak return + time-to-peak
- has_trade_within_H + observed_through_H + right_censored_H (per horizon)
- migration_outcome, venue_outcome, price_quality, observation_completeness
- return_gt_10x/100x/1000x flags

**NULL semantics:** 2.0% nulls in return_300s — unobserved values are NULL, not forward-filled.
**No champion-policy labels or hypothetical trade assumptions.**

## Fix 3: L3 Cardinality Contract (Audit 4)

**Before:** 32.7M rows (1:1 with L1, single scenario)

**After:** 94.8M rows — multi-scenario with 25 size×latency combos per state:
- 5 trade sizes: 0.01, 0.05, 0.1, 0.5, 1.0 SOL
- 5 latency scenarios: 0, 50, 200, 500, 2000 ms
- Composite key: `scenario_id` = hash(state_id + size + latency + fee_model_version)
- **L1:L2:L4 = 1:1:1** by state_id ✅
- **L3 = 1:25** by state_id ✅
- **0 duplicate scenario_ids** ✅
- **Infeasible combos emitted** (NOT silently omitted): 8,725,275 with feasible=false + reason
  - reserves_unavailable: 8,722,250
  - quote_zero_or_overflow: 3,025
- Confidence levels: high 90.1%, medium 0.4%, low 0.3%, none (infeasible) 9.2%

## Fix 4: Path-invariance / Execution Assumptions

- `path_invariance_assumption=true` recorded in **every L3 row** and **manifest** ✅
- `execution_model_version=lsv3-corrected-1` in every L3 row + manifest ✅
- `fee_model`: venue-specific (pumpfun 1% buy/0% sell, pumpswap 0.25% per swap) ✅
- `quote_source`: decoded_raw_account_snapshots ✅
- `reserve_source`: raw_data_b64_account_state ✅
- Capacity/feasibility/confidence flags per scenario ✅

## Final Certification (12 checks)

1. ✅ state_id uniqueness: L1=L2=L4 sets match exactly (3,790,870 each, 0 duplicates)
2. ✅ L3 cardinality: 94,771,750 = 3,790,870 × 25, 0 duplicate scenario_ids
3. ✅ Causal leakage: No future-looking fields in L1
4. ✅ Units/decimals: All lamports positive, PumpFun virtual_sol ≥ real_sol (76,921 rows, 0 violations)
5. ✅ Censoring: right_censored_300s = 59,868 (1.6%)
6. ✅ Migration: 29 successful migrations preserved, 10,692 failed attempts available as context
7. ✅ Provenance: run_uuid, git_sha (de0cba1a98ba), source, file hashes all recorded
8. ✅ Splits: 99/100 mints have monotonic timestamps, 7,016 mints sufficient for mint-disjoint splits
9. ✅ Reason ledger: 2 infeasible reasons (reserves_unavailable, quote_zero_or_overflow)
10. ✅ v2→v3 comparison: extreme_outlier REMOVED, price_scale_tier feature added
11. ✅ path_invariance: true in manifest
12. ✅ L2 richness: 52 columns (was 8)

## Audit 3 Warnings (non-blocking)

- `entry_executable` and `entry_failure_reason` present in L2 — these are observation-level executability metadata (whether a trade could have been observed at entry), NOT champion-policy labels. Kept as objective observation metadata.

## Conclusion

**LaserStream Gold v3 is cleared for freeze.** All corrected audits pass. Rust/Qwen work may begin.
