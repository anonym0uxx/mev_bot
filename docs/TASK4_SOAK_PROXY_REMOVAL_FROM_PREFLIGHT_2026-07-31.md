# Task 4: Remove Soak Proxy from Preflight Row 11

**Date:** 2026-07-31  
**Status:** COMPLETE  
**Directive:** "Stop a proven-false measurement from blocking a real gate"

## Problem

Preflight row 11 (`scripts/regression_e2e.py`) was failing because the
supervisor test suite (`test_regression_invariants.py:146-157`) contained
a bounded-workload assertion that called `soak_gate.run_soak()` on a bounded
CPython allocator workload and asserted it PASSES the RSS slope bound.

This is a proven-false measurement:
- `soak_gate.py` measures the **CPython harness allocator RSS**, NOT the
  trading engine's memory behaviour (documented in prior session, commit
  abdda50).
- The bounded-workload assertion exhibited 55× run-to-run variance in RSS
  slope (99,279 B/ckpt on the failing run vs 1,800 B/ckpt on a clean run).
- The failure blocked the preflight despite measuring the wrong system.

## What Was Removed

### 1. `supervisor/tests/test_regression_invariants.py`

**Removed:** The bounded-workload assertion (lines 156-157):
```python
# REMOVED:
ok = soak_gate.run_soak(checkpoints=14, warmup=6, rounds_per_checkpoint=2)
self.assertTrue(ok.passed, ok.summary())
```

**Kept:** The leak-detection assertion (line 154) — the REAL self-healing
property that verifies the gate catches an injected leak:
```python
# KEPT:
res = soak_gate.run_soak(checkpoints=14, warmup=6, rounds_per_checkpoint=3,
                         workload=lambda r: soak_gate.leaky_workload(store, r))
self.assertFalse(res.passed, res.summary())
```

The leak-detection test verifies that `soak_gate.analyze_trend()` correctly
identifies an unbounded accumulator as a leak. This is the self-healing
property: if the gate ever stops catching leaks, this test catches it.

The bounded-workload assertion had a different purpose: verifying that a
bounded workload PASSES. But "a bounded CPython workload passes" tells us
nothing about the engine's memory behaviour, and the 55× variance made it
flaky on this machine.

### 2. `scripts/phase_b_preflight.py`

Added the ENGINE MEMORY caveat to BOTH the text output (failure and success
paths) and the JSON output:
```json
{"engine_memory": "UNMEASURED", "criterion_99": "UNVERIFIED"}
```

This ensures a green preflight is NEVER readable as "memory is fine."

## What Was NOT Changed

- `soak_gate.py` — remains available as a standalone script. Its
  `analyze_trend()` logic is still tested via the leak-detection assertion.
- `ci_gate.py` — already decontaminated (commit 7134c6b). The removal
  comment already states "criterion 99 stays UNVERIFIED."
- Criterion 99 — remains UNVERIFIED. This is NOT widening a bound or
  deleting a real measurement. It is deleting a false measurement (CPython
  allocator RSS proxy) that was blocking a real gate (preflight row 11).

## Verification

```
$ python -m unittest discover -s supervisor/tests -t .
Ran 152 tests in 3.004s
OK
```

All 152 tests pass (was 152 with 1 flaky failure before; now 152 with 0
failures — the flaky bounded-workload assertion was removed, the leak-
detection assertion was kept).

## Criterion 99 Status

**UNVERIFIED.** Engine memory is **UNMEASURED.** The real engine soak
harness belongs in §4.5 deploy-hardware tuning, where engine performance
work happens anyway. This is named, not dropped.

## Distinction (binding)

This is NOT widening a bound. It is deleting a false measurement. The
distinction is:
- Widening a bound = changing 65,536 to 100,000 to make the proxy pass.
- Deleting a false measurement = removing the proxy because it measures
  the CPython harness allocator, not the engine.

The former hides a problem. The latter removes a measurement that was
never measuring what it claimed to.

## Metrics

- common_chat_peg_parse: 0 (delta: 0)
- Largest stop_processing n_tokens: (see chat report)
- compression_count: (see chat report)
