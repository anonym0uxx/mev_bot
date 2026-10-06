#!/usr/bin/env python
"""JOINT entry x exit scoring + coherence tests.

THE FAILURE MODE THIS GUARDS (operator, 2026-09-13): an entry policy and an exit
policy can each look excellent and be worthless TOGETHER. Ways that happens:

  * different horizons   - entry judged on a 30 min move, exit tuned on a 5 min one
  * different cost model - the round trip charged twice, or charged once with
                           different floors, so the joint expectancy is a fiction
  * inexecutable pairing - entry graded against an exit the model cannot hold
                           (our bridge still maps BUY -> ["EXIT"], an instant flip)
  * basis mismatch       - the exit is scored from the MARK instead of the actual
                           entry fill, so a continuation is really a new position
  * independent ranking  - entry chosen by its own score, exit by its own, and the
                           product is worse than either alone

So this module scores the PAIR on ONE episode: one horizon, one cost model, one
tick grid, one reward, with the exit scored as a CONTINUATION of the actual fill.

It also answers the question that decides whether joint learning is needed at all:
if the best exit policy varies per decision, a single fixed exit cannot realise
the entry edge and the two must be learned together.

Run: /home/alon/qwen27b-venv/bin/python joint_entry_exit.py [--label BUY]
"""
from __future__ import annotations

import argparse
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_engine import load_canonical_tapes, RewardEngine, LAMPORTS_PER_SOL
from reward_terms import reward_scalar, counterfactual_advantages
from exit_policy import TEMPLATES, simulate_policy
from exit_mechanics import cost_floor_bps

TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
CORPUS = "/training/v2/candidate_sft_c3/train.jsonl"
ENTRY_POLICIES = ("TRAIL_ONLY", "MOONSHOT_TAIL", "HOLD_TO_HORIZON")
MAX_MINTS = 600
DECS_PER_MINT = 4


FEAT_RE = None


def prompt_features(rec):
    """Causal features straight from the corpus prompt (what the model actually sees)."""
    import re
    txt = ""
    for m in rec.get("messages") or []:
        if m.get("role") == "user":
            txt = m.get("content") or ""
            break
    def g(name, cast=float):
        mm = re.search(rf"{name}=([-0-9.eE+]+)", txt)
        try:
            return cast(mm.group(1)) if mm else None
        except (TypeError, ValueError):
            return None
    return {"vol_30s_bp": g("vol_30s_bp"), "ret_30s_bp": g("ret_30s_bp"),
            "age_s": g("age_s"), "net_flow_lamports": g("net_flow_lamports"),
            "buyer_seller_ratio": g("buyer_seller_ratio")}


def record_label(rec):
    for m in rec.get("messages") or []:
        if m.get("role") == "assistant":
            t = (m.get("content") or "").upper()
            for lab in ("BUY", "WATCH", "SKIP"):
                if f"DECISION: {lab}" in t or f"DECISION:{lab}" in t:
                    return lab
    return None


