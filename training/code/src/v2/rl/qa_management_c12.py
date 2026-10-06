#!/usr/bin/env python
"""QA gates for the c12 barrier-aligned management_replay family.

COPY OF qa_management_c10.py WITH THREE DELIBERATE CHANGES AND NOTHING ELSE:
 1. DIR points at /training/v2/reports/mgmt_c12_aligned.
 2. Retention baseline is the SOURCE corpus /training/v2/candidate_sft_c11
    (the c12 build's input), not the c8 archive.
 3. The independent re-score uses each row's OWN meta.management_horizon_ms
    (horizon_policy=remaining_to_entry_barrier makes the horizon row-specific);
    a fixed 300 s re-score would grade the aligned labels against the very
    clock the rebuild retired.

QA gates for the rebuilt c10 management_replay family.

Every gate is a hard assertion with the measured numbers reported, including the
edge/corner cases: flat positions, the extremes of the P&L distribution, ties,
zero-margin labels, first/last step of an episode, and a full independent
RE-SCORING of a random sample (the strongest available check: the label must be
reproducible from the recorded state alone).

Run: /home/alon/qwen27b-venv/bin/python qa_management_c10.py [--sample 200]
"""
from __future__ import annotations

import argparse, json, os, random, re, sys
from collections import Counter, defaultdict

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build_management_c12 import load_tape, TRADES  # noqa: E402
from reward_terms import RewardConfig  # noqa: E402
from grpo_reward_bridge import score_decision  # noqa: E402
import cost_authority as CA  # noqa: E402

DIR = "/training/v2/reports/mgmt_c12_aligned"


def _ca(m):
    """The row's cost-authority block. One accessor: a rename must break here loudly
    rather than silently skipping the gates that read it."""
    ca = m.get("cost_authority")
    if not isinstance(ca, dict):
        raise KeyError("cost_authority block missing on a management row")
    return ca
C8 = "/training/v2/candidate_sft_c11"  # c12: retention baseline = the build's SOURCE corpus
SPLITS = ("train", "validation", "examination")
ACTIONS = ("HOLD", "ADD", "REDUCE", "EXIT")
EXPECTED_SYSTEM = ("You are an on-chain position manager for pump.fun/pumpswap memecoins. You "
                   "hold a position. Given the causal market snapshot and your position state, "
                   "choose exactly one action: HOLD, ADD, REDUCE, EXIT. "
                   + CA.model_statement() + " Answer in the fixed format.")

RE_INV = re.compile(r"inventory: ([0-9.eE+-]+) raw tokens")
RE_CASH = re.compile(r"cash: ([0-9.eE+-]+) SOL")
RE_MARK = re.compile(r"mark price \(lamports per raw token\): ([0-9.eE+-]+)")
RE_ENTRY = re.compile(r"entry price \(lamports per raw token\): ([0-9.eE+-]+)")
RE_UPNL = re.compile(r"unrealized PnL: ([0-9.eE+-]+) bp")
RE_HELD = re.compile(r"holding time: ([0-9.]+) s")


