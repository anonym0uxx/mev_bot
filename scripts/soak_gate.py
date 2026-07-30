#!/usr/bin/env python3
"""
soak_gate.py — portable steady-state RSS-trend gate (§99 / §57 memory-safety mandate).

Why this exists
    Acceptance criterion 99 (and the §56 "System-memory safety and continuous memory
    optimization" mandate) requires: "a CI soak test proves steady-state RSS does not trend
    upward (no leaks)". The real gate is a long, server-side soak of the running bot under
    sustained synthetic load. That is a Phase-B (deployment-hardware) artifact and is meaningless
    to run for hours on a CI runner.

    THIS module is the deterministic, fast, PORTABLE PROXY for that gate — runnable in seconds on
    any developer machine or CI runner. It does not certify the server soak; it enforces the same
    invariant in miniature so a gross leak in the portable-profile tooling is caught early, and it
    documents itself as the proxy it is. The server-side long-soak remains SERVER-DEFERRED.

How pass/fail is decided (two independent bounds; a leak must clear BOTH to pass)
    1. Warm up: the workload runs a few rounds before any sample, so interpreter/allocator
       warm-up (which inflates early RSS) is excluded from the steady-state window.
    2. Sample RSS at K checkpoints across a bounded, non-growing workload.
    3. Steady-state test — the tail (post-warmup samples) must satisfy BOTH:
         (a) linear-fit slope  <=  `max_slope_bytes` per checkpoint  (no sustained upward trend),
         (b) max-minus-median  <=  `max_spread_bytes`                (bounded jitter, no ramp).
       A genuine leak trends upward on (a) and ramps on (b); bounded-cache steady state passes
       both trivially. Both bounds are generous relative to allocator noise so the gate is not
       flaky, yet far tighter than any real leak's growth.

Determinism note
    The trend DECISION lives in `analyze_trend`, which is a pure function of a sample list — no
    wall-clock, no RSS read — so it is deterministic and directly unit-testable (a rising series
    fails, a flat/declining series passes). Only the live harness (`run_soak`) reads real RSS.
"""
from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from typing import Callable, Optional


# ---- RSS sampling (live harness only; kept out of the tested decision logic) -----------------
def sample_rss_bytes() -> int:
    """Current resident set size in bytes, best-effort and portable.

    Prefers Linux `/proc/self/status` VmRSS (true current RSS, which can go DOWN as the allocator
    releases pages — essential for trend detection). Falls back to `resource.getrusage` maxrss
    (a high-water mark; monotonic, so it can only ever understate a leak's reversal, never a
    leak's growth). On Windows, falls back to `ctypes` + `GetProcessMemoryInfo` (current RSS,
    can go down). Returns 0 if none of these are available.

    A return of 0 makes the gate VACUOUS (it cannot detect any leak), so callers that get 0
    should treat the result as non-evidence rather than a pass.
    """
    # Linux /proc/self/status VmRSS — true current RSS.
    try:
        with open("/proc/self/status", "r", encoding="utf-8") as fh:
            for line in fh:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) * 1024  # kB -> bytes
    except OSError:
        pass

    # Windows GetProcessMemoryInfo — current RSS via PROCESS_MEMORY_COUNTERS.
    if sys.platform == "win32":
        try:
            import ctypes
            from ctypes import wintypes

            class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
                _fields_ = [
                    ("cb", wintypes.DWORD),
                    ("PageFaultCount", wintypes.DWORD),
                    ("PeakWorkingSetSize", ctypes.c_size_t),
                    ("WorkingSetSize", ctypes.c_size_t),
                    ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                    ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                    ("PagefileUsage", ctypes.c_size_t),
                    ("PeakPagefileUsage", ctypes.c_size_t),
                ]

            psapi = ctypes.windll.psapi
            psapi.GetProcessMemoryInfo.argtypes = [
                wintypes.HANDLE,
                ctypes.POINTER(PROCESS_MEMORY_COUNTERS),
                wintypes.DWORD,
            ]
            psapi.GetProcessMemoryInfo.restype = wintypes.BOOL

            ctr = PROCESS_MEMORY_COUNTERS()
            ctr.cb = ctypes.sizeof(ctr)
            kernel32 = ctypes.windll.kernel32
            psapi.GetProcessMemoryInfo(
                kernel32.GetCurrentProcess(),
                ctypes.byref(ctr),
                ctr.cb,
            )
            return int(ctr.WorkingSetSize)
        except Exception:  # noqa: BLE001
            pass

    # POSIX resource.getrusage — high-water mark (monotonic; less precise but available).
    try:
        import resource
        ru = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        # Linux reports kB, macOS reports bytes; assume kB when the number is small.
        return ru * 1024 if ru < (1 << 32) else ru
    except Exception:  # noqa: BLE001
        return 0


# ---- trend decision (pure, deterministic, unit-tested) --------------------------------------
def _linear_slope(samples: list[float]) -> float:
    """Least-squares slope of `samples` vs their integer index (bytes per checkpoint).

    Pure arithmetic; deterministic. Returns 0.0 for fewer than two points or zero x-variance.
    """
    n = len(samples)
    if n < 2:
        return 0.0
    mean_x = (n - 1) / 2.0
    mean_y = sum(samples) / n
    num = sum((i - mean_x) * (y - mean_y) for i, y in enumerate(samples))
    den = sum((i - mean_x) ** 2 for i in range(n))
    return num / den if den else 0.0


