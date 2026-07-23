"""Unit tests for the §99 portable soak / RSS-trend gate decision logic.

The trend DECISION (`analyze_trend`) is a pure function of a sample list — deterministic, no
wall-clock, no RSS read — so it is tested directly with synthetic series. A live end-to-end run
of the bounded workload is also exercised to confirm it passes on a healthy tree.

Stdlib unittest; run with `python3 -m unittest`.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

_SCRIPTS = Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import soak_gate  # noqa: E402

MB = 1024 * 1024
KB = 1024


class TrendDecisionTests(unittest.TestCase):
    def test_flat_series_passes(self) -> None:
        samples = [100 * MB] * 12
        res = soak_gate.analyze_trend(samples, warmup=4, max_slope_bytes=64 * KB,
                                      max_spread_bytes=8 * MB)
        self.assertTrue(res.passed, res.summary())
        self.assertEqual(res.slope_bytes, 0.0)

    def test_declining_series_passes(self) -> None:
        # RSS falling (allocator releasing pages) is healthy.
        samples = [100 * MB - i * MB for i in range(12)]
        res = soak_gate.analyze_trend(samples, warmup=4, max_slope_bytes=64 * KB,
                                      max_spread_bytes=8 * MB)
        self.assertTrue(res.passed, res.summary())
        self.assertLess(res.slope_bytes, 0.0)

    def test_upward_trend_fails_on_slope(self) -> None:
        # steady 2 MB/checkpoint growth after warmup -> slope bound blown, leak caught
        samples = [100 * MB + i * 2 * MB for i in range(12)]
        res = soak_gate.analyze_trend(samples, warmup=4, max_slope_bytes=64 * KB,
                                      max_spread_bytes=8 * MB)
        self.assertFalse(res.passed, res.summary())
        self.assertGreater(res.slope_bytes, 64 * KB)

    def test_warmup_prefix_excluded(self) -> None:
        # a big warm-up spike then flat steady state -> passes (spike is pre-warmup)
        samples = [10 * MB, 40 * MB, 80 * MB, 95 * MB] + [100 * MB] * 8
        res = soak_gate.analyze_trend(samples, warmup=4, max_slope_bytes=64 * KB,
                                      max_spread_bytes=8 * MB)
        self.assertTrue(res.passed, res.summary())

    def test_ramp_within_slope_but_over_spread_fails(self) -> None:
        # small per-step slope but a large late ramp so max-minus-median exceeds the spread bound
        steady = [100 * MB] * 6 + [130 * MB] * 2
        samples = [0, 0, 0, 0] + steady  # warmup=4 drops the zeros
        res = soak_gate.analyze_trend(samples, warmup=4, max_slope_bytes=100 * MB,
                                      max_spread_bytes=8 * MB)
        self.assertFalse(res.passed, res.summary())
        self.assertGreater(res.spread_bytes, 8 * MB)

    def test_insufficient_samples_fails_closed(self) -> None:
        res = soak_gate.analyze_trend([100 * MB], warmup=4, max_slope_bytes=64 * KB,
                                      max_spread_bytes=8 * MB)
        self.assertFalse(res.passed)
        self.assertIn("insufficient", res.reason)


class SlopeMathTests(unittest.TestCase):
    def test_linear_slope_exact(self) -> None:
        self.assertAlmostEqual(soak_gate._linear_slope([0, 2, 4, 6]), 2.0)
        self.assertAlmostEqual(soak_gate._linear_slope([5, 5, 5]), 0.0)
        self.assertAlmostEqual(soak_gate._linear_slope([9]), 0.0)


class LiveHarnessTests(unittest.TestCase):
    def test_bounded_workload_passes(self) -> None:
        # small, fast run; the bounded ring buffer must reach steady state on any machine
        res = soak_gate.run_soak(checkpoints=10, warmup=3, rounds_per_checkpoint=2)
        self.assertTrue(res.passed, res.summary())

    def test_leaky_workload_is_caught(self) -> None:
        # proves the gate is not vacuous: an unbounded accumulator must FAIL
        store: list = []
        res = soak_gate.run_soak(checkpoints=10, warmup=3, rounds_per_checkpoint=3,
                                 workload=lambda r: soak_gate.leaky_workload(store, r))
        self.assertFalse(res.passed, res.summary())


if __name__ == "__main__":
    unittest.main()
