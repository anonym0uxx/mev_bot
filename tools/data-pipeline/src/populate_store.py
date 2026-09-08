"""Move large data files from repo data dirs into the artifact store.

Resumable: logs every move to a JSONL ledger; re-running skips already-moved
entries. Same-volume move (repo and store both on D:). Leaves small files
(<= threshold) in place so they remain in git.

Usage:
    python populate_store.py --repo D:/repos/mev_bot --store D:/mev_bot-artifacts \
        --min-size-mb 20 --dry-run
"""
from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

# source dir (relative to repo) -> store subdir
MAP = {
    "tools/data-pipeline/output": "gold",
    "rust/data": "rust-data",
    "tools/stream-capture-rs/grpc-server-only/training-data": "raw",
}

# Skip ephemeral/regenerable dirs entirely.
SKIP_PARTS = {".tmp", "target", "__pycache__"}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--store", required=True)
    ap.add_argument("--min-size-mb", type=int, default=20)
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args(argv)

    repo = Path(args.repo).resolve()
    store = Path(args.store).resolve()
    threshold = args.min_size_mb * 1024 * 1024
    ledger_path = store / ".moves.jsonl"
    moved = set()
    if ledger_path.exists():
        for line in ledger_path.read_text().splitlines():
            if line.strip():
                moved.add(json.loads(line)["src"])

    planned = []
    for srcdir, sub in MAP.items():
        root = repo / srcdir
        if not root.is_dir():
            print(f"skip (missing): {root}", file=sys.stderr)
            continue
        for p in root.rglob("*"):
            if not p.is_file() or any(part in SKIP_PARTS for part in p.parts):
                continue
            if p.stat().st_size <= threshold:
                continue
            rel = p.relative_to(repo).as_posix()
            if rel in moved:
                continue
            planned.append((p, store / sub / p.relative_to(root)))

    planned.sort(key=lambda t: -t[0].stat().st_size)
    total = sum(s for s, _ in ((p.stat().st_size, d) for p, d in planned))
    print(f"planned moves: {len(planned)} files, {total/1e9:.2f} GB",
          file=sys.stderr)

    if args.dry_run:
        for src, dst in planned[:20]:
            print(f"[dry] {src.relative_to(repo)} -> {dst.relative_to(store)}")
        return 0

    done = 0
    with ledger_path.open("a") as ledger:
        for src, dst in planned:
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(src), str(dst))
            ledger.write(json.dumps({"src": src.relative_to(repo).as_posix(),
                                     "dst": dst.relative_to(store).as_posix(),
                                     "size": dst.stat().st_size}) + "\n")
            ledger.flush()
            done += 1
            if done % 20 == 0:
                print(f"moved {done}/{len(planned)}", file=sys.stderr)

    print(json.dumps({"moved": done, "total_bytes": total}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
