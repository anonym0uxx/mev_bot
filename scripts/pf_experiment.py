#!/usr/bin/env python3
"""
instrument_eval.py — Evaluate Python-allocator-visible instruments
for the soak gate, replacing PageFaultCount (blind to pymalloc).

Instruments tested:
  1. sys.getallocatedblocks()  — block count held by Python allocator (incl. pymalloc)
  2. tracemalloc               — byte-level allocated bytes, per-line attribution

Workloads (same as soak_gate.py):
  - bounded: fixed-capacity ring buffer, overwritten in place
  - growing: unbounded list append (leak)
  - mmap:    mmap-based constant-rate leak (bypasses pymalloc entirely)

Each trial runs in a SUBPROCESS (fresh process) to simulate CI.
"""
from __future__ import annotations
import argparse
import ctypes
import json
import mmap
import sys
import tracemalloc
from typing import Optional

# ---- instrument readers ----

def read_allocated_blocks() -> int:
    """sys.getallocatedblocks(): count of memory blocks currently held by
    the Python allocator, including pymalloc pools. Deterministic, zero OS noise."""
    return sys.getallocatedblocks()

def read_tracemalloc_bytes() -> int:
    """tracemalloc current allocated bytes. Byte-level precision."""
    import tracemalloc
    stats = tracemalloc.take_snapshot()
    total = sum(stat.size for stat in stats.statistics("filename"))
    return total

def read_tracemalloc_blocks() -> int:
    """tracemalloc current allocated block count."""
    import tracemalloc
    stats = tracemalloc.take_snapshot()
    total = sum(stat.count for stat in stats.statistics("filename"))
    return total

# ---- workloads (identical to soak_gate.py) ----

def bounded_workload(rounds: int) -> None:
    cap = 4096
    ring: list[Optional[bytes]] = [None] * cap
    for i in range(rounds * cap):
        ring[i % cap] = (i % 251).to_bytes(1, "little") * 512

def growing_workload(store: list, rounds: int, rate: int = 4096) -> None:
    for i in range(rate * rounds):
        store.append((i % 251).to_bytes(1, "little") * 512)

def mmap_leaky_workload(store: list, rounds: int, rate: int = 64) -> None:
    """mmap-based leak: each checkpoint maps `rate` pages and never unmaps.
    Bypasses pymalloc entirely — forces system allocator + page faults."""
    for i in range(rate * rounds):
        m = mmap.mmap(-1, 4096)
        m.write(b'\x00' * 4096)
        store.append(m)  # keep reference alive (leak)

# ---- trend analysis (same linear slope as soak_gate.py) ----

def linear_slope(samples: list[float]) -> float:
    n = len(samples)
    if n < 2:
        return 0.0
    mean_x = (n - 1) / 2.0
    mean_y = sum(samples) / n
    num = sum((i - mean_x) * (y - mean_y) for i, y in enumerate(samples))
    den = sum((i - mean_x) ** 2 for i in range(n))
    return num / den if den else 0.0

# ---- trial runner ----

def run_trial(workload_name: str, instrument: str, rate: int = 0,
              checkpoints: int = 14, warmup: int = 6, rounds: int = 4) -> dict:
    store: list = []

    if instrument == "blocks":
        reader = read_allocated_blocks
    elif instrument == "tmalloc_bytes":
        tracemalloc.start()
        reader = read_tracemalloc_bytes
    elif instrument == "tmalloc_count":
        tracemalloc.start()
        reader = read_tracemalloc_blocks
    else:
        raise ValueError(f"unknown instrument: {instrument}")

    import gc
    gc.collect()

    samples: list[int] = []
    for i in range(checkpoints):
        if workload_name == "bounded":
            bounded_workload(rounds)
        elif workload_name == "growing":
            growing_workload(store, rounds, rate)
        elif workload_name == "mmap":
            mmap_leaky_workload(store, rounds, rate)
        gc.collect()
        samples.append(reader())

    if instrument in ("tmalloc_bytes", "tmalloc_count"):
        tracemalloc.stop()

    steady = samples[warmup:]
    slope = linear_slope([float(s) for s in steady])
    return {
        "instrument": instrument,
        "workload": workload_name,
        "rate": rate,
        "samples": samples,
        "steady": steady,
        "slope": slope,
        "steady_min": min(steady) if steady else 0,
        "steady_max": max(steady) if steady else 0,
    }

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workload", choices=["bounded", "growing", "mmap"], default="bounded")
    ap.add_argument("--instrument", choices=["blocks", "tmalloc_bytes", "tmalloc_count"], default="blocks")
    ap.add_argument("--rate", type=int, default=0)
    ap.add_argument("--checkpoints", type=int, default=14)
    ap.add_argument("--warmup", type=int, default=6)
    ap.add_argument("--rounds", type=int, default=4)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    result = run_trial(args.workload, args.instrument, args.rate,
                       args.checkpoints, args.warmup, args.rounds)
    if args.json:
        print(json.dumps(result))
    else:
        print(f"instrument={result['instrument']} workload={result['workload']} rate={result['rate']}")
        print(f"  samples: {result['samples']}")
        print(f"  steady:  {result['steady']}")
        print(f"  slope:   {result['slope']:.2f}")

if __name__ == "__main__":
    raise SystemExit(main())
