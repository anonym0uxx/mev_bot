#!/usr/bin/env python
"""TRUE FORWARD HOLDOUT v1.

WHY THIS EXISTS
---------------
`candidate_sft_c3` / `candidate_sft_c4` already carry an `examination` partition,
but that partition is a forward **mint-GROUP** holdout, not a forward-in-TIME
holdout: its decision-time range still overlaps the train partition's edges
(measured below and written into the manifest).  Treating it as "the future" is
therefore wrong, and the overlap is documented here rather than hidden.

This builder reserves the most RECENT captured decision window instead:

    forward window W = [t_max - DELTA, t_max]
    t_max = max decision time in the source partitions

Every episode whose decision time falls in W is written to a RESERVED partition
that must never be trained on.  Output goes to a NEW directory; the source
candidate dirs are read-only here.

Reported (and written to the manifest):
  * exact time bounds of W in unix ms and in UTC,
  * row counts, mint counts, family counts and split provenance,
  * the pre-existing examination overlap that made it unusable as a forward test,
  * whether enough recent data exists for W to be meaningful.

Usage:
  python forward_holdout_v1.py [--src /training/v2/candidate_sft_c4] [--delta-hours 24]
"""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import os
import re
import sys

TS_RX = re.compile(r"(\d{13})")


def utc(ms):
    return dt.datetime.fromtimestamp(ms / 1000.0, dt.timezone.utc).isoformat()


def pt(ms):
    return (dt.datetime.fromtimestamp(ms / 1000.0, dt.timezone.utc)
            - dt.timedelta(hours=7)).strftime("%Y-%m-%d %H:%M:%S") + " PT"


