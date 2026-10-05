#!/usr/bin/env python
"""Integration check: real recorded tape -> simulator -> risk-adjusted reward ->
counterfactual/group advantages. Proves the reward chain end-to-end on real data
(the small pump-swap derived subset, 91,735 rows / 1,268 mints).

Run: /home/alon/qwen27b-venv/bin/python check_reward_integration.py
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_engine import load_canonical_tapes  # noqa: E402
from reward_terms import (  # noqa: E402
    RewardConfig, episode_reward, group_advantages, counterfactual_advantages,
)
from grpo_reward_bridge import ACTION_SEQUENCE  # noqa: E402

P = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
# The candidate set is taken FROM THE BRIDGE, not retyped here: this check exists to
# prove the reward chain that RL actually grades with, and a locally retyped table is
# exactly how the old `["HOLD","REDUCE","EXIT"]`-labels-REDUCE defect survived review.
GROUPS = [ACTION_SEQUENCE[a] for a in ("EXIT", "HOLD", "REDUCE", "ADD")]


def main():
    tapes = load_canonical_tapes(P)
    cand = sorted(tapes.values(), key=lambda t: -t.tt.size)[:4]
    rep = []
    for t in cand:
        t_dec = int(t.tt[0])
        rewards, detail = [], []
        for acts in GROUPS:
            r = episode_reward(t, t_dec, acts, cfg=RewardConfig(),
                               refuse_cross_graduation=False)
            rewards.append(r["reward"])
            detail.append({"actions": acts, "status": r["status"],
                           "refused": r["refused"], "reward": round(r["reward"], 8),
                           "dd_frac": (r["risk"] or {}).get("max_drawdown_frac"),
                           "eq_points": (r["risk"] or {}).get("n_points")})
        rep.append({"mint": t.mint, "fills": int(t.tt.size), "t_dec_ms": t_dec,
                    "per_action": detail,
                    "group_advantages": [round(x, 6) for x in group_advantages(rewards)],
                    "counterfactual_advantages": [round(x, 6)
                                                  for x in counterfactual_advantages(rewards)]})
    ok = all(p["per_action"][0]["status"] in ("ok",) or p["per_action"][0]["refused"]
             for p in rep)
    scoreable = sum(1 for p in rep for d in p["per_action"] if d["status"] == "ok")
    print(json.dumps({"tapes_checked": len(rep), "scoreable_episodes": scoreable,
                      "verdict": "PASS" if (ok and scoreable) else "FAIL",
                      "episodes": rep}, indent=1))
    return 0 if (ok and scoreable) else 1


if __name__ == "__main__":
    sys.exit(main())
