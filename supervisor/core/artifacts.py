"""
Production-artifact auto-discovery.

The build produces the evaluator, research runner, and status export at canonical paths
(mandated in the constitution's §62 MCP block). The supervisor discovers them itself —
the operator never fills paths by hand. Discovery order per artifact:

  1. canonical path(s) inside the repo
  2. glob search under target/release (newest match wins)

Evaluator hash pinning is trust-on-first-use at release: the first discovered evaluator's
sha256 is pinned automatically and persisted; ANY later mismatch is Tier-0 and is never
silently re-pinned.
"""
from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Optional

_EXE = (".exe", "")  # windows-first, portable


def _first_existing(cands: list[Path]) -> Optional[Path]:
    for c in cands:
        if c.is_file():
            return c
    return None


def _newest_glob(root: Path, patterns: list[str]) -> Optional[Path]:
    hits: list[Path] = []
    for pat in patterns:
        hits.extend(p for p in root.glob(pat) if p.is_file())
    if not hits:
        return None
    return max(hits, key=lambda p: p.stat().st_mtime)


def discover_evaluator(repo: str | Path) -> Optional[Path]:
    r = Path(repo)
    canonical = [r / "target" / "release" / f"pq-evaluator{e}" for e in _EXE]
    found = _first_existing(canonical)
    if found:
        return found
    rel = r / "target" / "release"
    return _newest_glob(rel, ["pq*evaluator*"]) if rel.is_dir() else None


def discover_research_runner(repo: str | Path) -> Optional[Path]:
    r = Path(repo)
    canonical = [r / "target" / "release" / f"pq-research-runner{e}" for e in _EXE]
    found = _first_existing(canonical)
    if found:
        return found
    rel = r / "target" / "release"
    return _newest_glob(rel, ["pq*research*", "pq*runner*"]) if rel.is_dir() else None


def discover_status_file(repo: str | Path) -> Optional[Path]:
    r = Path(repo)
    canonical = [r / "data" / "live_status.json", r / "live_status.json",
                 r / "data" / "status.json"]
    return _first_existing(canonical)


def sha256_of(path: str | Path) -> str:
    # Stream in chunks — never read a large binary (evaluator can be hundreds of MB) into RAM.
    from .memory import sha256_stream
    return sha256_stream(path)
