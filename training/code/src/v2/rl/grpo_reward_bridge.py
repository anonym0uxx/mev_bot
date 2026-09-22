#!/usr/bin/env python
"""GRPO reward bridge: one SFT-style decision -> mechanical reward per candidate action.

THE PROBLEM THIS SOLVES. A naive RL reward of "simulated PnL of the chosen action"
is exploitable: SKIP/HOLD score 0, so the policy learns to skip forever, and a
BUY-only reward teaches "size up on any positive-EV read". The fix (principal-
engineer review, 2026-09-13) is the COUNTERFACTUAL, which our simulator makes
cheap because it is deterministic given the tape:

    for ONE decision (mint, t_dec_ms) score EVERY candidate action against the
    SAME recorded reserves, then take the advantage of the chosen action relative
    to the other candidates. SKIP is then rewarded when the trade would have
    lost, and punished by the opportunity cost when it would have won.

Everything here is mechanical: no model-authored targets, no future information
(every lookup is guarded by reward_engine's causality checks).

Run: /home/alon/qwen27b-venv/bin/python grpo_reward_bridge.py --self-check
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_terms import (  # noqa: E402
    RewardConfig, counterfactual_advantages, group_advantages, reward_scalar,
)
from exit_policy import TEMPLATES, POLICY_NAMES, simulate_policy  # noqa: E402

# family -> the candidate action set for that prompt kind
DECISION_ACTIONS = ("SKIP", "WATCH", "BUY")
MANAGEMENT_ACTIONS = ("HOLD", "ADD", "REDUCE", "EXIT")
NO_POSITION_ACTIONS = ("SKIP", "WATCH")

# action -> the action SEQUENCE handed to the simulator (entry is implicit).
#
# The FIRST element is the action the label NAMES: at tick 0 (the decision being
# graded) the candidate does what the token says, then the sequence's remaining
# elements apply at the following ticks and default to HOLD past the end. The
# earlier table began REDUCE with HOLD (copied from an illustrative group where
# the list was a trajectory, not a label), so the token and the authority
# disagreed: "REDUCE" was graded as a hold-then-reduce path. All four management
# candidates are now the same length and share tick 0 semantics, which is what
# makes the counterfactual a comparison of ACTIONS.
ACTION_SEQUENCE = {
    "SKIP": [], "WATCH": [],
    "BUY": ["EXIT"],
    "HOLD": ["HOLD"],
    "ADD": ["ADD"],
    "REDUCE": ["REDUCE"],
    "EXIT": ["EXIT"],
}


def candidate_actions(family: str) -> tuple:
    return MANAGEMENT_ACTIONS if family == "management" else DECISION_ACTIONS


def parse_decision(text: str) -> str | None:
    """Extract the DECISION field from a completion. Returns an action or None.

    Accepts 'DECISION: BUY', 'DECISION — skip', '**DECISION:** WATCH'. Exact-match
    on the label; anything unparseable returns None so the caller can refuse
    rather than guess (never grade a guess).
    """
    if not text:
        return None
    m = re.search(r"DECISION\s*[:\-\u2014]\s*\**\s*([A-Za-z_]+)", text)
    if not m:
        return None
    lab = m.group(1).strip().upper()
    return lab if lab in ("BUY", "WATCH", "SKIP", "HOLD", "ADD", "REDUCE", "EXIT") else None


def score_decision(tape, t_dec_ms, family="decision", engine=None,
                   cfg: RewardConfig = RewardConfig(), entry=None,
                   **score_kw) -> dict:
    """Score EVERY candidate action for one decision on the same reserves.

    `entry=None` grades a fresh ENTRY decision. `entry={entry_px,t_entry_ms,
    qty_tokens,cash_sol,capital_sol}` grades a MANAGEMENT decision from a position
    that is already open: every candidate is then valued from the SAME held state,
    so the counterfactual compares actions rather than comparing "buy a new 1 SOL
    position here" against "manage the position we actually hold".

    Returns {action: {reward, refused, status, ...}} plus the advantage vectors.
    A candidate that cannot be scored (no tape / refused) is marked `usable=False`
    and is NOT silently treated as a zero-return option.
    """
    from reward_engine import score_action_sequence
    actions = candidate_actions(family)
    skw = dict(refuse_cross_graduation=False)
    if entry is not None:
        skw["entry"] = entry
    skw.update(score_kw)
    per = {}
    for a in actions:
        seq = ACTION_SEQUENCE.get(a, [])
        if a in NO_POSITION_ACTIONS:
            # no position taken: the mechanical return of doing nothing is 0, but
            # it is NOT a graded outcome - the counterfactual below is what makes
            # it comparable to trading.
            per[a] = {"reward": 0.0, "refused": False, "status": "no_position",
                      "usable": True, "episode": None}
            continue
        ep = score_action_sequence(tape, t_dec_ms, seq, engine=engine, **skw)
        r = reward_scalar(ep, cfg)
        per[a] = {"reward": r["reward"], "refused": r["refused"], "status": r["status"],
                  "usable": r["status"] == "ok", "episode": ep,
                  "components": r["components"], "risk": r["risk"]}
    usable = [a for a in actions if per[a]["usable"]]
    returns = [per[a]["reward"] for a in usable]
    out = {"t_dec_ms": int(t_dec_ms), "family": family, "per_action": per,
           "usable_actions": list(usable), "returns": returns,
           "advantages": {}, "advantage_mode": None,
           "group_usable": len(usable) >= 2 and any(a in ("BUY", "ADD", "REDUCE", "EXIT")
                                                    for a in usable)}
    ret_adj, gweight, cvar_info = _tail_adjust(returns, cfg)
    out["returns_adjusted"] = ret_adj
    out["group_loss_weight"] = gweight
    out["cvar"] = cvar_info
    if out["group_usable"]:
        if len(usable) >= 3:
            adv = counterfactual_advantages(ret_adj)      # preferred: exact LOO
            out["advantage_mode"] = "counterfactual_loo"
        else:
            adv = group_advantages(ret_adj)               # fallback
            out["advantage_mode"] = "group_normalised"
        out["advantages"] = {a: float(v) for a, v in zip(usable, adv)}
    return out


POLICY_CANDIDATES = POLICY_NAMES


def _tail_adjust(returns, cfg):
    """CVaR tail-risk wiring for one counterfactual group.

    Applies (A) `tail_amplified_rewards` -> the returns the ADVANTAGES are computed
    from, and (C) `cvar_group_weight` -> a per-group LOSS MULTIPLIER carried on the
    record. Identity when cfg.lambda_cvar == 0.0 (the pinned default), so enabling
    the tail term is an explicit pin change and never a silent behaviour change.

    Raw `returns` are preserved separately for diagnostics/parity: the adjusted
    vector is ADDITIONAL, not a redefinition.
    """
    from reward_terms import tail_amplified_rewards, cvar_group_weight
    tap = tail_amplified_rewards(returns, cfg)
    return ([float(x) for x in tap["rewards"]], cvar_group_weight(returns, cfg),
            {"applied": bool(tap["applied"]), "alpha": tap.get("alpha"),
             "cvar": tap.get("cvar"), "cvar_z": tap.get("cvar_z"),
             "q_alpha": tap.get("q_alpha"), "n_penalised": tap.get("n_penalised"),
             "max_penalty": tap.get("max_penalty")})


def score_policy_decision(tape, t_dec_ms, entry=None, engine=None,
                          cfg: RewardConfig = RewardConfig(), **sim_kw) -> dict:
    """Counterfactual group over EXIT POLICY templates (TP ladders + trailing stops).

    This is the swing/scalp capability surface: the model chooses a policy spec,
    not a one-dimensional buy/sell. `entry=None` scores a fresh entry decision;
    `entry={entry_px,t_entry_ms,qty_tokens,cash_sol,...}` scores MID-POSITION
    management as a continuation with the existing cost basis, so an exit is judged
    as an exit rather than as a new buy at the current price.

    Adaptivity: the returned `state` fingerprint is causal, so policy behaviour can
    be measured per regime on mints never trained on.
    """
    per = {}
    for name in POLICY_CANDIDATES:
        ep = simulate_policy(tape, t_dec_ms, TEMPLATES[name], engine=engine,
                             entry=entry, **sim_kw)
        r = reward_scalar(ep, cfg)
        per[name] = {"reward": r["reward"], "refused": r["refused"],
                     "status": r["status"], "usable": r["status"] == "ok",
                     "exit_reason": ep.get("exit_reason"), "episode": ep}
    # EXIT_NOW/HOLD_TO_HORIZON are in the template set on purpose: "do nothing" and
    # "dump everything" must be beatable, otherwise a policy is rewarded for churn.
    usable = [n for n in POLICY_CANDIDATES if per[n]["usable"]]
    returns = [per[n]["reward"] for n in usable]
    out = {"t_dec_ms": int(t_dec_ms), "family": "policy", "per_action": per,
           "usable_actions": list(usable), "returns": returns, "advantages": {},
           "advantage_mode": None, "entry_mode": "fresh" if entry is None else "continuation",
           "group_usable": len(usable) >= 2 and any(n not in ("EXIT_NOW",) for n in usable)}
    ret_adj, gweight, cvar_info = _tail_adjust(returns, cfg)
    out["returns_adjusted"] = ret_adj
    out["group_loss_weight"] = gweight
    out["cvar"] = cvar_info
    if out["group_usable"]:
        if len(usable) >= 3:
            adv = counterfactual_advantages(ret_adj)
            out["advantage_mode"] = "counterfactual_loo"
        else:
            adv = group_advantages(ret_adj)
            out["advantage_mode"] = "group_normalised"
        out["advantages"] = {n: float(v) for n, v in zip(usable, adv)}
    st = next((per[n]["episode"].get("state") for n in usable if per[n]["episode"]), None)
    out["state"] = st
    return out


def _self_check() -> int:
    """Deterministic checks with a fake tape-free path: parsing + action mapping."""
    fails = []
    cases = {"DECISION: BUY": "BUY", "DECISION — skip": "SKIP",
             "**DECISION:** WATCH": "WATCH", "DECISION: HODL": None,
             "no decision here": None}
    for txt, want in cases.items():
        got = parse_decision(txt)
        if got != want:
            fails.append(f"parse_decision({txt!r})={got!r} want {want!r}")
    if candidate_actions("management") != MANAGEMENT_ACTIONS:
        fails.append("management_candidates_wrong")
    if candidate_actions("decision") != DECISION_ACTIONS:
        fails.append("decision_candidates_wrong")
    if ACTION_SEQUENCE["SKIP"] or ACTION_SEQUENCE["WATCH"]:
        fails.append("no_position_must_not_trade")
    if ACTION_SEQUENCE["BUY"] != ["EXIT"]:
        fails.append("buy_sequence_wrong")
    # the token must BE the first action of its own sequence (label == authority)
    for tok in MANAGEMENT_ACTIONS:
        seq = ACTION_SEQUENCE[tok]
        if not seq or seq[0] != tok:
            fails.append(f"management_sequence_not_led_by_{tok}:{seq}")
    if len({tuple(ACTION_SEQUENCE[t]) for t in MANAGEMENT_ACTIONS}) != len(MANAGEMENT_ACTIONS):
        fails.append("management_sequences_not_distinct")
    # advantage vectors are pure functions; verify the bridge wires them unchanged
    # LOO of [0.10,-0.02,0.04]: loo=[0.01,0.07,0.04] -> adv=[0.09,-0.09,0.0]
    r = [0.10, -0.02, 0.04]
    if not all(abs(a - b) < 1e-9 for a, b in zip(counterfactual_advantages(r),
                                                 [0.09, -0.09, 0.0])):
        fails.append("loo_vector_wrong")
    print(json.dumps({"suite": "grpo_reward_bridge", "failed": len(fails),
                      "failures": fails}, indent=1))
    return 1 if fails else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true")
    a = ap.parse_args()
    if a.self_check:
        return _self_check()
    ap.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())