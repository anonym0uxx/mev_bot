# Slinky original trade census v1

## Scope and ownership

Read-only technical integrity/quality census of **all 18 original Parquet shards / 33,581,765 footer rows** in `D:/mev_bot-artifacts/rust-data/slinky21_data/trades`.
The parent inventory is `D:/mev_bot-artifacts/north_star/aggregation/SLINKY_TRADE_SHARD_INVENTORY.json`.
All executable code, logs, pins, checkpoints, reports, and temporary DuckDB storage are isolated in `D:/mev_bot-artifacts/north_star/aggregation/slinky_trade_census_v1/`.
This document is the only worktree file owned by this task. No commits, network, package installations, source writes, or generated trade records.

## Execution and resume

```bash
python D:/mev_bot-artifacts/north_star/aggregation/slinky_trade_census_v1/test_census.py
python -u D:/mev_bot-artifacts/north_star/aggregation/slinky_trade_census_v1/census.py --run
```

The actual full-population run was launched with terminal process handle `proc_8e295ab34265`; `RUN_HANDLE.json` stores the Python PID, and `STATUS.json` stores progress. `run.log` captures stdout/stderr. Initial source-backed tests: 3 passing; tests use only original rows, not synthetic data. The 65,536-row test is an implementation test, **not** the census population.

Re-running the same command resumes only if the script hash, parent inventory hash, original SHA-256 pins, schema, footer rows, and completed shard checkpoint relationships still match. A Windows byte-range lock rejects concurrent launches. Completed shard checkpoints are exclusively created and never overwritten; a torn/invalid checkpoint fails closed rather than being silently replaced. An interrupted incomplete shard is re-scanned in full. The script itself must not be edited after `PINNED_MANIFEST.json` creation.

## Measured checks

- Full SHA-256 pinning of every original, repeated immediately before/after each complete streaming shard scan. Additional full-population hashes bracket exact uniqueness queries.
- Exactly the inventory file set; per-shard byte size, row-group count, footer row count, parent schema SHA-256, and full Arrow schema including metadata must match.
- PyArrow batches at most 65,536, with explicit selected columns and no parallel Parquet reader. All columns are selected because all-field null counts are measured.
- Vectorized null, NaN, positive/negative infinity, finite negative/zero/positive counts and finite extrema for **every double field**, including SOL/token amounts and virtual reserves. Integer `id` receives null checking only.
- Null/empty strings; `is_buy` true/false/null; exact observed `source` value counts.
- Event timestamp extrema in integer epoch microseconds, original timezone recorded in schema; non-null timestamp decreases and ties in physical per-shard row order, including batch boundaries. Nulls are skipped for ordering comparisons. Cross-shard chronology is not asserted.
- Hard row-count equality against each footer and the complete 33,581,765-row inventory; numeric/categorical partitions also must equal scanned rows.
- Exact non-null distinct raw `mint`, `user_wallet`, `tx_signature` values through installed disk-backed DuckDB, one projected field at a time. No whole-population Python sets, no ID normalization. NULL excluded; empty strings included. Repeated signatures are **not** established duplicate trade events.

## Resource and failure contract

RSS guard: 12% of detected physical RAM; sampled every 0.5 seconds and between Arrow batches. Disk guard: 30,000,000,000 bytes over this entire owned output directory, not just temporary files; keep 2GB free on the volume. DuckDB configured for 2GB memory, two threads, and 26GB maximum spill. Guard maxima are sampled, not a continuous OS allocation cap; guard failure terminates native work and writes `RESOURCE_GUARD_FAILURE.json`. Standard exceptions write immutable `PARTIAL_ERROR_*.json` plus partial `STATUS.json`. A progress file saying running is never proof of completion; a guard failure artifact takes precedence.

`QUALITY_COMPLETE.json` means all quality rows were scanned but does not establish completed uniqueness. Only `REPORT.json` with `status=complete`, 18 completed shards, correct total, exact distinct result rows, matching checkpoints, and final hashes establishes technical census completion. Source semantic certification remains separate.

