#!/usr/bin/env python3
"""Generate M9 management-magnitude parity fixtures by DRIVING THE REAL ENGINE.

Why this exists: the corpus (the label authority) sizes a management action off a
different base per action — ADD off the ACCOUNT's capital, REDUCE/EXIT off the
CURRENT position — while the serving seam resolved ADD off inventory, a ~4x error
on the highest-stakes action. A parity test whose expected numbers are written by
hand would pass by agreeing with itself, so this script runs the actual simulator
(`score_action_sequence` in continuation mode) and records what IT executed.

Emitted per row:
    {action, inventory_value_lamports, account_capital_lamports,
     free_cash_lamports, python_clip_lamports}

Run: /home/alon/qwen27b-venv/bin/python gen_mgmt_parity_fixtures.py
"""
import json
import pathlib
import sys

import numpy as np

RL = "/training/v2/code/src/v2/rl"
sys.path.insert(0, RL)

from reward_engine import Tape, score_action_sequence  # noqa: E402

# px must land at canonical magnitudes (~1.36e-3 SOL/token) or the engine's own
# sanity guards reject the episode as a unit mix-up.
PX_SOL_PER_TOKEN = 1.36e-3
SOL = 1_000_000_000


def build_tape(mint="PARITY_MINT"):
    """A short, causally ordered tape at real price magnitude, with a mid-price drift."""
    tt = [1_000_000, 1_030_000, 1_060_000, 1_090_000, 1_120_000,
          1_150_000, 1_180_000, 1_210_000]
    sol = []
    tok = []
    for i, _ in enumerate(tt):
        px = PX_SOL_PER_TOKEN * (1.0 + 0.01 * i)      # gentle drift upward
        t = 1_000_000
        sol.append(int(round(px * t)))
        tok.append(t)
    venue = ["pumpfun"] * len(tt)
    side = ["buy" if i % 2 == 0 else "sell" for i in range(len(tt))]
    return Tape(mint, tt, sol, tok, venue, side)


def make_case(tape, action, qty_tokens, cash_sol, entry_idx=0, dec_idx=2):
    """One held-position continuation case; returns the engine's executed clip."""
    entry_px = float(tape.px[entry_idx])
    t_entry = int(tape.tt[entry_idx])
    t_dec = int(tape.tt[dec_idx])
    capital = cash_sol + qty_tokens * entry_px
    entry = {
        "entry_px": entry_px,
        "t_entry_ms": t_entry,
        "qty_tokens": qty_tokens,
        "cash_sol": cash_sol,
        "capital_sol": capital,
    }
    res = score_action_sequence(
        tape, t_dec, [action], engine=None, entry=entry,
        horizon_ms=180_000, interval_ms=30_000, min_hold_ms=0,
    )
    # The executed leg IS the engine's own clip for the action.
    legs = res.get("legs") or []
    executed = None
    for l in legs:
        if l["action"] == action:
            executed = l
            break
    if res.get("status") != "ok":
        raise SystemExit(f"engine refused the {action} case: {res.get('status')} {res.get('violations')}")
    if executed is None:
        raise SystemExit(f"engine executed no {action} leg (rows={legs})")
    return {
        "action": action,
        "inventory_value_lamports": int(round(qty_tokens * entry_px * SOL)),
        "account_capital_lamports": int(round(capital * SOL)),
        "free_cash_lamports": int(round(cash_sol * SOL)),
        "python_clip_lamports": int(round(executed["sol"] * SOL)),
        "_note": f"engine executed {action} leg sol={executed['sol']:.9f}",
    }


def main():
    # ADD only: for a BUY leg the executed `sol` IS the clip. For REDUCE/EXIT the
    # engine reports NET PROCEEDS (after fees), not the clipped notional, so a
    # lamport clip comparison there would encode a wrong expectation; those two
    # actions are covered by the Rust unit tests instead.
    tape = build_tape()
    entry_px = float(tape.px[0])

    cases = [
        # (label, position_sol, cash_sol) -- capital-limited vs cash-limited
        ("capital_limited", 0.25, 0.76),      # 0.5*cap=0.505 < cash 0.76 -> capital binds
        ("cash_limited", 0.25, 0.10),         # 0.5*cap=0.175 > cash 0.10 -> CASH binds
        ("cash_limited_tight", 0.10, 0.20),   # 0.5*cap=0.150 < cash 0.20 -> capital binds
    ]

    rows = []
    for label, position_sol, cash_sol in cases:
        qty = position_sol / entry_px
        r = make_case(tape, "ADD", qty, cash_sol)
        r["case"] = label
        rows.append(r)

    out = pathlib.Path(__file__).resolve().parent.parent / "tests/fixtures/management_parity.jsonl"
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as f:
        for r in rows:
            f.write(json.dumps(r, sort_keys=True) + "\n")

    print(f"px={entry_px:.6e} SOL/token (canonical magnitude)")
    for r in rows:
        print(f"  {r['case']:28s} clip={r['python_clip_lamports']:>12d} lamports  "
              f"(inv={r['inventory_value_lamports']}, cap={r['account_capital_lamports']}, "
              f"cash={r['free_cash_lamports']})")
    print(f"\nwrote {out} ({len(rows)} rows)")


if __name__ == "__main__":
    main()
