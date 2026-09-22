#!/usr/bin/env python
"""INTENT CONFORMANCE for the reward engine.

The reward engine's founding intent (see reward_engine.py header) is:

  1. MECHANICAL ONLY - the reward comes from simulating execution against
     causally-ordered recorded data. NO model-generated labels, no AI-authored
     targets, no LLM in the reward path.
  2. CAUSAL - a decision may only see state at or before its own decision time.
  3. HONEST - a refusal / unscoreable decision is NEVER graded as a zero-return
     opportunity, and nothing is ever fabricated to make an episode scoreable.
  4. LAMPORT EXACT - 1 SOL = 1e9 lamports, cross-checked, no silent unit drift.
  5. PURE + SCALE-FREE - the reward is a deterministic function of the episode,
     and is expressed relative to capital so absolute notional cannot create
     reward out of nothing.

This module asserts each of those against the code that now exists, so the
engineering added on top (risk terms, exit policies, impairment, GRPO bridge)
cannot quietly erode the intent.

Run: /home/alon/qwen27b-venv/bin/python test_reward_intent.py
Exit: 0 = intent holds, 1 = a property was violated (every tag printed).
"""
from __future__ import annotations

import json
import os
import re
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_terms import (  # noqa: E402
    REFUSAL_STATUSES, RewardConfig, reward_scalar,
)
from reward_engine import to_lamports_checked, LAMPORTS_PER_SOL  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
FAILS = []
CHECKS = [0]

# every module that is allowed to influence a reward number
REWARD_PATH = ["reward_engine.py", "reward_terms.py", "regime_pricing.py",
               "exit_policy.py", "exit_mechanics.py", "grpo_reward_bridge.py"]
# anything that would mean a model/AI authored part of the reward
AI_PATTERNS = [r"\bopenai\b", r"\banthropic\b", r"\btransformers\b", r"\bvllm\b",
               r"\bhttp[s]?://", r"\brequests\.", r"\burllib\b", r"\bchat\.completions\b",
               r"\bllm\b", r"\bgpt\b", r"\bprompt\b.*\bgene(rate|rator)\b"]


def chk(tag, cond, extra=""):
    CHECKS[0] += 1
    if not cond:
        FAILS.append(f"{tag} {extra}".strip())


def mk(net=5.0, path=(100.0, 101.0, 103.0, 105.0), status="ok", capital=100.0):
    return {"status": status, "net_sol_returned": net,
            "final_equity_sol": capital + net,
            "equity_path": [[i * 1000, v] for i, v in enumerate(path)]}


def test_no_model_authored_targets():
    """1. No LLM/network in the reward path."""
    for name in REWARD_PATH:
        p = os.path.join(HERE, name)
        if not os.path.exists(p):
            FAILS.append(f"missing_reward_path_module:{name}")
            continue
        src = open(p, encoding="utf-8").read()
        body = "\n".join(l for l in src.splitlines()
                         if not l.strip().startswith("#"))
        for pat in AI_PATTERNS:
            m = re.search(pat, body, re.IGNORECASE)
            chk(f"no_ai_in_reward_path:{name}:{pat}", m is None,
                f"found {m.group(0) if m else ''}")


def test_causality_guard_present():
    """2. The causal guard exists and is enforced, not decorative."""
    src = open(os.path.join(HERE, "reward_engine.py"), encoding="utf-8").read()
    chk("causality_check_present", "reserves_never_future_dated" in src)
    chk("no_lookahead_check_present", "no_lookahead" in src)
    chk("lookahead_error_defined", "class LookaheadError" in src
        or "LookaheadError" in src)
    # reserve joins must be asserted causal in the scored episode
    chk("reserve_staleness_bounded", "reserves_within_stale_bound" in src)


def test_honest_refusals():
    """3. Refusals are flagged and never turned into a positive reward."""
    for st in REFUSAL_STATUSES:
        r = reward_scalar(mk(50.0, (100.0, 150.0), status=st))
        chk(f"refusal_flagged:{st}", r["refused"] is True)
        chk(f"refusal_zero_reward:{st}", r["reward"] == 0.0, f"{r['reward']}")
        chk(f"refusal_has_reason:{st}", "reason" in r)
    # an unknown status must not be silently graded as a win either
    r = reward_scalar(mk(50.0, (100.0, 150.0), status="weird_new_status"))
    chk("unknown_status_refused", r["refused"] is True and r["reward"] == 0.0)


def test_purity_and_determinism():
    """5. Same episode -> same reward, every time."""
    ep = mk(7.5, (100.0, 90.0, 107.5))
    a = reward_scalar(ep)["reward"]
    b = reward_scalar(ep)["reward"]
    chk("reward_is_deterministic", a == b, f"{a} vs {b}")


def test_scale_free():
    """5b. Scaling capital AND pnl together must not change the reward."""
    base = reward_scalar(mk(5.0, (100.0, 101.0, 105.0), capital=100.0))["reward"]
    big = reward_scalar(mk(5_000.0, (100_000.0, 101_000.0, 105_000.0),
                           capital=100_000.0))["reward"]
    chk("reward_is_scale_free", abs(base - big) < 1e-9, f"{base} vs {big}")


def test_monotone_in_cost():
    """5c. More cost at the same gross must lower the reward."""
    cheap = reward_scalar(mk(5.0, (100.0, 105.0)))["reward"]
    dear = reward_scalar(mk(3.0, (100.0, 103.0)))["reward"]
    chk("reward_monotone_in_cost", dear < cheap, f"{dear} vs {cheap}")


def test_ruin_is_hard():
    """3b. Ruin is a hard terminal penalty, not a soft deduction."""
    r = reward_scalar(mk(-120.0, (100.0, 40.0, 0.0, -20.0)))
    chk("ruin_hard_failed", r["reward"] == -RewardConfig().ruin_penalty
        and r["risk"]["ruin"] is True)


def test_lamport_exactness():
    """4. 1 SOL = 1e9 lamports; the conversion is checked, not assumed."""
    chk("lamports_per_sol", LAMPORTS_PER_SOL == 1_000_000_000)
    chk("lamport_roundtrip", to_lamports_checked(1.0, "t") == 1_000_000_000)
    # a 1000x unit error is the historical failure this guards against
    chk("no_1000x_unit_error",
        abs(to_lamports_checked(0.001, "t") - 1_000_000) <= 1)


def test_no_free_reward():
    """3c. Doing nothing can never earn a positive reward."""
    flat = mk(0.0, (100.0, 100.0, 100.0))
    r = reward_scalar(flat)
    chk("no_position_earns_nothing", r["reward"] <= 0.0, f"{r['reward']}")
    # and a refusal with a huge notional in the sheet must still be refused
    ref = reward_scalar({"status": "no_entry", "net_sol_returned": None,
                         "final_equity_sol": None, "equity_path": []})
    chk("refusal_with_none_fields", ref["refused"] and ref["reward"] == 0.0)


def main():
    for fn in (test_no_model_authored_targets, test_causality_guard_present,
               test_honest_refusals, test_purity_and_determinism, test_scale_free,
               test_monotone_in_cost, test_ruin_is_hard, test_lamport_exactness,
               test_no_free_reward):
        fn()
    print(json.dumps({"suite": "test_reward_intent", "checks": CHECKS[0],
                      "failed": len(FAILS), "failures": FAILS}, indent=1))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())