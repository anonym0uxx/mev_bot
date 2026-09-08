"""
Shared pipeline utilities: Parquet I/O, hashing, mint-disjoint splits,
deterministic sampling, manifest writing.
"""

from __future__ import annotations
import hashlib, json, os, struct, time
from pathlib import Path
from typing import Optional, List, Dict, Any
from datetime import datetime, timezone

import pyarrow as pa
import pyarrow.parquet as pq

# ─── Parquet I/O (chunked, bounded memory) ────────────────────────────

def write_parquet_partitioned(
    rows: list[dict],
    out_dir: str,
    name: str,
    partition_cols: list[str] | None = None,
    chunk_size: int = 100_000,
) -> list[str]:
    """Write rows to partitioned Parquet files, chunked for bounded memory.
    Returns list of written file paths."""
    os.makedirs(out_dir, exist_ok=True)
    if not rows:
        return []

    # Build arrow table from first chunk to infer schema
    written_files = []
    for i in range(0, len(rows), chunk_size):
        chunk = rows[i:i + chunk_size]
        table = pa.Table.from_pylist(chunk)
        fname = f"{name}_part{i // chunk_size:04d}.parquet"
        fpath = os.path.join(out_dir, fname)
        pq.write_table(table, fpath, compression="zstd")
        written_files.append(fpath)
    return written_files


def read_parquet_files(file_paths: list[str]):
    """Generator that yields rows from parquet files one batch at a time."""
    for fpath in file_paths:
        table = pq.read_table(fpath)
        for batch in table.to_batches():
            for row in batch.to_pylist():
                yield row


def hash_file(path: str) -> str:
    """SHA256 of a file, reading in chunks."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            buf = f.read(65536)
            if not buf:
                break
            h.update(buf)
    return h.hexdigest()


def file_size_bytes(path: str) -> int:
    return os.path.getsize(path)


# ─── Manifest ──────────────────────────────────────────────────────────

def write_manifest(
    manifest_path: str,
    name: str,
    source: str,
    schema_version: str,
    generator_version: str,
    source_files: list[dict],
    output_files: list[dict],
    counts: dict,
    qa: dict,
    known_issues: list[str],
):
    """Write a JSON manifest with SHA256 hashes for all output files."""
    manifest = {
        "name": name,
        "source": source,
        "schema_version": schema_version,
        "generator_version": generator_version,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source_files": source_files,
        "output_files": output_files,
        "counts": counts,
        "qa": qa,
        "known_issues": known_issues,
    }
    os.makedirs(os.path.dirname(manifest_path), exist_ok=True)
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2)
    return manifest_path


# ─── Mint-disjoint chronological splits ────────────────────────────────

def mint_disjoint_split(
    mints_with_time: list[tuple[str, int]],  # (mint, first_seen_unix_ms)
    train_frac: float = 0.7,
    val_frac: float = 0.15,
    test_frac: float = 0.15,
) -> dict[str, list[str]]:
    """Chronological mint-disjoint split: sort mints by first-seen time,
    then assign contiguous blocks to train/val/test so no mint appears
    in multiple splits."""
    assert abs(train_frac + val_frac + test_frac - 1.0) < 1e-6
    sorted_mints = sorted(mints_with_time, key=lambda x: x[1])
    n = len(sorted_mints)
    train_end = int(n * train_frac)
    val_end = int(n * (train_frac + val_frac))

    return {
        "train": [m[0] for m in sorted_mints[:train_end]],
        "val": [m[0] for m in sorted_mints[train_end:val_end]],
        "test": [m[0] for m in sorted_mints[val_end:]],
    }


# ─── Deterministic sampling for validation ────────────────────────────

def deterministic_sample(rows: list, n: int, seed: int = 42) -> list:
    """Deterministic sampling using a fixed seed for reproducible validation."""
    import random
    rng = random.Random(seed)
    indices = list(range(len(rows)))
    rng.shuffle(indices)
    return [rows[i] for i in indices[:n]]
