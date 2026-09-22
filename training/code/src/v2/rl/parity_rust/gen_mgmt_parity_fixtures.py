#!/usr/bin/env python3
"""Generate the M9 management-magnitude parity fixture from the REAL reward engine.

The corpus convention (the reward engine is the single label authority) is what the Rust
serving seam must reproduce, so the fixture is PRODUCED by running
`reward_engine.score_action_sequence` on worked held-position examples and reading the
magnitudes it actually executes. The Rust test then re-derives each magnitude through
`seam::resolve_management_clip_lamports` and must land on the SAME lamports.

Worked state (task spec): a position of qty X opened with a SMALL/0.25 SOL entry, cash C and
account capital 1.01 SOL, at a management decision.

The magnitudes:
  ADD    -- SOL the engine spends          (`min(cash, 0.5 * capital_sol)`; ACCOUNT decision)
  REDUCE -- SOL value of the half it trims (0.5 * inventory, at the decision mark)
  EXIT   -- SOL value of the whole inventory (at the decision mark)

Run:  python3 gen_mgmt_parity_fixtures.py <out.jsonl>
"""
import json
import sys

import numpy as np

sys.path.insert(0, "/training/v2/code/src/v2/rl")
from reward_engine import Tape, RewardEngine, score_action_sequence, LAMPORTS_PER_SOL  # noqa: E402

LAM = int(LAMPORTS_PER_SOL)          # 1e9
CAPITAL_SOL = 1.01                   # ACCOUNT capital (corpus convention)
# tape.px == |sol_lamports| / |tokens| is SOL per token (verified against a real canonical
# tape: px ~ 1.36e-3 for mint 4FqqLmD...pump). Mirror that ratio so depth/cost maths behave.
TOK = 1_000_000_000                  # tokens per fill
SOL_LAM = 1_360_000                  # SOL leg per fill (lamports) -> tape.px == 0.00136
PX0 = SOL_LAM / TOK                  # 0.00136 SOL per token
TICK_MS = 5_000
HOLD_MS = 300_000                    # position age at the decision (>= min_hold_ms)


def build_tape(mint, t_entry, horizon_ms=1_800_000):
    """A causally ordered, flat-price tape long enough for a full horizon."""
    n = horizon_ms // TICK_MS + 4
    tt = [t_entry + i * TICK_MS for i in range(n)]
    return Tape(mint, tt, [SOL_LAM] * n, [TOK] * n,
                ["pumpfun_bonding"] * n, ["buy"] * n)


def magnitude(case, action):
    """Run the real engine for ONE management action; return the executed magnitude in SOL.

    Returns (magnitude_sol, result) where the result carries the engine's own ledger so the
    caller can assert the episode was coherent (no violations).
    """
    t_dec = case["t_entry_ms"] + HOLD_MS
    tape = build_tape(case["mint"], case["t_entry_ms"], case["horizon_ms"])
    entry = {"entry_px": PX0, "t_entry_ms": case["t_entry_ms"],
             "qty_tokens": case["qty_tokens"], "cash_sol": case["cash_sol"],
             "capital_sol": case["capital_sol"]}
    res = score_action_sequence(tape, t_dec, [action], engine=RewardEngine(), entry=entry,
                                horizon_ms=case["horizon_ms"])
    if res.get("status") != "ok":
        raise RuntimeError("engine refused %s/%s: %s" % (case["mint"], action, res.get("status")))
    if res.get("violations"):
        raise RuntimeError("incoherent episode %s/%s: %s"
                           % (case["mint"], action, res["violations"]))
    legs = [l for l in res["legs"] if l["action"] == action]
    if action == "ADD":
        if not legs:
            return 0.0, res           # ADD became a no-op (cash too thin) -> magnitude 0
        return float(legs[0]["sol"]), res
    # REDUCE / EXIT: magnitude is the SOL VALUE of what was sold, marked at the decision.
    mark_sol = float(tape.price_at_or_before(int(tape.tt[0]) + HOLD_MS, t_dec, "parity_mark"))
    qty = case["qty_tokens"]
    frac = 0.5 if action == "REDUCE" else 1.0
    return frac * qty * mark_sol, res


CASES = [
    # a SMALL / 0.25 SOL entry, cash 0.76 -> ADD is NOT cash-capped (the 4x case)
    {"mint": "PARITY_SMALL_UNCAPPED", "entry_sol": 0.25},
    # a SMALL / 0.25 SOL entry with thin cash 0.30 -> ADD IS cash-capped
    {"mint": "PARITY_SMALL_CASHCAPPED", "entry_sol": 0.25, "cash_override": 0.30},
    # a MID / 0.50 SOL entry -> cash 0.51 just clears 0.5*capital=0.505? it does not; the
    # cap binds by 0.005 SOL, which is the point of an integer-lamport check
    {"mint": "PARITY_MID_NEARCAP", "entry_sol": 0.50},
    # a FULL / 1.00 SOL entry would not be affordable at capital 1.01 with the buffer, so the
    # worked set stays inside the corpus's affordable tiers
]


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/management_parity.jsonl"
    rows = []
    for c in CASES:
        case = dict(c)
        case.setdefault("capital_sol", CAPITAL_SOL)
        case.setdefault("horizon_ms", 1_800_000)
        case.setdefault("t_entry_ms", 1_700_000_000_000)
        case.setdefault("cash_override", None)
        entry_sol = float(case.pop("entry_sol"))
        qty = entry_sol / PX0
        cash = (case.pop("cash_override") if case["cash_override"] is not None
                else case["capital_sol"] - entry_sol)
        case["qty_tokens"] = qty
        case["cash_sol"] = cash
        case["entry_sol"] = entry_sol

        inventory_sol = qty * PX0                     # position value at the (flat) mark
        for action in ("ADD", "REDUCE", "EXIT"):
            mag_sol, res = magnitude(case, action)
            rows.append({
                "note": "%s %s entry=%s cash=%.4f" % (case["mint"], action, entry_sol, cash),
                "action": action,
                "account_capital_lamports": int(round(case["capital_sol"] * LAM)),
                "free_cash_lamports": int(round(cash * LAM)),
                "inventory_value_lamports": int(round(inventory_sol * LAM)),
                "qty_tokens": qty,
                "entry_sol": entry_sol,
                "python_clip_lamports": int(round(mag_sol * LAM)),
                "engine_status": res["status"],
                "engine_fills": res["fills"],
            })

    with open(out, "w") as fh:
        for r in rows:
            fh.write(json.dumps(r, sort_keys=True) + "\n")
    print(json.dumps({"written": len(rows), "out": out,
                      "rows": [{k: r[k] for k in ("action", "note", "inventory_value_lamports",
                                                  "free_cash_lamports", "python_clip_lamports")}
                               for r in rows]}, indent=1))


if __name__ == "__main__":
    main()