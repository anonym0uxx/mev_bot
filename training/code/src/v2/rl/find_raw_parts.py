#!/usr/bin/env python
"""Locate which raw capture parts cover a unix-ms window (cheap: first/last ts)."""
from __future__ import annotations

import glob
import json
import subprocess
import sys

PAT = "/mnt/data/mev_bot-artifacts/raw/pumpfun_laserstream_raw_*_part*.ndjson.zst"


def bounds(path, max_lines=4000):
    lo = hi = None
    p = subprocess.Popen(["zstd", "-dc", path], stdout=subprocess.PIPE)
    for i, line in enumerate(p.stdout):
        if i > max_lines:
            break
        try:
            o = json.loads(line)
        except Exception:
            continue
        t = o.get("recv_unix_ms")
        if not t:
            continue
        if lo is None:
            lo = t
        hi = t
    p.kill()
    return lo, hi


def main():
    want_lo, want_hi = int(sys.argv[1]), int(sys.argv[2])
    files = sorted(glob.glob(PAT))
    print("parts on disk:", len(files), "want", want_lo, want_hi, flush=True)
    hits = []
    for f in files:
        lo, hi = bounds(f)
        if lo is None:
            continue
        if hi >= want_lo and lo <= want_hi:
            hits.append((f, lo, hi))
            print("HIT %s [%d, %d]" % (f.split("/")[-1], lo, hi), flush=True)
    print("hits:", len(hits))
    for h in hits:
        print(h[0])


if __name__ == "__main__":
    main()
