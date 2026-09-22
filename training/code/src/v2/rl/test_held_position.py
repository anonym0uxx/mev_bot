#!/usr/bin/env python
"""Held-position continuation: reconstruct recorded management episodes and prove
the four candidates are valued from the SAME held state.

The c8 `management_replay` family was emitted with the prompt carrying the
POST-action position state (build_replay_v2.py applied the action before writing
the prompt), so:
  * 38.5% of rows showed inventory=0 and were labelled EXIT - a flat position
    cannot be managed, and the label was trivially implied by the state;
  * ADD rows showed the state AFTER the add, REDUCE rows after the reduce;
  * every label was the old heuristic `score_actions` (180 bp one-way, risk
    budget 2.0), not the reward engine.

This test (a) recovers the PRE-action state exactly, (b) checks it against the
recorded post-action state using the archived accounting rule, which is an
independent audit of the old corpus, and (c) scores the four candidate actions
through reward_engine.score_action_sequence(entry=...) on the real tape.

Run: /home/alon/qwen27b-venv/bin/python test_held_position.py
"""
from __future__ import annotations

import json
import os
import re
import sys
from collections import Counter, defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_engine import load_canonical_tapes  # noqa: E402
from build_management_c10 import load_tape, TRADES  # noqa: E402
import numpy as np  # noqa: E402
from reward_terms import RewardConfig, reward_scalar  # noqa: E402
from grpo_reward_bridge import ACTION_SEQUENCE, score_decision  # noqa: E402

C8 = "/training/v2/candidate_sft_c8/train.jsonl"
TAPE = "/training/v2/canonical/renormalized_v7/trades.jsonl"

CAPITAL = 1.0
ENTRY_TRANCHE = 0.5
OW = 0.018          # the archived replay's one-way cost (180 bp), used ONLY to
                    # re-derive history - never to score the new labels
MAX_ROWS = 4000

RE_MARK = re.compile(r"mark price \(SOL per raw token\): ([0-9.eE+-]+)")
RE_ENTRY = re.compile(r"entry price: ([0-9.eE+-]+)")
RE_HOLD = re.compile(r"holding time: ([0-9.]+) s")
RE_INV = re.compile(r"inventory: ([0-9.eE+-]+) raw tokens")
RE_CASH = re.compile(r"cash: ([0-9.eE+-]+) SOL")


def parse(r):
    u = r["messages"][1]["content"]
    m = r["meta"]
    return {
        "mint": m["mint"],
        "episode_id": m["episode_id"],
        "step": int(m["step"]),
        "action": m["action"],
        "t_dec": int(m["decision_time_unix_ms"]),
        "mark": float(RE_MARK.search(u).group(1)),
        "entry_px": float(RE_ENTRY.search(u).group(1)),
        "held_s": float(RE_HOLD.search(u).group(1)),
        "inv": float(RE_INV.search(u).group(1)),
        "cash": float(RE_CASH.search(u).group(1)),
    }


def apply_archived(row):
    """Apply the recorded action with the ARCHIVED accounting rule, exactly as
    build_replay_v2.py did, and return the resulting (qty, cash)."""
    qty, cash, cur, action = row["qty_pre"], row["cash_pre"], row["mark"], row["action"]
    ow = OW
    if action == "ADD":
        add_qty = qty * 0.5
        spend = add_qty * cur * (1 + ow)
        if spend > cash + 1e-12:
            action = "HOLD"
        else:
            cash -= spend
            qty += add_qty
    elif action == "REDUCE":
        sq = 0.5 * qty
        cash += sq * cur * (1 - ow)
        qty -= sq
    elif action == "EXIT":
        cash += qty * cur * (1 - ow)
        qty = 0.0
    return action, qty, cash