## Precision, admission, rights

**Binary-float source values are not native-unit integers.** No scaling, rounding, quantization, labels, source filtering, or row deletion is performed. Finite/sign classifications and extrema retain the source representation and do not certify prices, reserve semantics, executability, profitability, or absence of leakage. `unique_events` remains unknown because an event-key semantic contract has not been established. `training_eligible=false`. Source rights need separate evidence; the reported user grant is not established by a technical hash/quality scan.

## Result

**Completed and verified: 18/18 shards, 33,581,765/33,581,765 rows.** Full scan and exact distinct execution exited 0. First execution: `2026-09-10T02:11:34.718785+00:00` through `2026-09-10T02:12:14.299975+00:00`. All original SHA-256 hashes remained unchanged. Schema/footer assertions and every partition assertion passed.

| Measurement | Actual full-population result |
|---|---:|
| Exact distinct mints | 622,870 |
| Exact distinct wallets | 1,006,641 |
| Exact distinct transaction signatures | 32,771,605 |
| Repeated non-null signature value rows beyond first occurrence | 810,160 |
| `is_buy=true` / `is_buy=false` | 17,597,586 / 15,984,179 |
| Source | `pumpdev`: 33,581,765 |
| Null SOL amounts | 2,357,752 |
| Zero SOL amounts | 3,488 |
| Null token amounts | 0 |
| Null virtual SOL reserves | 6,122,166 |
| Null virtual token reserves | 4,086,977 |
| Null price / market-cap values | 2,360,061 each |
| Null curve depletion values | 6,122,166 |
| NaN/infinity or finite negative values across checked doubles | 0 |
| Null timestamp, mint, wallet, signature, `is_buy`, source | 0 |
| Empty mint, wallet, signature, source strings | 0 |
| Shards with decreasing timestamps in physical order | 18 |
| Adjacent non-null timestamp decreases, summed per shard | 26,840,835 |

Timestamp coverage: `2026-06-05T09:12:28.604950+00:00` to `2026-07-14T15:02:54.115887+00:00`. Every shard contains decreases: do not assume ascending physical time order. `curve_pct_depleted` maximum is 110.0; source field names/units and plausible economic ranges are not certified. Full finite extrema and per-shard measurements are in reports/checkpoints. Missing SOL/reserve values remain present and unmodified.

Exact value uniqueness does not identify unique trade events; the 810,160 repeated-signature value rows must not be automatically deleted or labeled duplicates. No positive economic conclusions follow from zero negative/nonfinite values.

First-execution sampled peak RSS: **2,365,059,072 bytes**, below the measured **32,931,389,767-byte** 12%-RAM limit. Sampled output-plus-temporary peak: **2,777,197,833 bytes**, below 30,000,000,000 bytes. No guard failures or partial errors were produced. `REPORT_FIRST_EXECUTION.json` preserves these full-scan resource metrics; the latest `REPORT.json` reflects the actual resume-verification run.

Verification: `verify_census.py` independently reloaded/aggregated all shard checkpoints, re-hashed all originals, checked report/pin relationships, and actually resumed the frozen script. All **21 immutable checkpoints** (18 shard + 3 exact uniqueness) retained identical hashes; all 18 shards were resumed, not rescanned. Three source-backed implementation tests pass. `VERIFICATION.json` records the result, latest report SHA-256 `624d25a415586e2378113fc4bd27f9ec3f3b35e46e0a1b3043b797c5cc908f8f`, and first-execution report SHA-256 `e28e610d2898cc50c57fda87d82e2a1d42d1c0bf55b23694a4a50d89acc2a871`.

Primary evidence: external `REPORT.json`, `REPORT_FIRST_EXECUTION.json`, `VERIFICATION.json`, `PINNED_MANIFEST.json`, `checkpoints/`, `run.log`, and `resume_verification.log`. Semantic admission, rights evidence, exact-native-unit interpretation, and event identity remain unresolved separately.