def joint_grid(tape, t_dec, engine, cfg_floor):
    """One decision -> {exit_policy: reward} for ENTERING, plus the skip value 0.0."""
    out = {}
    for p in ENTRY_POLICIES:
        ep = simulate_policy(tape, t_dec, TEMPLATES[p], engine=engine,
                             refuse_cross_graduation=False)
        r = reward_scalar(ep)
        out[p] = {"reward": r["reward"], "refused": r["refused"],
                  "status": r["status"], "net_sol": ep.get("net_sol_returned"),
                  "exit_reason": ep.get("exit_reason")}
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--label", default=None, help="restrict to BUY/WATCH/SKIP")
    ap.add_argument("--max-mints", type=int, default=MAX_MINTS)
    a = ap.parse_args()

    tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000,
                                 max_px_ratio=50)
    want = {}
    prompt_features_by_mint = {}
    for line in open(CORPUS, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("family") != "decision":
            continue
        lab = record_label(r)
        if a.label and lab != a.label:
            continue
        parts = (r.get("episode_id") or "").split(":")
        if len(parts) < 3:
            continue
        try:
            td = int(parts[2])
        except ValueError:
            continue
        want.setdefault(parts[1], []).append((td, lab))
        prompt_features_by_mint[(parts[1], td)] = prompt_features(r)
        if len(want) >= a.max_mints:
            break

    engine = RewardEngine()
    floor = cost_floor_bps("amm", notional_sol=1.0, expected_impact_bps=50)
    rows = []
    label_mix = {}
    for mint, decs in want.items():
        tape = tapes.get(mint)
        if tape is None:
            continue
        t0, tN = int(tape.tt[0]), int(tape.tt[-1])
        inwin = [(t, l) for t, l in decs if t0 <= t <= tN]
        if not inwin:
            continue
        step = max(1, len(inwin) // DECS_PER_MINT)
        for t_dec, lab in inwin[::step][:DECS_PER_MINT]:
            label_mix[lab] = label_mix.get(lab, 0) + 1
            g = joint_grid(tape, t_dec, engine, floor)
            usable = {k: v for k, v in g.items() if v["status"] == "ok"}
            if not usable:
                continue
            vals = {k: v["reward"] for k, v in usable.items()}
            best_p = max(vals, key=vals.get)
            rows.append({"mint": mint, "t_dec": t_dec, "label": lab,
                         "feat": prompt_features_by_mint.get((mint, t_dec), {}),
                         "vals": vals, "best_exit": best_p,
                         "best": max(vals.values()),
                         "ref_exit": vals.get("TRAIL_ONLY"),
                         "spread": max(vals.values()) - min(vals.values())})

    if len(rows) < 50:
        print(json.dumps({"verdict": "INSUFFICIENT", "n": len(rows),
                          "label_mix": label_mix}, indent=1))
        return 1

    # entry value under a FIXED reference exit vs under the best exit
    ref = np.asarray([r["ref_exit"] for r in rows], dtype=float)
    best = np.asarray([r["best"] for r in rows], dtype=float)
    # how often does the best exit differ from the fixed one?
    best_choice = np.asarray([r["best_exit"] for r in rows])
    best_rate = float((best_choice == "TRAIL_ONLY").mean())
    # rank agreement between the two ways of scoring the SAME entry decision
    ra = np.argsort(np.argsort(ref)); rb = np.argsort(np.argsort(best))
    rank_corr = float(np.corrcoef(ra, rb)[0, 1]) if ref.size > 2 else 0.0

    out = {
        "decisions": len(rows), "label_mix": label_mix,
        "cost_floor_bps_used": floor,
        "entry_value_fixed_exit_TRAIL": {
            "mean": round(float(ref.mean()), 5),
            "median": round(float(np.median(ref)), 5),
            "p_positive": round(float((ref > 0).mean()), 4)},
        "entry_value_best_exit": {
            "mean": round(float(best.mean()), 5),
            "median": round(float(np.median(best)), 5),
            "p_positive": round(float((best > 0).mean()), 4)},
        "best_exit_variability": {
            "frac_TRAIL_ONLY_is_best": round(best_rate, 4),
            "frac_other_exit_is_best": round(1.0 - best_rate, 4),
            "mean_spread_across_exits": round(float(np.mean([r["spread"] for r in rows])), 5)},
        "rank_corr_fixed_vs_best": round(rank_corr, 4),
        "joint_learning_required": bool(best_rate < 0.85 or rank_corr < 0.95),
        "verdict": None,
    }
    if out["entry_value_best_exit"]["mean"] > 0:
        out["verdict"] = ("JOINT ENTRY EDGE PRESENT: entering pays once paired with an "
                          "executable exit policy")
    else:
        out["verdict"] = ("NO JOINT EDGE on this sample: even the best executable exit "
                          "does not make entering pay at these decisions")
    # CONDITIONALITY: does the best exit change with the causal market state?
    def bucket(vals, lo, hi):
        return [i for i, v in enumerate(vals) if v is not None and lo <= v < hi]
    cond = {}
    vols = [r["feat"].get("vol_30s_bp") for r in rows]
    good = [v for v in vols if v is not None]
    if len(good) >= 30:
        import statistics as st
        q1, q2 = st.quantiles(good, n=3)[0], st.quantiles(good, n=3)[1]
        for name, idxs in (("low_vol", bucket(vols, -1e18, q1)),
                           ("mid_vol", bucket(vols, q1, q2)),
                           ("high_vol", bucket(vols, q2, 1e18))):
            if len(idxs) < 15:
                continue
            be = [rows[i]["best_exit"] for i in idxs]
            el = [rows[i]["best"] for i in idxs]
            cond[name] = {"n": len(idxs),
                          "frac_TRAIL_best": round(be.count("TRAIL_ONLY") / len(be), 3),
                          "frac_MOONSHOT_best": round(be.count("MOONSHOT_TAIL") / len(be), 3),
                          "frac_HOLD_best": round(be.count("HOLD_TO_HORIZON") / len(be), 3),
                          "mean_best_entry_value": round(float(np.mean(el)), 5)}
    rets = [r["feat"].get("ret_30s_bp") for r in rows]
    for name, idxs in (("ret_30s_negative", bucket(rets, -1e18, 0.0)),
                       ("ret_30s_positive", bucket(rets, 0.0, 1e18))):
        if len(idxs) < 15:
            continue
        be = [rows[i]["best_exit"] for i in idxs]
        el = [rows[i]["best"] for i in idxs]
        cond[name] = {"n": len(idxs),
                      "frac_TRAIL_best": round(be.count("TRAIL_ONLY") / len(be), 3),
                      "frac_MOONSHOT_best": round(be.count("MOONSHOT_TAIL") / len(be), 3),
                      "frac_HOLD_best": round(be.count("HOLD_TO_HORIZON") / len(be), 3),
                      "mean_best_entry_value": round(float(np.mean(el)), 5)}
    out["best_exit_by_state"] = cond
    out["state_conditionality_present"] = bool(
        len(cond) >= 3 and len({round(c["frac_TRAIL_best"], 2) for c in cond.values()}) > 1)
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())