def main():
    rows = []
    with open(C8, encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            if (r.get("meta") or {}).get("family") != "management_replay":
                continue
            rows.append(parse(r))
            if len(rows) >= MAX_ROWS:
                break
    print(f"rows parsed: {len(rows)}")

    eps = defaultdict(list)
    for r in rows:
        eps[r["episode_id"]].append(r)
    for v in eps.values():
        v.sort(key=lambda x: x["step"])

    # ---- recover the pre-action state of every row -------------------------
    recovered, mismatches, drop_eps = 0, [], 0
    for eid, seq in eps.items():
        prev = None
        for i, r in enumerate(seq):
            if i == 0:
                if r["step"] != 0:
                    drop_eps += 1
                    break
                r["qty_pre"] = ENTRY_TRANCHE / r["entry_px"]
                r["cash_pre"] = CAPITAL - ENTRY_TRANCHE
            else:
                r["qty_pre"], r["cash_pre"] = prev["qty_post"], prev["cash_post"]
            exec_action, q_post, c_post = apply_archived(r)
            r["qty_post"], r["cash_post"] = q_post, c_post
            tol = 2e-5 * max(1.0, abs(r["inv"]), abs(r["cash"])) + 2e-5
            if abs(q_post - r["inv"]) > tol or abs(c_post - r["cash"]) > tol:
                mismatches.append((eid, r["step"], r["action"],
                                   round(q_post, 9), r["inv"], round(c_post, 9), r["cash"]))
            else:
                recovered += 1
            prev = r
    print(f"pre-state recovered & verified: {recovered}/{len(rows)}  "
          f"mismatch={len(mismatches)}  episodes_without_step0={drop_eps}")
    for m in mismatches[:5]:
        print("   MISMATCH", m)

    # ---- score the four candidates on the real tape -------------------------
    mints = {r["mint"] for r in rows}
    print(f"loading tapes for {len(mints)} mints ...")
    # Same loader the builder uses: fills ordered (recv_unix_ms, tokens_raw) - the
    # archive's order - with the house dust filter applied, plus the unfiltered
    # archive-order series for the entry index. Two separate concerns: history is
    # read off the unfiltered series (that is what the archive saw), the label is
    # scored off the filtered one (that is what every production scorer uses).
    tapes, full_series = load_tape(TRADES, mints)
    print(f"tapes loaded: {len(tapes)}")

    entry_bad = 0
    entry_bad_details = []
    for r in rows:
        seed = int(r["episode_id"].rsplit(":", 1)[1])
        fs = full_series.get(r["mint"])
        if fs is None:
            continue
        tt_all, px_all = fs
        jf = int(np.searchsorted(tt_all, seed, side="right"))
        if jf >= tt_all.size:
            entry_bad += 1
            continue
        px_entry = float(px_all[jf])
        # the archived prompt prints the entry price to 8 significant digits
        if not np.isfinite(px_entry) or px_entry <= 0 \
                or abs(px_entry - r["entry_px"]) > 1e-4 * r["entry_px"]:
            entry_bad += 1
            if len(entry_bad_details) < 8:
                entry_bad_details.append({"mint": r["mint"], "eid": r["episode_id"],
                                          "step": r["step"], "prompt_entry_px": r["entry_px"],
                                          "tape_px_at_seed": px_entry, "seed": seed})
            continue
        r["t_entry"] = int(tt_all[jf])
        r["px_entry_tape"] = px_entry
    print(f"entry recovered from the archive-order series: "
          f"{sum(1 for r in rows if r.get('t_entry'))}/{len(rows)}  bad={entry_bad}")

    cfg = RewardConfig(lambda_exposure=0.08884)
    per_action = Counter()
    status = Counter()
    refused_by = Counter()
    violations = Counter()
    viol_details = []
    n_scored = 0
    label_by_action = Counter()
    ties = 0
    sample_out = []
    for r in rows:
        t = tapes.get(r["mint"])
        if t is None:
            status["no_tape"] += 1
            continue
        if not r.get("t_entry"):
            status["entry_unreconstructible"] += 1
            continue
        entry = {"entry_px": r["px_entry_tape"], "t_entry_ms": r["t_entry"],
                 "qty_tokens": r["qty_pre"], "cash_sol": r["cash_pre"], "capital_sol": CAPITAL}
        dec = score_decision(t, r["t_dec"], family="management", cfg=cfg, entry=entry)
        rewards = {}
        for a, d in dec["per_action"].items():
            status[d["status"]] += 1
            if d["status"] != "ok":
                refused_by[a] += 1
            else:
                ep = d["episode"]
                for v in ep["violations"]:
                    violations[v] += 1
                    if v == "cost_within_bounds" and len(viol_details) < 8:
                        viol_details.append({"mint": r["mint"], "step": r["step"], "action": a,
                                             "costs": ep["costs_sol"], "deploy_sol": ep["deploy_sol"],
                                             "total_cost_sol": ep["total_cost_sol"],
                                             "qty_at_decision": ep["qty_at_decision"]})
                rewards[a] = d["reward"]
                if ep["entry_mode"] != "continuation":
                    violations["entry_mode_not_continuation"] += 1
        if len(rewards) < 4:
            continue
        n_scored += 1
        best = max(rewards, key=lambda k: rewards[k])
        label_by_action[best] += 1
        top = sorted(rewards.values(), reverse=True)
        if abs(top[0] - top[1]) < 1e-9:
            ties += 1
        if len(sample_out) < 12:
            sample_out.append({"mint": r["mint"], "step": r["step"], "old": r["action"],
                               "new": best,
                               "rewards": {k: round(v, 6) for k, v in rewards.items()}})

    print(json.dumps({
        "rows": len(rows), "scored_all_four": n_scored,
        "candidate_status": dict(status),
        "refused_by_action": dict(refused_by),
        "conservation_violations": dict(violations),
        "label_by_action": dict(label_by_action),
        "exact_ties": ties,
        "viol_details": viol_details,
        "entry_bad_details": entry_bad_details,
    }, indent=1))
    print(json.dumps({"samples": sample_out}, indent=1))
    fails = []
    if n_scored < 0.95 * len(rows):
        fails.append(f"only {n_scored}/{len(rows)} rows scored all four candidates")
    if violations:
        fails.append(f"conservation violations: {dict(violations)}")
    if label_by_action and len(label_by_action) < 3:
        fails.append(f"degenerate label distribution {dict(label_by_action)}")
    if mismatches:
        fails.append(f"{len(mismatches)} pre-state recovery mismatches")
    print(json.dumps({"suite": "held_position", "failed": len(fails), "failures": fails}, indent=1))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())