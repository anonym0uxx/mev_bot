"""Resumable artifact-store manifest builder.

Walks an artifact root, records per-file SHA256 + size + relative path to
MANIFEST.json. Resumable: skips files already present in an existing manifest
with matching size+mtime. Bounded memory; stream-hashes in 1 MiB chunks.

Usage:
    python artifact_store.py --root D:/mev_bot-artifacts [--workers 4] [--update]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

CHUNK = 1024 * 1024
MANIFEST = "MANIFEST.json"


def hash_file(path: Path) -> tuple[str, int]:
    h = hashlib.sha256()
    size = 0
    with open(path, "rb") as f:
        while True:
            block = f.read(CHUNK)
            if not block:
                break
            h.update(block)
            size += len(block)
    return h.hexdigest(), size


def _worker(args: tuple[str, str]) -> tuple[str, str, int]:
    root, rel = args
    p = Path(root) / rel
    digest, size = hash_file(p)
    return rel, digest, size


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", required=True)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--update", action="store_true",
                    help="reuse existing entries whose size+mtime match")
    ap.add_argument("--ignore", action="append", default=[],
                    help="glob patterns to skip (repeatable)")
    args = ap.parse_args(argv)

    root = Path(args.root).resolve()
    if not root.is_dir():
        print(f"artifact root missing: {root}", file=sys.stderr)
        return 2

    import fnmatch
    man_path = root / MANIFEST
    existing: dict[str, dict] = {}
    if args.update and man_path.exists():
        existing = {e["path"]: e for e in json.loads(man_path.read_text())["files"]}

    ignores = args.ignore

    def skip(rel: str) -> bool:
        return any(fnmatch.fnmatch(rel, pat) for pat in ignores)

    todo: list[str] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in (".git",)]
        for fn in filenames:
            if fn == MANIFEST:
                continue
            p = Path(dirpath) / fn
            rel = p.relative_to(root).as_posix()
            if skip(rel):
                continue
            try:
                st = p.stat()
            except OSError:
                continue
            if (rel in existing and existing[rel].get("size") == st.st_size
                    and existing[rel].get("mtime_ns") == st.st_mtime_ns):
                continue
            todo.append(rel)

    files: dict[str, dict] = {e["path"]: e for e in existing.values()}
    if todo:
        work = [(str(root), rel) for rel in todo]
        done = 0
        if args.workers > 1 and len(work) > 8:
            with ProcessPoolExecutor(max_workers=args.workers) as ex:
                futs = [ex.submit(_worker, w) for w in work]
                for fut in as_completed(futs):
                    rel, digest, size = fut.result()
                    files[rel] = {"path": rel, "sha256": digest, "size": size}
                    done += 1
                    if done % 50 == 0:
                        print(f"  hashed {done}/{len(work)}", file=sys.stderr)
        else:
            for rel in todo:
                digest, size = hash_file(root / rel)
                files[rel] = {"path": rel, "sha256": digest, "size": size}
                done += 1

    # include mtime for resumability on next --update
    for rel, entry in files.items():
        p = root / rel
        try:
            entry["mtime_ns"] = p.stat().st_mtime_ns
        except OSError:
            pass

    total_size = sum(e.get("size", 0) for e in files.values())
    out = {
        "root": str(root),
        "generator": "artifact_store.py",
        "file_count": len(files),
        "total_bytes": total_size,
        "files": [files[k] for k in sorted(files)],
    }
    tmp = man_path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(out, indent=2))
    tmp.replace(man_path)
    print(json.dumps({"root": str(root), "files": len(files),
                      "total_bytes": total_size}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