def load(split):
    p = os.path.join(DIR, f"{split}.jsonl")
    return [json.loads(l) for l in open(p, encoding="utf-8")]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sample", type=int, default=200)
    a = ap.parse_args()
    failures = []
    report = {}

    c8_counts = {}
    for s in SPLITS:
        n = 0
        for l in open(f"{C8}/{s}.jsonl", encoding="utf-8"):
            if (json.loads(l).get("meta") or {}).get("family") == "management_replay":
                n += 1
        c8_counts[s] = n

    all_rows = {}
    for s in SPLITS:
        rows = load(s)
        all_rows[s] = rows
        n_flat = 0
        bad_cost = 0
        bad_system = 0
        bad_action = 0
        bad_mismatch = 0
        bad_upnl = 0
        retired_cost = 0
        wrong_units = 0
        nonfinite = 0
        causality_bad = 0
        margins, upnls, steps, qty_ok = [], [], Counter(), 0
        for r in rows:
            m = r["meta"]
            u = r["messages"][1]["content"]
            ass = r["messages"][2]["content"]
            # -- cost authority reconciliation: the stated cost must be recomputable
            #    from the row's own state, and the prompt must state it verbatim
            if "venue:" not in u or CA.regime_for(m.get("venue")) not in CA.REGIMES:
                bad_cost += 1
            else:
                mark_px = float(RE_MARK.search(u).group(1))
                clip = m["qty_tokens_at_decision"] * mark_px
                recomputed = CA.decompose(clip, m["depth_sol"],
                                          CA.regime_for(m["venue"]))["round_trip_bp"]
                if recomputed != _ca(m)["round_trip_bp"]:
                    bad_cost += 1
                if f"round trip {_ca(m)['round_trip_bp']} bp" not in u:
                    bad_cost += 1
                if _ca(m)["statement"] not in u:
                    bad_cost += 1
            if m["depth_sol"] <= 0:
                bad_cost += 1
            if m["action"] not in ACTIONS:
                bad_action += 1
            if f"DECISION: {m['action']}" not in ass:
                bad_mismatch += 1
            inv = float(RE_INV.search(u).group(1))
            cash = float(RE_CASH.search(u).group(1))
            mark = float(RE_MARK.search(u).group(1))
            entry = float(RE_ENTRY.search(u).group(1))
            upnl = float(RE_UPNL.search(u).group(1))
            held = float(RE_HELD.search(u).group(1))
            if inv <= 0:
                n_flat += 1
            else:
                qty_ok += 1
            if abs((mark / entry - 1.0) * 1e4 - upnl) > 0.05:
                bad_upnl += 1
            # Retired CONTEXTS, not bare numbers: a row whose own calibrated round trip
            # happens to be 210 bp is legitimate. What must never reappear is a
            # hand-typed cost claim or the retired decomposition.
            if ("fees + slippage + priority + failure charge" in u
                    or "one-way execution cost" in u or "~180 bp" in u
                    or "Round-trip cost (" in u or "cost_model_bp" in u
                    or "bp round trip (pool depth" in u):
                retired_cost += 1
            if r["messages"][0]["content"] != EXPECTED_SYSTEM:
                bad_system += 1
            if "lamports per raw token" not in u:
                wrong_units += 1
            for v in (inv, cash, mark, entry, upnl):
                if not np.isfinite(v):
                    nonfinite += 1
            if m["t_entry_ms"] > m["decision_time_unix_ms"] or m["held_ms_at_decision"] <= 0:
                causality_bad += 1
            if held * 1000 != m["held_ms_at_decision"]:
                causality_bad += 1
            margins.append(m["label_margin"])
            upnls.append(upnl)
            steps[m["step"]] += 1
        margins = np.asarray(margins)
        upnls = np.asarray(upnls)
        stats = {
            "rows": len(rows), "c8_rows": c8_counts[s],
            "retention_pct": round(100.0 * len(rows) / max(1, c8_counts[s]), 2),
            "zero_inventory_rows": n_flat,
            "action_not_in_action_space": bad_action,
            "assistant_decision_mismatch": bad_mismatch,
            "pnl_arithmetic_mismatch": bad_upnl,
            "cost_authority_violations": bad_cost,
            "system_prompt_not_from_authority": bad_system,
            "retired_cost_or_unit_string": retired_cost,
            "missing_lamports_unit_label": wrong_units,
            "nonfinite_state_field": nonfinite,
            "causality_violations": causality_bad,
            "label_distribution": dict(Counter(r["meta"]["action"] for r in rows)),
            "label_margin": {"min": float(margins.min()), "p10": float(np.percentile(margins, 10)),
                             "median": float(np.median(margins)),
                             "exact_tie_rows": int((margins <= 0.0).sum()),
                             "below_1e-6": int((margins < 1e-6).sum())},
            "upnl_bp": {"min": round(float(upnls.min()), 1),
                        "p25": round(float(np.percentile(upnls, 25)), 1),
                        "median": round(float(np.median(upnls)), 1),
                        "p75": round(float(np.percentile(upnls, 75)), 1),
                        "max": round(float(upnls.max()), 1)},
            "steps": dict(sorted(steps.items(), key=lambda kv: int(kv[0]))),
        }
        report[s] = stats
        for k in ("zero_inventory_rows", "cost_authority_violations",
                  "system_prompt_not_from_authority",
                  "action_not_in_action_space",
                  "assistant_decision_mismatch", "pnl_arithmetic_mismatch",
                  "retired_cost_or_unit_string", "missing_lamports_unit_label",
                  "nonfinite_state_field", "causality_violations"):
            if stats[k]:
                failures.append(f"{s}:{k}={stats[k]}")
        if stats["label_margin"]["exact_tie_rows"]:
            failures.append(f"{s}:exact_tie_rows={stats['label_margin']['exact_tie_rows']}")
        if stats["retention_pct"] < 95.0:
            failures.append(f"{s}:retention={stats['retention_pct']}%")

    # ---- independent re-scoring of a random sample -------------------------
    rng = random.Random(20260915)
    pool = [r for r in all_rows["train"] if rng.random() < 0.02]
    rng.shuffle(pool)
    sample = pool[:a.sample]
    mints = {r["meta"]["mint"] for r in sample}
    tapes, full = load_tape(TRADES, mints)
    agree = 0
    disagree = []
    for r in sample:
        m = r["meta"]
        t = tapes.get(m["mint"])
        fs = full.get(m["mint"])
        if t is None or fs is None:
            continue
        tt_all, px_all = fs
        jf = int(np.searchsorted(tt_all, m["t_entry_ms"], side="left"))
        if jf >= tt_all.size or int(tt_all[jf]) != m["t_entry_ms"]:
            disagree.append({"mint": m["mint"], "reason": "entry_fill_missing"})
            continue
        entry = {"entry_px": float(px_all[jf]), "t_entry_ms": m["t_entry_ms"],
                 "qty_tokens": m["qty_tokens_at_decision"], "cash_sol": m["cash_sol_at_decision"],
                 "capital_sol": m["capital_sol"]}
        # c12: the horizon is row-specific under horizon_policy=remaining_to_entry_barrier,
        # so the independent re-score reads the row's OWN recorded horizon. Grading the
        # aligned labels on a fixed 300 s clock would re-introduce the retired defect.
        h_ms = int(m["management_horizon_ms"])
        cfg = RewardConfig(lambda_exposure=0.08884, horizon_ms=h_ms)
        dec = score_decision(t, m["decision_time_unix_ms"], family="management", cfg=cfg,
                             entry=entry, horizon_ms=h_ms)
        ok = {k: v["reward"] for k, v in dec["per_action"].items() if v["status"] == "ok"}
        if len(ok) != 4:
            disagree.append({"mint": m["mint"], "reason": "candidate_refused"})
            continue
        best = max(("HOLD", "REDUCE", "EXIT", "ADD"), key=lambda k: ok[k])
        if best == m["action"]:
            agree += 1
        else:
            disagree.append({"mint": m["mint"], "step": m["step"], "label": m["action"],
                             "rescored": best})
    report["rescore_sample"] = {"tried": len(sample), "agree": agree,
                                "disagree": len(disagree), "detail": disagree[:5]}
    if disagree:
        failures.append(f"rescore_disagreement={len(disagree)}/{len(sample)}")

    report["gates_failed"] = failures
    print(json.dumps(report, indent=1))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())