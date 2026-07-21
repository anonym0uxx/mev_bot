"""
Memory safety for the long-running supervisor/research processes.

Precedence (non-negotiable ordering when goals conflict):
  1. DURABILITY   — never lose reconciled evidence/trade data. Flush/checkpoint before shedding.
  2. SAFETY       — never exceed a memory budget. Shed or stream, never OOM-crash.
  3. OPTIMIZATION — compact and stream within (1) and (2); optimization never trades away data.

This module provides:
  * sha256_stream       — hash large files in fixed-size chunks (no whole-file read into RAM).
  * BoundedList         — a list that caps its length, evicting oldest (for in-flight buffers only,
                          NEVER for durable data — durable data goes to SQLite immediately).
  * memory_report / under_pressure — cross-platform RSS + system-memory awareness (stdlib-only
                          fallback if psutil is absent), so the loop can back off before limits.
  * MemoryGuard         — context manager that samples memory around a unit of work and raises a
                          typed signal to shed load (widen-N down, smaller batches) rather than crash.
"""
from __future__ import annotations

import hashlib
import os
import sys
from collections import deque
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Iterator, Optional


# --------------------------------------------------------------- streaming hash
def sha256_stream(path: str | Path, chunk: int = 1 << 20) -> str:
    """Hash a file in 1 MiB chunks. Constant memory regardless of file size
    (the evaluator binary can be hundreds of MB — never read it whole)."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(chunk), b""):
            h.update(block)
    return h.hexdigest()


# ------------------------------------------------------------------ bounded buf
class BoundedList:
    """
    Fixed-capacity sequence for IN-FLIGHT, non-durable buffers only (e.g. best-of-N candidate
    bodies). Evicts oldest on overflow. NEVER use for data that must persist — durable data is
    written to the evidence store immediately, not held here.
    """
    def __init__(self, cap: int):
        if cap <= 0:
            raise ValueError("cap must be positive")
        self._cap = cap
        self._dq: deque = deque(maxlen=cap)

    def append(self, item: Any) -> None:
        self._dq.append(item)

    def __iter__(self) -> Iterator[Any]:
        return iter(self._dq)

    def __len__(self) -> int:
        return len(self._dq)

    def __bool__(self) -> bool:
        return bool(self._dq)

    def to_list(self) -> list:
        return list(self._dq)


# ------------------------------------------------------------- memory awareness
@dataclass
class MemReport:
    rss_bytes: int
    sys_total_bytes: int
    sys_available_bytes: int

    @property
    def rss_mb(self) -> float:
        return self.rss_bytes / (1024 * 1024)

    @property
    def sys_used_fraction(self) -> float:
        if self.sys_total_bytes <= 0:
            return 0.0
        return 1.0 - (self.sys_available_bytes / self.sys_total_bytes)


def _rss_bytes_stdlib() -> int:
    """Best-effort RSS without psutil."""
    try:
        import resource  # unix
        ru = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        # linux: kilobytes; macos: bytes
        return ru * 1024 if sys.platform != "darwin" else ru
    except Exception:
        pass
    # windows fallback via ctypes
    try:
        import ctypes
        from ctypes import wintypes

        class PMC(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD),
                        ("PageFaultCount", wintypes.DWORD),
                        ("PeakWorkingSetSize", ctypes.c_size_t),
                        ("WorkingSetSize", ctypes.c_size_t),
                        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                        ("PagefileUsage", ctypes.c_size_t),
                        ("PeakPagefileUsage", ctypes.c_size_t)]
        counters = PMC()
        counters.cb = ctypes.sizeof(PMC)
        handle = ctypes.windll.kernel32.GetCurrentProcess()
        if ctypes.windll.psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
            return int(counters.WorkingSetSize)
    except Exception:
        pass
    return 0


def _sys_mem_stdlib() -> tuple[int, int]:
    """(total, available) system memory bytes, best-effort without psutil."""
    # linux
    try:
        meminfo = Path("/proc/meminfo").read_text()
        total = avail = 0
        for line in meminfo.splitlines():
            if line.startswith("MemTotal:"):
                total = int(line.split()[1]) * 1024
            elif line.startswith("MemAvailable:"):
                avail = int(line.split()[1]) * 1024
        if total:
            return total, (avail or total)
    except Exception:
        pass
    # windows
    try:
        import ctypes
        from ctypes import wintypes

        class MSEX(ctypes.Structure):
            _fields_ = [("dwLength", wintypes.DWORD),
                        ("dwMemoryLoad", wintypes.DWORD),
                        ("ullTotalPhys", ctypes.c_ulonglong),
                        ("ullAvailPhys", ctypes.c_ulonglong),
                        ("ullTotalPageFile", ctypes.c_ulonglong),
                        ("ullAvailPageFile", ctypes.c_ulonglong),
                        ("ullTotalVirtual", ctypes.c_ulonglong),
                        ("ullAvailVirtual", ctypes.c_ulonglong),
                        ("ullAvailExtendedVirtual", ctypes.c_ulonglong)]
        stat = MSEX()
        stat.dwLength = ctypes.sizeof(MSEX)
        if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(stat)):
            return int(stat.ullTotalPhys), int(stat.ullAvailPhys)
    except Exception:
        pass
    return 0, 0


def memory_report() -> MemReport:
    """Prefer psutil if available (accurate); else stdlib best-effort. Never raises."""
    try:
        import psutil  # type: ignore
        p = psutil.Process()
        vm = psutil.virtual_memory()
        return MemReport(p.memory_info().rss, vm.total, vm.available)
    except Exception:
        total, avail = _sys_mem_stdlib()
        return MemReport(_rss_bytes_stdlib(), total, avail)


def under_pressure(threshold_fraction: float = 0.85,
                   rss_cap_mb: Optional[float] = None) -> bool:
    """True if system memory use exceeds threshold, or our RSS exceeds an optional hard cap.
    The loop checks this to back off (narrow best-of-N, smaller batches, flush+compact) BEFORE
    hitting a real limit — safety-second, ahead of optimization."""
    r = memory_report()
    if rss_cap_mb is not None and r.rss_mb > rss_cap_mb:
        return True
    if r.sys_total_bytes > 0 and r.sys_used_fraction > threshold_fraction:
        return True
    return False


class MemoryPressure(RuntimeError):
    """Raised to signal the loop should shed load rather than continue and risk OOM."""


@dataclass
class MemoryGuard:
    """
    Wrap a unit of work; if memory is under pressure entering the block, signal shed-load.
    Durable data must already be flushed before entering a guarded shed point (durability-first).
    """
    threshold_fraction: float = 0.85
    rss_cap_mb: Optional[float] = None

    def check(self) -> None:
        if under_pressure(self.threshold_fraction, self.rss_cap_mb):
            r = memory_report()
            raise MemoryPressure(
                f"memory pressure: rss={r.rss_mb:.0f}MB "
                f"sys_used={r.sys_used_fraction:.0%} — shedding load")

    def __enter__(self) -> "MemoryGuard":
        self.check()
        return self

    def __exit__(self, *exc) -> None:
        return None
