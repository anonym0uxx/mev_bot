# Task 4: Churn Positive Control — §99 Caps

**Date**: 2026-07-31  
**Branch**: `task4-churn-positive-control` (scratch)  
**Commit**: (this commit)

## Summary

The §99 caps on `holder_last_ns` and `meta_prev_totals` are proven necessary:
without them, both maps grow without bound. With them, both are bounded at
4,096 entries.

## Bench Churn Harness Limitation

The bench engine-soak `--churn 256` harness (600s) did NOT demonstrate
unbounded growth — RSS plateaued at ~31 MB. Root cause: the churn harness
generates `MarketTrade` events with synthetic `buyer_entity` fields, but
neither uncapped map is exercised by this workload:

- `holder_last_ns` only grows when `record_holder_count` is called, which
  requires `observe_swap_aged` to return `Some(sample)` — i.e. an observable
  holder-count change. The churn harness does not simulate holder counts.
- `meta_prev_totals` only grows when `record_meta_interval` is called with a
  non-empty `mint_category`. The churn harness does not set categories.

The churn harness exercises the engine's event-processing pipeline but does
not reach the §99 maps. A dedicated positive-control test was written to
directly exercise both maps.

## Positive Control Test

**File**: `rust/crates/pump-quant-app/tests/criterion99_positive_control.rs`

Two tests, each inserting 10,000 unique keys into one §99 map:

### `holder_last_ns_bounded_vs_unbounded`

| State | Map size (10,000 keys) | Verdict |
|-------|----------------------|---------|
| Caps PRESENT | 4,096 | BOUNDED — oldest 5,904 evicted |
| Caps REVERTED | 10,000 | UNBOUNDED — all retained |

### `meta_prev_totals_bounded_vs_unbounded`

| State | Map size (10,000 keys) | Verdict |
|-------|----------------------|---------|
| Caps PRESENT | 4,096 | BOUNDED — oldest 5,904 evicted |
| Caps REVERTED | 10,000 | UNBOUNDED — all retained |

## Diagnostic Accessors

Added `holder_last_ns_len()` and `meta_prev_totals_len()` public methods to
`MeasuredState` to inspect the private BTreeMap sizes. These are diagnostic
only — they do not affect production behavior.

## What Was Reverted

On this scratch branch, the eviction logic in `record_holder_count` and
`record_meta_interval` was removed (the cap constants remain defined but
unused, marked `#[allow(dead_code)]`). The constants are kept so the test
can reference `CAP_WHEN_PRESENT = 4096` for the negative-control assertion.

## Criterion 99 Status

**VERIFIED.** The positive control proves the caps are necessary: without
them, both maps grow without bound. The negative control proves the caps
work: with them, both maps are bounded at 4,096.

## Four Numbers

- `common_chat_peg_parse`: 0 (Δ0)
- `stop_processing n_tokens`: post-compaction
- `compression_count`: 47 (source: agent.log cumulative grep
  "compression_attempt"; Δ0 since Task 3 report)
- `max_turns` effective: **150** (config NOT raised to 250)
