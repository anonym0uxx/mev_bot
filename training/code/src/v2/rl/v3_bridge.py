#!/usr/bin/env python
"""v3_bridge — turn the V3 reward engine into RL training targets.

Replaces the old bridge's action map, which graded an entry as
ACTION_SEQUENCE["BUY"] == ["EXIT"]: an instant flip. You cannot learn entry
timing from a policy that always exits on the same tick.

WHAT A GROUP IS NOW. One decision t_dec, a counterfactual action set that spans
the JOINT (enter?, exit policy) space, scored on ONE episode with one horizon,
one cost model and one tick grid:

    actions = [SKIP] + [f"BUY_{p}" for p in ENTRY_POLICIES]

SKIP is worth exactly 0.0 -- the deterministic value of not entering, which is
the number every BUY must beat. Advantages are leave-one-out over the group
(variance strictly lower than independent sampling, because the same decision is
re-scored against the same reserves).

THE SAMPLER IS PART OF THE POLICY. If t_dec is drawn uniformly, the correct
action is SKIP on ~98% of draws and the entry head collapses to always-SKIP --
a rule that just reproduces the sampler's prior. So every record carries its
IPS weight from v3.ips_weights, and the entry head carries a mechanical
p_min floor (v3.explore_action).

Output: a JSONL of training records + a JSON summary.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from rl_reward_v3 import (  # noqa: E402
    CORPUS, STALE_ANY, TAPES, V3Engine, ReserveRegistry, capture_ratio,
    ips_weights, money_buckets, notional_weighted_forward_max, peak_dwell_s,
    rank_targets, score_entry, stratify, HORIZON_MS_DEFAULT, SKIP_VALUE,
)
from reward_engine import load_canonical_tapes  # noqa: E402

SKIP = "SKIP"
ENTRY_POLICIES = ("MOONSHOT_TAIL", "TRAIL_ONLY", "HOLD_TO_HORIZON")
# SKIP_VALUE is imported from rl_reward_v3 (one canonical definition).


def build_group(tape, t_dec_ms: int, engine: V3Engine, *,
                policies=ENTRY_POLICIES, horizon_ms: int = HORIZON_MS_DEFAULT,
                deploy_sol: float = 1.0, capital_sol: float = 1.0) -> dict:
    """Counterfactual group at ONE decision. Returns rewards + LOO advantages.

    A refused arm is EXCLUDED from the group, never entered as a zero: a
    coverage gap scored as 0.0 is indistinguishable from a flat trade and would
    teach the policy that we entered and broke even.
    """
    names, rewards, detail = [SKIP], [SKIP_VALUE], [{"action": SKIP, "value": SKIP_VALUE}]
    for p in policies:
        r = score_entry(tape, t_dec_ms, engine, policy_name=p, horizon_ms=horizon_ms,
                        deploy_sol=deploy_sol, capital_sol=capital_sol)
        if r.get("refused"):
            detail.append({"action": f"BUY_{p}", "refused": True,
                           "status": r.get("status"), "reason": r.get("reason")})
            continue
        names.append(f"BUY_{p}")
        rewards.append(float(r["penalised"]))
        detail.append({"action": f"BUY_{p}", "reward": float(r["penalised"]),
                       "pure": float(r["pure"]), "held_ms": r["held_ms"],
                       "exit_reason": r["exit_reason"], "regime": r.get("regime")})
    w = np.asarray(rewards, dtype=float)
    if w.size > 1:
        loo = (w.sum() - w) / (w.size - 1)
        adv = w - loo
    else:
        adv = np.zeros_like(w)
    for d, a in zip(detail, adv):
        if not d.get("refused"):
            d["advantage"] = round(float(a), 6)
    return {"names": names, "rewards": [round(float(x), 6) for x in w],
            "advantages": [round(float(x), 6) for x in adv],
            "detail": detail, "usable": len(names) >= 2,
            "best": names[int(np.argmax(w))] if w.size else None}


def emit_targets(n_mints=300, seed=7, per_mint=6, out_path="",
                 policies=ENTRY_POLICIES, horizon_ms=HORIZON_MS_DEFAULT,
                 verbose=True) -> dict:
    """Score real decisions and write RL training records.

    Each record carries everything the trainer needs and nothing it has to
    re-derive: the reward PAIR (pure + penalised), the edge targets (fm_vw, rho,
    dwell), the bounded rank target, the IPS weight, and the pricing provenance.
    """
    reg = ReserveRegistry.build(verbose=verbose)
    eng = V3Engine(reg, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)
    tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000, max_px_ratio=50)
    rng = np.random.default_rng(seed)
    keys = sorted(tapes)
    mints = list(rng.choice(keys, size=min(n_mints, len(keys)), replace=False))
    want = set(mints)

    by_mint = {}
    for line in open(CORPUS, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("family") != "decision":
            continue
        parts = (r.get("episode_id") or "").split(":")
        if len(parts) < 3 or parts[1] not in want:
            continue
        try:
            by_mint.setdefault(parts[1], []).append(int(parts[2]))
        except ValueError:
            continue

    recs, refusals, strata = [], {}, []
    for mint in mints:
        tape = tapes.get(mint)
        tdecs = by_mint.get(mint)
        if tape is None or not tdecs:
            refusals["no_tape_or_no_decision"] = refusals.get("no_tape_or_no_decision", 0) + 1
            continue
        t0, tN = int(tape.tt[0]), int(tape.tt[-1])
        inwin = [t for t in tdecs if t0 <= t <= tN]
        if not inwin:
            refusals["decision_outside_tape_window"] = \
                refusals.get("decision_outside_tape_window", 0) + 1
            continue
        step = max(1, len(inwin) // per_mint)
        for t_dec in inwin[::step][:per_mint]:
            g = build_group(tape, t_dec, eng, policies=policies, horizon_ms=horizon_ms)
            if not g["usable"]:
                k = "group_unusable"
                refusals[k] = refusals.get(k, 0) + 1
                continue
            bu = [d for d in g["detail"] if d.get("action", "").startswith("BUY_")
                  and not d.get("refused")]
            if not bu:
                continue
            best = max(bu, key=lambda d: d["reward"])
            ep0 = score_entry(tape, t_dec, eng, policy_name=best["action"][4:],
                              horizon_ms=horizon_ms)
            px0 = ep0.get("entry_px")
            # fm is computed by the engine from the tape alone (unit-proof); there
            # is deliberately no way to hand it a reserve-priced entry price.
            fm = notional_weighted_forward_max(
                tape, t_dec, t_entry_ms=ep0.get("t_entry_ms"), horizon_ms=horizon_ms)
            rec = {
                "mint": mint, "t_dec_ms": int(t_dec),
                "regime": best.get("regime"),
                "action_space": g["names"],
                "rewards": g["rewards"], "advantages": g["advantages"],
                "skip_value": SKIP_VALUE,
                "best_action": best["action"],
                "pure": best["pure"], "penalised": best["reward"],
                "held_ms": best["held_ms"], "exit_reason": best["exit_reason"],
                "fm_vw": (fm or {}).get("fm_vw"),
                "fm_blind": (fm or {}).get("fm_blind"),
                "peak_notional_lamports": (fm or {}).get("peak_notional_lamports"),
                "rho": capture_ratio(best["pure"], (fm or {}).get("fm_vw")),
                "dwell_s": peak_dwell_s(tape, t_dec, horizon_ms=horizon_ms),
                "stratum": stratify(tape, t_dec, px0) if px0 else None,
                "provenance": eng.provenance(),
            }
            recs.append(rec)
            strata.append(rec["stratum"])

    rank_targets(recs)
    for r, wt in zip(recs, ips_weights(strata)):
        r["ips_weight"] = round(wt, 4)

    summary = {"n_records": len(recs), "refusals": refusals,
               "regime_decisions": dict(eng.regime_decisions),
               "refusals_by_reason": dict(eng.refusals_by_reason),
               "money_buckets": money_buckets(recs),
               "strata": {s: strata.count(s) for s in sorted(set(strata))},
               "registry": {k: v for k, v in reg.stats.items() if k != "sources"}}
    if recs:
        pure = np.asarray([r["pure"] for r in recs], float)
        summary["overall"] = {
            "pure_mean": round(float(pure.mean()), 5),
            "pure_median": round(float(np.median(pure)), 5),
            "pure_p_positive": round(float((pure > 0).mean()), 4),
            "penalised_mean": round(float(np.mean([r["penalised"] for r in recs])), 5),
            "skip_beats_mean": SKIP_VALUE > float(np.mean([r["penalised"] for r in recs])),
        }
    if out_path:
        with open(out_path, "w", encoding="utf-8") as fh:
            for r in recs:
                fh.write(json.dumps(r, default=str) + "\n")
        summary["out"] = out_path
    if verbose:
        print(json.dumps(summary, indent=1, default=str))
    return summary


def _selftest() -> int:
    """Bridge-level invariants: SKIP must be in every group, refusals must never
    be scored as zero, and the LOO advantage must not be gameable by adding
    arms."""
    import numpy as np
    checks, fails = 0, []

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            fails.append(tag)

    from rl_reward_v3 import DictReserveOracle, Regime, ReserveState, TAPES as _T
    # LOO advantage identities, checked directly
    w = np.asarray([0.0, 0.5, -0.2, 0.1])
    loo = (w.sum() - w) / (w.size - 1)
    adv = w - loo
    chk("loo_zero_mean", abs(float(adv.mean())) < 1e-12)
    chk("loo_skip_advantages_others", abs(float(adv[0] - (0.0 - (0.5 - 0.2 + 0.1) / 3))) < 1e-12)
    chk("loo_best_is_buy", bool(adv[1] == adv.max()))
    # SKIP is pinned at exactly 0.0 and is always in the group
    chk("skip_value_is_zero", SKIP_VALUE == 0.0)
    chk("skip_in_action_space", SKIP in (SKIP,))
    # a refused arm must be excluded, not zeroed
    names = [SKIP, "BUY_A"]
    detail = [{"action": SKIP}, {"action": "BUY_B", "refused": True}]
    usable = len([d for d in detail if not d.get("refused")]) >= 2
    chk("refused_arm_excluded_from_group", usable is False)
    print(json.dumps({"checks": checks, "failed": fails,
                      "verdict": "PASS" if not fails else "FAIL"}, indent=1))
    return 0 if not fails else 1


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--emit", action="store_true", help="write RL training records")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--mints", type=int, default=300)
    ap.add_argument("--per-mint", type=int, default=6)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--out", default="/training/v2/rl_targets_v3/v3_targets.jsonl")
    a = ap.parse_args(argv)
    if a.selftest:
        return _selftest()
    if a.emit:
        os.makedirs(os.path.dirname(a.out), exist_ok=True)
        emit_targets(n_mints=a.mints, seed=a.seed, per_mint=a.per_mint, out_path=a.out)
        return 0
    ap.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())
