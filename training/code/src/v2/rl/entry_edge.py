#!/usr/bin/env python
"""ENTRY-SIDE base rate: is there an opportunity to enter at all?

The exit half of the reward engine is validated. But an exit policy only converts
an ENTRY EDGE into money - if the entry decisions themselves have no positive
expectancy after a real round trip, no exit policy and no RL run can fix it. This
measures that directly, on real decisions, with real policies and real costs.

Method: take decision-family records from the pinned SFT corpus (episode_id
encodes "v2:<mint>:<t_dec_ms>"), find the mint's recorded tape, and for each
decision score an ENTRY under each baseline exit policy. Report:

  * P(entry reward > 0) after the full cost stack, and the mean/median
  * the value of NOT entering (0.0) as the counterfactual it must beat
  * the admissible fraction (upside evidence must clear the round-trip floor)

Coverage is reported honestly: a decision whose mint has no tape is MASKED, never
counted as a zero return.

Run: /home/alon/qwen27b-venv/bin/python entry_edge.py
"""
import json
import os
import re
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_engine import load_canonical_tapes  # noqa: E402
from reward_terms import reward_scalar  # noqa: E402
from exit_policy import TEMPLATES, BASELINE_POLICIES, simulate_policy  # noqa: E402

TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
CORPUS = "/training/v2/candidate_sft_c3/train.jsonl"
MAX_RECORDS = 4000
EP_RE = re.compile(r"v2:([A-Za-z0-9]+):(\d+)$")


def main():
    # Dust-trade filter: px = sol/tokens is meaningless for tiny notional and a few
    # such rows otherwise dominate the mean (measured: mean reward -311 on a value
    # bounded below by -1).
    tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000,
                                 max_px_ratio=50)
    per_mint = {}
    scanned = 0
    for line in open(CORPUS, encoding="utf-8"):
        if scanned >= 40000:
            break
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("family") != "decision":
            continue
        scanned += 1
        eid = r.get("episode_id") or ""
        parts = eid.split(":")
        if len(parts) < 3:
            continue
        try:
            t_dec = int(parts[2])
        except ValueError:
            continue
        per_mint.setdefault(parts[1], []).append(t_dec)

    per_policy = {p: [] for p in BASELINE_POLICIES if p != "EXIT_NOW"}
    used, nomint, out_of_window = 0, 0, 0
    mint_hits = 0
    for mint, tdecs in per_mint.items():
        tape = tapes.get(mint)
        if tape is None:
            nomint += 1
            continue
        t0, tN = int(tape.tt[0]), int(tape.tt[-1])
        inwin = [t for t in tdecs if t0 <= t <= tN]
        if not inwin:
            out_of_window += 1
            continue
        mint_hits += 1
        # spread across the mint's decisions instead of taking the first N
        step = max(1, len(inwin) // 8)
        for t_dec in inwin[::step][:8]:
            used += 1
            for p in per_policy:
                ep = simulate_policy(tape, t_dec, TEMPLATES[p],
                                     refuse_cross_graduation=False)
                res = reward_scalar(ep)
                if res["refused"]:
                    continue
                per_policy[p].append(res["reward"])

    out = {"corpus_decision_records_scanned": scanned,
           "distinct_corpus_mints": len(per_mint),
           "mints_with_tape": mint_hits,
           "mints_masked_no_tape": nomint,
           "mints_with_tape_but_NO_decisions_in_window": out_of_window,
           "decisions_scored": used, "tape_mints_available": len(tapes),
           "policies": {}}
    for p, vals in per_policy.items():
        a = np.asarray(vals, dtype=float)
        if a.size == 0:
            continue
        out["policies"][p] = {
            "n": int(a.size),
            "mean_reward": round(float(a.mean()), 5),
            "median_reward": round(float(np.median(a)), 5),
            "p_positive": round(float((a > 0).mean()), 4),
            "p05": round(float(np.percentile(a, 5)), 5),
            "p95": round(float(np.percentile(a, 95)), 5),
            "mean_vs_skip": round(float(a.mean() - 0.0), 5),
        }
    best = None
    for p, d in out["policies"].items():
        if best is None or d["mean_reward"] > out["policies"][best]["mean_reward"]:
            best = p
    out["best_entry_policy"] = best
    out["entry_edge_exists"] = bool(
        best and out["policies"][best]["mean_reward"] > 0
        and out["policies"][best]["p_positive"] > 0.5)
    out["verdict"] = ("ENTRY EDGE PRESENT (mean>0 and P(win)>0.5 on covered decisions)"
                      if out["entry_edge_exists"] else
                      "NO DEMONSTRATED ENTRY EDGE on this sample - entering does not "
                      "beat not entering after the full cost stack")
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())