#!/usr/bin/env python
"""G0 — artifact inventory with hashes for the V2 build (closes the lineage gap)."""
import hashlib, json, os, sys

ROOT = "/training/v2"
SKIP_DIR = {".git", "__pycache__"}
TARGETS = ["contracts", "code/src/v2", "canonical", "candidate", "reports"]


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""):
            h.update(c)
    return h.hexdigest()


def main():
    inv = {"schema": "north_star_artifact_inventory_v2", "root": ROOT, "artifacts": []}
    for t in TARGETS:
        base = os.path.join(ROOT, t)
        if not os.path.isdir(base):
            continue
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = [d for d in dirnames if d not in SKIP_DIR]
            for fn in sorted(filenames):
                if fn.endswith((".pyc",)):
                    continue
                fp = os.path.join(dirpath, fn)
                try:
                    sz = os.path.getsize(fp)
                    h = sha256_file(fp) if sz < (512 << 20) else "SKIPPED_TOO_LARGE"
                except OSError:
                    continue
                inv["artifacts"].append({
                    "path": os.path.relpath(fp, ROOT), "bytes": sz, "sha256": h})
    inv["count"] = len(inv["artifacts"])
    os.makedirs(f"{ROOT}/lineage", exist_ok=True)
    json.dump(inv, open(f"{ROOT}/lineage/ARTIFACT_INVENTORY.json", "w"), indent=1)
    print(f"[G0] artifacts={inv['count']}")
    big = sorted(inv["artifacts"], key=lambda a: -a["bytes"])[:8]
    for a in big:
        print(f"    {a['bytes']:>12,}  {a['path']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
