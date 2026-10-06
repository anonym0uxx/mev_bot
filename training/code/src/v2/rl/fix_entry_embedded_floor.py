#!/usr/bin/env python
"""Regenerate the embedded pool floor in the entry family's barrier_triplet.

THE DEFECT. `meta.barrier_triplet.cost_floor_bps = {"amm": 0, "bonding_curve": 189}`
on all 84,681 rows. The AMM value 0 is the RETIRED model - the one that charged PumpSwap
nothing on the argument that the pool retains its fee. The authority now returns 60
(2 x (30 venue + 0.1 tx)). Gate 11 checked the PROMPT TEXT for the retired figure and
never looked at meta, which is why this survived both the cost pass and the audit.

Nothing else in any family carries an embedded cost number: a scan of all four families
found only `.barrier_triplet.cost_floor_bps.*` (decision) and `.cost_authority.*`
(management, current by construction).

Written to a temp file and renamed into place so a failure cannot leave a half-edited
corpus behind.
"""
import argparse
import collections
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from exit_mechanics import cost_floor_bps, DEPLOY_SOL_CANONICAL  # noqa: E402

TARGET = "/training/v2/candidate_sft_c10_entry_labeled"


def expected():
    return {"amm": cost_floor_bps("amm", DEPLOY_SOL_CANONICAL, 0),
            "bonding_curve": cost_floor_bps("bonding_curve", DEPLOY_SOL_CANONICAL, 0)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--splits", nargs="+",
                    default=["train", "validation", "examination"])
    ap.add_argument("--dir", default=TARGET)
    ap.add_argument("--verify-only", action="store_true")
    args = ap.parse_args()
    exp = expected()
    print("authority floor:", exp, flush=True)
    total_bad = 0
    for split in args.splits:
        p = os.path.join(args.dir, f"{split}.jsonl")
        if not os.path.exists(p):
            continue
        tmp = p + ".tmp"
        st = collections.Counter()
        fout = None if args.verify_only else open(tmp, "w")
        with open(p) as fh:
            for line in fh:
                r = json.loads(line)
                m = r["meta"]
                bt = m.get("barrier_triplet") or {}
                cur = bt.get("cost_floor_bps")
                if not isinstance(cur, dict):
                    st["no_floor_field"] += 1
                elif cur == exp:
                    st["already_correct"] += 1
                else:
                    st["stale"] += 1
                    if not args.verify_only:
                        bt["cost_floor_bps"] = dict(exp)
                        bt["cost_floor_source"] = "exit_mechanics.cost_floor_bps@venue_resolved"
                        m["barrier_triplet"] = bt
                if args.verify_only:
                    if isinstance(cur, dict) and cur != exp:
                        st["still_wrong"] += 1
                if fout:
                    fout.write(json.dumps(r, ensure_ascii=False) + "\n")
                st["rows"] += 1
        if fout:
            fout.close()
            os.replace(tmp, p)          # atomic: never a half-written corpus
        bad = st.get("still_wrong", 0) + (st["no_floor_field"] if args.verify_only else 0)
        total_bad += bad
        print(f"  {split:<12} rows={st['rows']:<7} stale_found={st['stale']:<7} "
              f"wrong_after={bad}", flush=True)
    print("TOTAL_WRONG", total_bad)
    return 1 if total_bad else 0


if __name__ == "__main__":
    sys.exit(main())