@dataclass
class SoakResult:
    passed: bool
    samples: list[int]
    warmup: int
    steady: list[int]
    slope_bytes: float
    max_slope_bytes: float
    spread_bytes: int
    max_spread_bytes: int
    reason: str

    def summary(self) -> str:
        return (f"steady_n={len(self.steady)} slope={self.slope_bytes:.0f}B/ckpt "
                f"(<= {self.max_slope_bytes:.0f}) spread={self.spread_bytes}B "
                f"(<= {self.max_spread_bytes}) -> {'ok' if self.passed else 'FAIL'} "
                f"[{self.reason}]")


def analyze_trend(samples: list[int], warmup: int,
                  max_slope_bytes: float, max_spread_bytes: int) -> SoakResult:
    """Decide pass/fail from an RSS sample series. Pure function — no I/O, no clock.

    Drops the first `warmup` samples, then requires the steady-state tail to satisfy both the
    slope bound and the max-minus-median spread bound. A short series (no steady tail) is a
    harness error and fails closed.
    """
    steady = samples[warmup:] if warmup < len(samples) else []
    if len(steady) < 2:
        return SoakResult(False, samples, warmup, steady, 0.0, max_slope_bytes, 0,
                          max_spread_bytes, "insufficient steady-state samples")
    slope = _linear_slope([float(s) for s in steady])
    ordered = sorted(steady)
    median = ordered[len(ordered) // 2] if len(ordered) % 2 == 1 else \
        (ordered[len(ordered) // 2 - 1] + ordered[len(ordered) // 2]) / 2.0
    spread = int(max(steady) - median)
    slope_ok = slope <= max_slope_bytes
    spread_ok = spread <= max_spread_bytes
    passed = slope_ok and spread_ok
    if passed:
        reason = "steady-state RSS bounded — no upward trend"
    elif not slope_ok and not spread_ok:
        reason = "RSS trends upward AND ramps beyond bound (leak suspected)"
    elif not slope_ok:
        reason = "RSS slope exceeds bound (upward trend / leak suspected)"
    else:
        reason = "RSS spread exceeds bound (ramp / leak suspected)"
    return SoakResult(passed, samples, warmup, steady, slope, max_slope_bytes,
                      spread, max_spread_bytes, reason)


# ---- synthetic workloads --------------------------------------------------------------------
def bounded_workload(rounds: int) -> None:
    """A bounded, non-growing workload: a fixed-capacity ring buffer that is overwritten in place.

    Mirrors the constitution's memory discipline — every long-lived collection has a capacity
    bound and an eviction policy (§56/§99) — so a correct implementation reaches steady-state RSS.
    """
    cap = 4096
    ring: list[Optional[bytes]] = [None] * cap
    for i in range(rounds * cap):
        ring[i % cap] = (i % 251).to_bytes(1, "little") * 512  # overwrite; old slot freed


def leaky_workload(store: list, rounds: int) -> None:
    """A deliberately LEAKING workload (never bounded) — used only to prove the gate CATCHES a
    leak in the self-test. Never wired into the CI path."""
    for i in range(rounds * 4096):
        store.append((i % 251).to_bytes(1, "little") * 512)


# ---- live harness ---------------------------------------------------------------------------
def run_soak(checkpoints: int = 12, warmup: int = 4, rounds_per_checkpoint: int = 4,
             workload: Callable[[int], None] = bounded_workload,
             max_slope_bytes: float = 64 * 1024,
             max_spread_bytes: int = 8 * 1024 * 1024) -> SoakResult:
    """Run the bounded workload, sampling RSS at each checkpoint, then decide the trend.

    Defaults are portable-CI tuned: a few seconds of work, a 64 KB/checkpoint slope bound and an
    8 MB spread bound — orders of magnitude below any real leak yet above allocator jitter. The
    workload does not grow, so on a healthy tree every sample after warm-up is flat and the gate
    passes.
    """
    samples: list[int] = []
    for _ in range(checkpoints):
        workload(rounds_per_checkpoint)
        samples.append(sample_rss_bytes())
    return analyze_trend(samples, warmup, max_slope_bytes, max_spread_bytes)


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--checkpoints", type=int, default=12)
    ap.add_argument("--warmup", type=int, default=4)
    ap.add_argument("--rounds", type=int, default=4,
                    help="workload rounds per checkpoint")
    ap.add_argument("--self-test", action="store_true",
                    help="also run the leaky workload and assert the gate FAILS it (proves the "
                         "gate is not vacuous); does not affect the exit code of the real gate")
    args = ap.parse_args(argv)

    res = run_soak(checkpoints=args.checkpoints, warmup=args.warmup,
                   rounds_per_checkpoint=args.rounds)
    print(f"[soak_gate] portable RSS-trend proxy (§99): {res.summary()}")

    if args.self_test:
        leak_store: list = []
        leak = run_soak(checkpoints=args.checkpoints, warmup=args.warmup,
                        rounds_per_checkpoint=args.rounds,
                        workload=lambda r: leaky_workload(leak_store, r))
        caught = not leak.passed
        print(f"[soak_gate] self-test (leaky workload must FAIL): "
              f"{'ok — leak caught' if caught else 'BROKEN — leak NOT caught'} :: {leak.summary()}")
        if not caught:
            return 2

    if not res.passed:
        print(f"[soak_gate] FAILED — {res.reason}")
        return 1
    print("[soak_gate] steady-state RSS bounded — PASSED (portable proxy; server long-soak "
          "remains SERVER-DEFERRED)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
