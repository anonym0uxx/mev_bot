#!/usr/bin/env python
"""size_dimension — the ONE definition of the entry SIZE decision.

WHY THIS MODULE EXISTS
The corpus used to state a CONSTANT size: every one of 13,281 BUY rows said
`SIZE: 0.5 SOL`. The prompt carried full pool depth and the format demanded a size
judgement, but nothing ever made one, nothing scored it, and nothing supervised it.
Meanwhile the reward was blind to size on purpose (capital == deploy), so even a
varying label could not have been learned. Both halves are fixed here:

  * the tier set and its fractions of the ACCOUNT  (SIZE_FRACTIONS)
  * the causal cost of each tier at the decision clock (menu_text / quote_cost)
  * the LABEL rule: argmax over {SKIP} u tiers of the size-aware ENTRY reward

CAUSALITY: every cost shown in the menu is a quote taken AT t_dec from reserves at
or before t_dec. No forward information leaks into the affordance.

The label rule does not invent an answer: it runs the same V3 engine the RL reward
uses, at each tier, through a real exit policy, and takes the best. Because the
entry reward now carries the quadratic exposure term, the tiers genuinely differ.
"""
from __future__ import annotations

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

LAMPORTS_PER_SOL = 1_000_000_000

try:
    from exit_mechanics import DEPLOY_SOL_CANONICAL as SIZE_NOTIONAL_SOL
except Exception:                                                # noqa: BLE001
    SIZE_NOTIONAL_SOL = 1.0

SIZE_LABELS = ("SMALL", "MID", "FULL")
SIZE_FRACTIONS = {"SMALL": 0.25, "MID": 0.50, "FULL": 1.00}
SIZE_SOL_TEXT = {k: ("%.2f" % v) for k, v in SIZE_FRACTIONS.items()}
NONE_LABEL = "NONE"


def tier_sol(label: str, notional_sol: float = SIZE_NOTIONAL_SOL) -> float:
    """A tier is a fraction of the TRADED NOTIONAL (canonical 1.0 SOL), never of the
    account-with-fee-buffer: deploying the buffer too leaves zero free cash and
    drives cash negative on the priority fee."""
    return float(SIZE_FRACTIONS[label]) * float(notional_sol)


def quote_cost(engine, tape, t_dec_ms: int, notional_sol: float) -> dict | None:
    """A BUY quote for `notional_sol` at t_dec. Causal: t_dec is the decision clock.

    Returns the round-trip cost in bp for our own size, measured against the real
    reserves (fee sits inside the pool price; slippage is our own impact). Returns
    None when the engine cannot price the size (thin book, stale/absent reserves)
    - never a fabricated number.
    """
    try:
        qb = engine.quote_buy(tape, t_dec_ms, t_dec_ms, float(notional_sol))
    except Exception:
        return None
    if not qb or not qb.get("tokens"):
        return None
    cost_sol = float(qb.get("fee_sol", 0.0)) + float(qb.get("slippage_sol", 0.0)) \
        + float(qb.get("priority_sol", 0.0))
    one_way = cost_sol / max(float(notional_sol), 1e-12)
    return {"notional_sol": float(notional_sol),
            "tokens": float(qb["tokens"]),
            "one_way_bp": one_way * 1e4,
            "round_trip_bp": 2.0 * one_way * 1e4,
            "impact_sol": float(qb.get("slippage_sol", 0.0)),
            "depth_sol": qb.get("depth_sol")}


def menu_text(engine, tape, t_dec_ms: int, notional_sol: float = SIZE_NOTIONAL_SOL) -> str:
    """The SIZE affordance: each offered tier with its own causally measured cost."""
    out = ["SIZE OPTIONS (choose one on BUY; round-trip cost measured at the decision clock):"]
    for lab in SIZE_LABELS:
        n = tier_sol(lab, notional_sol)
        c = quote_cost(engine, tape, t_dec_ms, n)
        if c is None:
            out.append("  %s = %.2f SOL - not priceable at this book depth" % (lab, n))
            continue
        out.append("  %s = %.2f SOL - ~%.0f bp round trip%s" % (
            lab, n, c["round_trip_bp"],
            "" if c.get("depth_sol") is None else " (pool depth %.1f SOL)"
            % c["depth_sol"]))
    out.append("  Size is a judgement: a bigger clip pays more impact in a thinner book.")
    return "\n".join(out)


def label_for(engine, tape, t_dec_ms: int, *,
              policies=None, notional_sol: float = SIZE_NOTIONAL_SOL,
              account_sol: float | None = None,
              horizon_ms: int | None = None) -> dict:
    """The size LABEL for one entry decision: SKIP, or BUY at a tier.

    argmax over {SKIP=0.0} u tiers of the size-aware ENTRY reward, each tier run
    through a real exit policy by the same engine the RL reward uses. Uses lazy
    imports so this module never participates in an import cycle.
    """
    from rl_reward_v3 import simulate_v3, ENTRY_CFG, HORIZON_MS_DEFAULT
    from reward_terms import reward_scalar
    if policies is None:
        from grpo_dataset import BUY_POLICIES as policies  # noqa: PLC0415
    acct = float(account_sol) if account_sol is not None else None
    horizon = int(horizon_ms if horizon_ms is not None else HORIZON_MS_DEFAULT)

    values = {"SKIP": 0.0}
    detail = {}
    for lab in SIZE_LABELS:
        n = tier_sol(lab, notional_sol)
        kw = {} if acct is None else {"capital_sol": acct}
        best = None
        for pol in policies:
            try:
                ep = simulate_v3(tape, t_dec_ms, engine, policy_name=pol,
                                 deploy_sol=n, horizon_ms=horizon, **kw)
            except Exception:                                    # noqa: BLE001
                continue
            # A fatal violation means the episode is incoherent (cash/equity
            # conservation broken). score_entry refuses these; a label must too -
            # never grade a broken episode as if it were a real outcome.
            if ep.get("status") != "ok" or ep.get("fatal"):
                continue
            r = reward_scalar(ep, ENTRY_CFG)["reward"]
            if best is None or r > best["reward"]:
                best = {"reward": float(r), "policy": pol,
                        "net_sol": float(ep["net_sol_returned"]),
                        "exit_reason": ep.get("exit_reason")}
        if best is not None:
            values[lab] = best["reward"]
            detail[lab] = best
    if len(values) < 2:
        return {"decision": None, "size": None, "values": values, "detail": detail,
                "reason": "no_priceable_tier"}
    best_arm = max(values, key=lambda k: values[k])
    if best_arm == "SKIP" or values[best_arm] <= 0.0:
        return {"decision": "SKIP", "size": None, "values": values,
                "detail": detail, "reason": "no tier beats not trading"}
    return {"decision": "BUY", "size": best_arm, "values": values,
            "detail": detail, "reason": "size-aware argmax"}