def read_episodes(path):
    """Yield (t_dec_ms, mint, family, episode_id) for each SFT row."""
    with open(path) as fh:
        for line in fh:
            o = json.loads(line)
            eid = o.get("episode_id", "")
            m = TS_RX.search(eid.rsplit(":", 1)[-1] if ":" in eid else "")
            t = int(m.group(1)) if m else None
            if t is None:
                mm = TS_RX.search(line)
                t = int(mm.group(1)) if mm else None
            yield t, o.get("mint"), o.get("family") or (o.get("meta") or {}).get("family"), eid


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default="/training/v2/candidate_sft_c4")
    ap.add_argument("--out", default="/training/v2/forward_holdout_v1")
    ap.add_argument("--delta-hours", type=float, default=24.0)
    ap.add_argument("--min-episodes", type=int, default=2000)
    ap.add_argument("--min-mints", type=int, default=100)
    ap.add_argument("--report", default="/training/v2/reports/FORWARD_HOLDOUT_V1.json")
    a = ap.parse_args()

    parts = {p: os.path.join(a.src, p + ".jsonl")
             for p in ("train", "validation", "examination")}
    for p, f in parts.items():
        if not os.path.exists(f):
            raise SystemExit("missing partition: " + f)

    per_split = {}
    all_rows = []
    for split, f in parts.items():
        rows = list(read_episodes(f))
        rows = [r for r in rows if r[0] is not None]
        all_rows += [(split,) + r for r in rows]
        ts = [r[0] for r in rows]
        per_split[split] = {
            "rows": len(rows),
            "rows_with_decision_ts": len(ts),
            "t_dec_min_ms": min(ts), "t_dec_max_ms": max(ts),
            "t_dec_min_pt": pt(min(ts)), "t_dec_max_pt": pt(max(ts)),
            "distinct_mints": len({r[1] for r in rows}),
            "families": dict(collections.Counter(r[2] for r in rows)),
        }

    t_max = max(per_split[s]["t_dec_max_ms"] for s in per_split)
    t_min = min(per_split[s]["t_dec_min_ms"] for s in per_split)
    delta = int(a.delta_hours * 3_600_000)
    cut = t_max - delta

    W = [r for r in all_rows if r[1] >= cut]
    fam = collections.Counter(r[3] for r in W)
    by_split = collections.Counter(r[0] for r in W)
    mints = {r[2] for r in W}

    # ---- the documented caveat: the existing examination partition is NOT
    # forward in time - its decision range overlaps train's edges.
    caveat = {
        "train_t_dec_min_ms": per_split["train"]["t_dec_min_ms"],
        "train_t_dec_max_ms": per_split["train"]["t_dec_max_ms"],
        "examination_t_dec_min_ms": per_split["examination"]["t_dec_min_ms"],
        "examination_t_dec_max_ms": per_split["examination"]["t_dec_max_ms"],
        "examination_minus_train_min_ms":
            per_split["examination"]["t_dec_min_ms"] - per_split["train"]["t_dec_min_ms"],
        "overlap_ms_of_train_range_captured_by_examination": None,
        "verdict": ("examination is a forward mint-GROUP holdout; its decision-time "
                    "range still OVERLAPS the train range, so it is not a "
                    "forward-in-time test"),
    }
    tr_lo, tr_hi = per_split["train"]["t_dec_min_ms"], per_split["train"]["t_dec_max_ms"]
    ex_lo, ex_hi = per_split["examination"]["t_dec_min_ms"], per_split["examination"]["t_dec_max_ms"]
    ov = max(0, min(tr_hi, ex_hi) - max(tr_lo, ex_lo))
    caveat["overlap_ms_of_train_range_captured_by_examination"] = ov
    caveat["overlap_pct_of_train_range"] = round(100.0 * ov / max(tr_hi - tr_lo, 1), 4)
    # median decision time per split (streamed, exact via sort)
    med = {}
    for split in per_split:
        ts = sorted(r[1] for r in all_rows if r[0] == split)
        med[split] = ts[len(ts) // 2]
    caveat["median_t_dec_ms_by_split"] = med
    caveat["examination_median_minus_train_median_ms"] = med["examination"] - med["train"]
    caveat["examination_median_minus_train_median_hours"] = round(
        (med["examination"] - med["train"]) / 3.6e6, 3)

    enough = (len(W) >= a.min_episodes and len(mints) >= a.min_mints)
    os.makedirs(a.out, exist_ok=True)
    reserved_path = os.path.join(a.out, "forward_holdout.jsonl")
    with open(reserved_path, "w") as fh:
        for split, t, mint, family, eid in sorted(W, key=lambda r: r[1]):
            fh.write(json.dumps({"episode_id": eid, "mint": mint, "split": split,
                                 "family": family, "t_dec_ms": t}) + "\n")
    # an explicit exclusion list so a trainer can drop these ids without parsing
    ids_path = os.path.join(a.out, "forward_holdout_episode_ids.txt")
    with open(ids_path, "w") as fh:
        for r in sorted(W, key=lambda r: r[1]):
            fh.write(r[4] + "\n")

    rep = {
        "schema": "forward_holdout_v1",
        "source_dir": a.src,
        "source_partitions": {k: v for k, v in parts.items()},
        "capture_bounds": {"t_dec_min_ms": t_min, "t_dec_max_ms": t_max,
                           "t_dec_min_pt": pt(t_min), "t_dec_max_pt": pt(t_max),
                           "span_hours": round((t_max - t_min) / 3.6e6, 3)},
        "forward_window": {
            "delta_hours": a.delta_hours,
            "t_start_ms": cut, "t_end_ms": t_max,
            "t_start_pt": pt(cut), "t_end_pt": pt(t_max),
            "t_start_utc": utc(cut), "t_end_utc": utc(t_max),
            "rows": len(W),
            "distinct_mints": len(mints),
            "families": dict(fam),
            "source_splits": dict(by_split),
            "episode_ids_path": ids_path,
            "reserved_path": reserved_path,
        },
        "meaningfulness": {
            "min_episodes_required": a.min_episodes,
            "min_mints_required": a.min_mints,
            "enough_recent_data": bool(enough),
            "verdict": ("MEANINGFUL: the reserved window has %d episodes across %d "
                        "mints in the last %.1f h of captured decision time"
                        % (len(W), len(mints), a.delta_hours)) if enough else
                       ("NOT MEANINGFUL: the reserved window has only %d episodes "
                        "across %d mints; widen --delta-hours or capture more"
                        % (len(W), len(mints))),
        },
        "existing_examination_partition_caveat": caveat,
        "per_split": per_split,
        "note": ("These rows MUST be removed from every training partition before "
                 "the next run. They live in train/validation/examination today; "
                 "this file only reserves them, it does not edit the candidates."),
    }
    json.dump(rep, open(a.report, "w"), indent=1)
    print(json.dumps(rep, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
