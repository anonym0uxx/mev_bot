#!/usr/bin/env python
"""FORWARD WALL v1 (D4 decision): one immutable timestamp; nothing after it is ever
trained on. 8 h wide by default - a genuinely forward test that keeps ~90% of train
(the 24 h alternative would hold out 99,199 of 253,347 rows = 39%).

Writes, into a NEW directory:
  FORWARD_WALL.json      wall ms/PT, counts per split, sha256 of the id list
  wall_episode_ids.txt   the reserved episode ids (never train)
  wall_episodes.jsonl    same, with mint/split/family/t_dec
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import re
import sys

EP = re.compile(rb'"episode_id":\s*"v2:[^"]*?:(\d{13})"')
DT = re.compile(rb'DECISION TIME \(unix ms\):\s*(\d{13})')
MI = re.compile(rb'"mint":\s*"([^"]+)"')


def scan(path):
    out = []
    with open(path, "rb") as f:
        buf = b""
        while True:
            c = f.read(1 << 24)
            if not c:
                break
            buf += c
            cut = buf.rfind(b"\n")
            if cut < 0:
                continue
            body, buf = buf[:cut], buf[cut:]
            for line in body.split(b"\n"):
                if not line:
                    continue
                m = EP.search(line) or DT.search(line)
                if not m:
                    continue
                mm = MI.search(line)
                out.append((int(m.group(1)), mm.group(1).decode() if mm else None,
                            (json.loads(line).get("family") or "?")))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default="/training/v2/candidate_sft_c4")
    ap.add_argument("--out", default="/training/v2/forward_wall_v1")
    ap.add_argument("--hours", type=float, default=8.0)
    ap.add_argument("--report", default="/training/v2/reports/FORWARD_WALL_V1.json")
    a = ap.parse_args()
    rows = []
    for s in ("train", "validation", "examination"):
        p = os.path.join(a.src, s + ".jsonl")
        for t, mint, fam in scan(p):
            rows.append((t, mint, fam, s))
    t_max = max(r[0] for r in rows)
    widths = {}
    for h in (2, 4, 8, 12, 24, 48):
        cut = t_max - int(h * 3.6e6)
        w = [r for r in rows if r[0] >= cut]
        widths[str(h)] = {"cut_ms": cut, "rows": len(w),
                          "mints": len({r[1] for r in w}),
                          "train_rows_lost": sum(1 for r in w if r[3] == "train")}
    cut = t_max - int(a.hours * 3.6e6)
    W = sorted([r for r in rows if r[0] >= cut], key=lambda r: r[0])
    ids = [r[2] and r for r in W]
    os.makedirs(a.out, exist_ok=True)
    h = hashlib.sha256()
    with open(os.path.join(a.out, "wall_episode_ids.txt"), "w") as fh:
        for t, mint, fam, s in W:
            fh.write(json.dumps([t, mint, fam, s]) + "\n")
    with open(os.path.join(a.out, "wall_episodes.jsonl"), "w") as fh:
        for t, mint, fam, s in W:
            fh.write(json.dumps({"t_dec_ms": t, "mint": mint, "family": fam,
                                 "split": s}) + "\n")
    blob = open(os.path.join(a.out, "wall_episode_ids.txt"), "rb").read()
    rep = {"schema": "forward_wall_v1", "policy": "nothing at or after wall_ms is ever trained on",
           "source": a.src, "wall_hours": a.hours, "wall_ms": cut, "t_max_ms": t_max,
           "reserved_rows": len(W), "reserved_mints": len({r[1] for r in W}),
           "reserved_by_split": dict(collections.Counter(r[3] for r in W)),
           "reserved_families": dict(collections.Counter(r[2] for r in W)),
           "corpus_rows": len(rows), "corpus_mints": len({r[1] for r in rows}),
           "corpus_t_max_ms": t_max,
           "train_rows_removed": sum(1 for r in W if r[3] == "train"),
           "train_rows_total": sum(1 for r in rows if r[3] == "train"),
           "width_sensitivity": widths,
           "wall_episode_ids_sha256": hashlib.sha256(blob).hexdigest(),
           "files": {"ids": os.path.join(a.out, "wall_episode_ids.txt"),
                     "rows": os.path.join(a.out, "wall_episodes.jsonl")}}
    json.dump(rep, open(a.report, "w"), indent=1)
    print(json.dumps({k: v for k, v in rep.items() if k != "width_sensitivity"}, indent=1))
    print(json.dumps(rep["width_sensitivity"], indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())