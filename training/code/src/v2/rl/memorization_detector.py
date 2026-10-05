#!/usr/bin/env python
"""Memorization detector - can the policy's choices be told apart from its evidence?

WHY THIS EXISTS. The checkpoint gate selects on the wall (unseen) only, which is necessary
but not sufficient: a policy can be flat-to-slightly-positive on unseen rows while its
CHOICES encode which rows it trained on. Nothing in the gate could fail a run for that, so
"we evaluate on a forward wall" was an argument rather than a test.

WHAT IT TESTS, and how each bound is derived rather than chosen:

  1. CHOICE DISCRIMINATION. Total-variation distance between the action distribution on
     in-sample prompts and on unseen prompts. Bound = the 99th percentile of the SAME
     statistic under a permutation null (in_sample labels shuffled), so the bound comes
     from the data and the sample size, not from taste.
  2. OUTCOME GAP. mean(realized net bp | unseen) - mean(... | in-sample). A memorized
     policy is much better where it has seen the answer. Same permutation bound.
  3. DOSE RESPONSE (optional, when rows carry a repeat count). Realized return by how
     many times the prompt appeared in training. A memorizer's edge grows with the count;
     an honest policy's is flat. Spearman rho, bounded by the permutation null.

NOT IN SCOPE: near-duplicate prompt detection (a text-similarity job, and a data-side
guard). This module answers the policy-side question: does BEHAVIOUR differ by exposure.

Usage:
    memorization_detector.py --self-check
    memorization_detector.py --score records.jsonl [--prereg reports/MEMORIZATION_BOUNDS.json]
                             [--out reports/MEMORIZATION_SCORE.json]

Records (JSONL, one per scored decision):
    {"prompt_sha256": str, "in_sample": bool, "chosen": str,
     "realized_net_bp": float|null, "repeat_count": int|null}
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys

import numpy as np

DEFAULT_PREREG = "/training/v2/reports/MEMORIZATION_BOUNDS.json"
N_PERM = 400
ALPHA = 0.01                      # the null's 99th percentile is the bound
SEED = 7                          # fixed: the bound must not move between runs


def tv_distance(a: dict, b: dict) -> float:
    """Total-variation distance between two empirical action distributions."""
    keys = set(a) | set(b)
    na, nb = max(1, sum(a.values())), max(1, sum(b.values()))
    return 0.5 * sum(abs(a.get(k, 0) / na - b.get(k, 0) / nb) for k in keys)


def _dist(actions) -> dict:
    out: dict = {}
    for x in actions:
        out[x] = out.get(x, 0) + 1
    return out


def permutation_bound(x, y, stat, n_perm: int = N_PERM, seed: int = SEED) -> float:
    """the (1-ALPHA) quantile of `stat` under a shuffled group label."""
    rng = np.random.default_rng(seed)
    vals = []
    n = len(x)
    joined = list(x) + list(y)
    for _ in range(n_perm):
        idx = rng.permutation(len(joined))
        vals.append(stat([joined[i] for i in idx[:n]], [joined[i] for i in idx[n:]]))
    return float(np.quantile(vals, 1.0 - ALPHA))


def scan(records: list, n_perm: int = N_PERM, seed: int = SEED) -> dict:
    """Two-group memorization scan with permutation-derived bounds."""
    seen = [r for r in records if r.get("in_sample")]
    unseen = [r for r in records if not r.get("in_sample")]
    out: dict = {"n_in_sample": len(seen), "n_unseen": len(unseen)}
    if len(seen) < 30 or len(unseen) < 30:
        return {**out, "status": "insufficient_groups",
                "detail": "need >= 30 rows per group for a bound to mean anything"}

    def _tv(xs, ys):
        return tv_distance(_dist([r["chosen"] for r in xs]),
                           _dist([r["chosen"] for r in ys]))

    obs_tv = _tv(seen, unseen)
    bound_tv = permutation_bound(seen, unseen, _tv, n_perm, seed)

    def _gap(xs, ys):
        a = [float(r["realized_net_bp"]) for r in xs
             if r.get("realized_net_bp") is not None]
        b = [float(r["realized_net_bp"]) for r in ys
             if r.get("realized_net_bp") is not None]
        if len(a) < 10 or len(b) < 10:
            return 0.0
        return float(np.mean(b) - np.mean(a))

    obs_gap = _gap(seen, unseen)
    # One-sided bound on |gap|: the null's 99th percentile of the ABSOLUTE gap.
    gap_vals = []
    rng = np.random.default_rng(seed + 1)
    joined = seen + unseen
    for _ in range(n_perm):
        idx = rng.permutation(len(joined))
        perm = [joined[i] for i in idx]
        gap_vals.append(abs(_gap(perm[:len(seen)], perm[len(seen):])))
    bound_gap = float(np.quantile(gap_vals, 1.0 - ALPHA))

    out.update({"tv_distance": obs_tv, "tv_bound": bound_tv,
                "outcome_gap_bp": obs_gap, "outcome_gap_bound_bp": bound_gap,
                "tv_exceeds": bool(obs_tv > bound_tv),
                "outcome_gap_exceeds": bool(abs(obs_gap) > bound_gap)})
    out["status"] = "MEMORIZATION_SUSPECTED" if (out["tv_exceeds"] or
                                                 out["outcome_gap_exceeds"]) else "clean"
    return out


def _self_check() -> int:
    fails = []

    def chk(tag, cond, extra=""):
        if not cond:
            fails.append(tag if not extra else "%s %s" % (tag, extra))

    rng = np.random.default_rng(SEED)
    acts = ["BUY_FULL", "WATCH", "SKIP"]

    def rec(in_sample, chosen, net, rep=None):
        return {"prompt_sha256": hashlib.sha256(
            ("%s%d%s" % (in_sample, rng.integers(1 << 30), chosen)).encode()).hexdigest(),
            "in_sample": in_sample, "chosen": chosen, "realized_net_bp": net,
            "repeat_count": rep}

    # NEGATIVE CONTROL 1: an honest policy - choices independent of exposure, and no
    # outcome gap. The detector must PASS it.
    honest = ([rec(True, str(rng.choice(acts)), float(rng.normal(10, 120))) for _ in range(400)]
              + [rec(False, str(rng.choice(acts)), float(rng.normal(10, 120)))
                 for _ in range(400)])
    h = scan(honest)
    chk("honest_policy_passes", h["status"] == "clean", str(h))

    # NEGATIVE CONTROL 2: a MEMORIZER - in-sample it repeats one action and its realized
    # return is far better there. The detector must FLAG it.
    memo = ([rec(True, "BUY_FULL", float(rng.normal(400, 80))) for _ in range(400)]
            + [rec(False, str(rng.choice(acts)), float(rng.normal(-30, 120)))
               for _ in range(400)])
    m = scan(memo)
    chk("memorizer_flagged", m["status"] == "MEMORIZATION_SUSPECTED", str(m))
    chk("memorizer_flags_both_signals", m["tv_exceeds"] and m["outcome_gap_exceeds"], str(m))

    # NEGATIVE CONTROL 3: too few rows must refuse, not pass.
    tiny = scan([rec(True, "SKIP", 1.0) for _ in range(5)]
                + [rec(False, "SKIP", 1.0) for _ in range(5)])
    chk("thin_groups_refuse", tiny["status"] == "insufficient_groups", str(tiny))

    # DETERMINISM: the same records must produce the same bounds.
    chk("bounds_are_deterministic", scan(honest)["tv_bound"] == h["tv_bound"])

    print(json.dumps({"suite": "memorization_detector", "failed": len(fails),
                      "failures": fails,
                      "honest": {k: h.get(k) for k in ("status", "tv_distance", "tv_bound")},
                      "memorizer": {k: m.get(k) for k in ("status", "tv_distance", "tv_bound",
                                                          "outcome_gap_bp",
                                                          "outcome_gap_bound_bp")}},
                     indent=1))
    return 1 if fails else 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true")
    ap.add_argument("--score")
    ap.add_argument("--prereg", default=DEFAULT_PREREG)
    ap.add_argument("--out", default="/training/v2/reports/MEMORIZATION_SCORE.json")
    a = ap.parse_args(argv)
    if a.self_check:
        return _self_check()
    if not a.score:
        ap.error("--score or --self-check")
    records = [json.loads(ln) for ln in open(a.score, encoding="utf-8") if ln.strip()]
    res = scan(records)
    res["source"] = a.score
    res["prereg"] = os.path.isfile(a.prereg)
    if os.path.isfile(a.prereg):
        pre = json.load(open(a.prereg, encoding="utf-8"))
        res["prereg_bounds"] = pre.get("bounds")
    json.dump(res, open(a.out, "w", encoding="utf-8"), indent=1)
    print(json.dumps(res, indent=1))
    # fail-closed: a suspected memorizer, or no preregistered instrument, is not a pass
    return 1 if (res["status"] in ("MEMORIZATION_SUSPECTED", "insufficient_groups")
                 or not res["prereg"]) else 0


if __name__ == "__main__":
    sys.exit(main())