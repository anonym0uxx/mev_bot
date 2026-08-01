# Task 2 Pre-Registered Prediction (2026-07-31)

## Prediction (written BEFORE patch, per operator directive)

**Hypothesis:** The harness `all_latencies: Vec<u64>` is the dominant RSS contributor,
via Vec capacity-doubling reallocation. Each doubling reallocs and memcpys the entire
buffer (8 MB at 1M elements, 16 MB at 2M). Doublings land at exponentially-spaced
intervals, producing the observed spike pattern: windows 8-16 (first doublings), quiet
mid-run, then spikes again after window 45 (later doublings at larger sizes).

**Prediction:** Draining `all_latencies` per window (keeping window statistics only)
fixes BOTH:
1. The RSS growth (front-loaded-then-flattening curve flattens to warm-up only)
2. The p999 degradation (latency spikes vanish — the clone+sort on the ever-growing
   Vec runs on the same thread as the tick loop, inflating the latency it measures)

If BOTH resolve → the engine is clean; criterion 99's remaining question is only the
unbounded maps (holder_last_ns, meta_prev_totals).
If EITHER persists → it is the engine and the investigation continues.

A prediction recorded after the result is not a test. This one is registered before
the patch.

## Secondary mechanism to verify

The `all_latencies.clone()` + `pct()` sort at each window boundary runs on the SAME
THREAD as the tick loop. Sorting a growing Vec is O(n log n) on an ever-larger buffer.
This inflates the very latency the harness is measuring — a self-inflicted measurement
artifact. If the drain fix removes the sort cost, p999 should drop to match p99.

## Measured quantities (to be filled from instrumented run)

- `all_latencies.len()` at end of run: ______ (measured, not computed)
- `all_latencies.capacity()` at end of run: ______ (measured, not computed)
- Actual tick rate: ______ (the 5000 tps target is paced with sleep; real rate differs)

## Soak contradiction (operator correction #2)

- 180s run: +40.5 MB
- 600s run: 6.9 → 52.6 MB, +45.7 MB
- Extra 420s added only 5.2 MB → FRONT-LOADED THEN FLATTENING (allocator-arena signature)
- Linear at 600s rate → 180s should be ~13.7 MB, not 40.5 MB
- Linear at 180s rate → 600s should be ~135 MB, not 45.7 MB
- Neither is linear. The curve must be reported as a series at 60s intervals.
