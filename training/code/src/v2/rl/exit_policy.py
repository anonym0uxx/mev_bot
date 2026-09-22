#!/usr/bin/env python
"""Exit-policy simulator: TP ladders, trailing stops, invalidation stops.

WHY (operator scope, 2026-09-13): the trading brain must swing-trade and scalp
pump.fun memecoins, not choose between one-dimensional buy/sell. A management
decision is therefore a POLICY SPEC that the simulator executes mechanically
against recorded reserves/tape:

    ExitPolicy(tp_ladder=((gain_bp, fraction), ...),
               trail_activate_bp, trail_distance_bp, stop_bp, ...)

- tp_ladder : scale out `fraction` of the ORIGINAL position each time price
              reaches entry*(1+gain_bp/1e4). Partial sells, real fees/slippage.
- trail      : once price first reaches entry*(1+trail_activate_bp/1e4) the trail
              ARMS; it then sells the whole remainder if price falls
              trail_distance_bp from the running peak (ratchet, never loosens).
- stop       : hard invalidation at entry*(1-stop_bp/1e4).
- move_stop_to_breakeven: after the first rung fills, the stop is moved to entry.

CAUSALITY: triggers are evaluated on decision ticks; every fill executes at the
last recorded fill at/before the tick (never a future price). All the discipline
of reward_engine is kept: lamport round-trip, cash/qty reconciliation, equity>=0.

ADAPTIVITY (operator requirement: find NEW edge, not repeat history): the result
carries a causal `state` fingerprint (regime, age, flow, concentration, curve) and
`exit_reason`, so a policy's behaviour can be measured CONDITIONALLY per regime on
mints never trained on - a policy that only memorised history cannot pass that.

Run: /home/alon/qwen27b-venv/bin/python exit_policy.py --self-check
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass, asdict

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from reward_engine import (  # noqa: E402
    RewardEngine, LAMPORTS_PER_SOL, to_lamports_checked, LookaheadError,
)
from exit_mechanics import protection_level_fp, DEPLOY_SOL_CANONICAL  # noqa: E402

CAPITAL_SOL = 1.0
# No literal: a second definition is exactly how the deploy size drifts apart.
DEPLOY_SOL = DEPLOY_SOL_CANONICAL


@dataclass(frozen=True)
class ExitPolicy:
    name: str
    tp_ladder: tuple = ()
    trail_activate_bp: float = 0.0
    trail_distance_bp: float = 0.0
    stop_bp: float = 0.0
    move_stop_to_breakeven: bool = True
    tick_ms: int = 30_000
    min_hold_ms: int = 60_000
    horizon_ms: int = 1_800_000
    max_ticks: int = 64

    def as_dict(self) -> dict:
        return asdict(self)


# --- the DISCRETE policy templates the RL candidate set is built from ---------
TEMPLATES = {
    "EXIT_NOW": ExitPolicy("EXIT_NOW"),
    "HOLD_TO_HORIZON": ExitPolicy("HOLD_TO_HORIZON", stop_bp=2500.0),
    "SCALP_TIGHT": ExitPolicy("SCALP_TIGHT", tp_ladder=((150.0, 0.5), (300.0, 0.5)),
                              trail_activate_bp=0.0, trail_distance_bp=200.0, stop_bp=400.0),
    "SCALP_LADDER": ExitPolicy("SCALP_LADDER",
                               tp_ladder=((200.0, 0.34), (500.0, 0.33), (1000.0, 0.33)),
                               trail_distance_bp=350.0, stop_bp=500.0),
    "SWING_RUNNER": ExitPolicy("SWING_RUNNER", tp_ladder=((300.0, 0.25), (1200.0, 0.25)),
                               trail_activate_bp=600.0, trail_distance_bp=1200.0,
                               stop_bp=800.0),
    "TRAIL_ONLY": ExitPolicy("TRAIL_ONLY", trail_activate_bp=650.0,
                             trail_distance_bp=500.0, stop_bp=600.0),
    "MOONSHOT_TAIL": ExitPolicy("MOONSHOT_TAIL", tp_ladder=((800.0, 0.2),),
                                trail_activate_bp=2700.0, trail_distance_bp=2000.0,
                                stop_bp=900.0),
}
def validate_policy(policy: ExitPolicy, cost_floor_bps: int = 100,
                    sigma_bps=None, z: float = 1.0) -> list:
    """Mechanical guards a policy must pass before it may be scored or shipped.

    (1) NEGATIVE_RATCHET: if the trail arms at a protection level below
        breakeven + cost floor, the "protection" can only ever exit at a loss -
        it is not protection at all. Measured 2026-09-13: every hand-picked
        trailing template failed this (MOONSHOT_TAIL armed at +400bp with a
        2000bp trail => protection -900bp). Predicted independently by the exit
        derivation ('mandatory: arm >= delta + c_out').
    (2) ARM_BELOW_TRAIL: the same condition stated directly as arm >= trail+floor.
    (3) STOP_INSIDE_NOISE: a stop tighter than z*sigma is a whipsaw generator; it
        must sit outside expected noise. Only checked when sigma is supplied.
    """
    out = []
    if policy.trail_distance_bp > 0:
        arm, trail = float(policy.trail_activate_bp), float(policy.trail_distance_bp)
        peak = 1.0 + arm / 1e4
        level = protection_level_fp(peak, 1.0, trail, policy.stop_bp)
        net_bps = (level - 1.0) * 1e4
        if net_bps < cost_floor_bps:
            out.append(f"NEGATIVE_RATCHET:{policy.name}:protection={net_bps:.0f}bp"
                       f"<floor={cost_floor_bps}bp")
        if arm < trail + cost_floor_bps:
            # Exact requirement: (1+a)(1-t) >= 1+f  =>  a >= (t+f)/(1-t).
            # The naive "arm >= trail + floor" is not enough because the trail is a
            # fraction of an already-raised peak (measured: 600bp arm with a 500bp
            # trail and a 100bp floor lands at +70bp, not +100bp).
            denom = max(1e-9, 1.0 - trail / 1e4)
            need_arm = (trail + cost_floor_bps) / denom
            if arm < need_arm:
                out.append(f"ARM_BELOW_TRAIL:{policy.name}:arm={arm:.0f}"
                           f"<need={need_arm:.0f}")
    if sigma_bps is not None and policy.stop_bp > 0:
        need = z * float(sigma_bps)
        if policy.stop_bp < need:
            out.append(f"STOP_INSIDE_NOISE:{policy.name}:stop={policy.stop_bp:.0f}"
                       f"<z*sigma={need:.0f}")
    return out


# Policies rejected by BOTH our own admissibility math and the exit derivation
# (sub-noise stops, negative ratchets, and - for SCALP_LADDER - writing away
# roughly half the distribution's expectancy). Kept in TEMPLATES as documented
# negative references; excluded from the baseline set the advantage is measured
# against, because a rejected policy must not become a target to match.
REJECTED_POLICIES = ("SCALP_TIGHT", "SCALP_LADDER", "SWING_RUNNER")
BASELINE_POLICIES = tuple(n for n in TEMPLATES if n not in REJECTED_POLICIES)

POLICY_NAMES = tuple(TEMPLATES)


def simulate_policy(tape, t_dec_ms, policy: ExitPolicy, engine: RewardEngine | None = None,
                    entry: dict | None = None, capital_sol: float = CAPITAL_SOL,
                    deploy_sol: float = DEPLOY_SOL,
                    refuse_cross_graduation: bool = True) -> dict:
    """Execute `policy` against the tape. `entry=None` = fresh entry at the first
    fill after t_dec_ms; `entry={entry_px,t_entry_ms,qty_tokens,cash_sol,capital_sol}`
    = CONTINUATION of an existing position (mid-episode management).
    """
    engine = engine or RewardEngine()
    acts = "EXIT_NOW" if policy.name == "EXIT_NOW" else policy.name
    gx = tape.venue_change_index()
    if refuse_cross_graduation and gx is not None and tape.tt[gx] > t_dec_ms:
        return {"status": "refused_cross_graduation", "net_sol_returned": None,
                "mint": tape.mint, "policy": acts,
                "graduation_ts": int(tape.tt[gx])}

    if entry is None:
        j0 = tape.first_fill_after(t_dec_ms, t_dec_ms, "entry")
        if j0 is None:
            return {"status": "no_entry", "net_sol_returned": None, "mint": tape.mint,
                    "policy": acts}
        t_entry = int(tape.tt[j0])
        px0 = float(tape.px[j0])
        if not np.isfinite(px0) or px0 <= 0:
            return {"status": "bad_entry_price", "net_sol_returned": None,
                    "mint": tape.mint, "policy": acts}
        qty = deploy_sol / px0
        cash = capital_sol - deploy_sol
        try:
            qb = engine.quote_buy(tape, t_entry, t_entry, deploy_sol) or {}
        except Exception as e:                                   # noqa: BLE001
            return {"status": "refused_no_reserves", "net_sol_returned": None,
                    "mint": tape.mint, "policy": acts, "reason": str(e)[:300]}
        legs = [("BUY", t_entry, deploy_sol, qb)]
        costs = {"curve_fee_sol": float(qb.get("fee_sol", 0.0)),
                 "slippage_sol": float(qb.get("slippage_sol", 0.0)),
                 "creator_fee_sol": float(qb.get("creator_sol", 0.0)),
                 "priority_sol": float(qb.get("priority_sol", 0.0))}
        qty_init = qty
    else:
        t_entry = int(entry["t_entry_ms"])
        px0 = float(entry["entry_px"])
        qty = float(entry["qty_tokens"])
        cash = float(entry["cash_sol"])
        capital_sol = float(entry.get("capital_sol", capital_sol))
        qty_init = float(entry.get("qty_init", qty))
        if qty <= 1e-12:
            # A management decision with no position is NOT a management decision.
            # Refuse it instead of silently scoring identical "do nothing" outcomes
            # for every policy (measured degenerate group, 2026-09-13).
            return {"status": "no_position_refused", "net_sol_returned": None,
                    "mint": tape.mint, "policy": acts,
                    "reason": "continuation with zero tokens held"}
        legs, costs = [], {"curve_fee_sol": 0.0, "slippage_sol": 0.0,
                           "creator_fee_sol": 0.0, "priority_sol": 0.0}

    checks, violations = 0, []

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            violations.append(tag)

    end = min(t_entry + policy.horizon_ms, int(tape.tt[-1]))
    if end <= t_entry:
        return {"status": "no_horizon", "net_sol_returned": None, "mint": tape.mint,
                "policy": acts}
    ticks = np.arange(t_entry + policy.tick_ms, end + 1, policy.tick_ms, dtype=np.int64)
    npri = np.searchsorted(tape.tt, ticks, side="right") - 1
    keep = npri >= 0
    ticks, npri = ticks[keep], npri[keep]
    if ticks.size == 0:
        return {"status": "no_ticks", "net_sol_returned": None, "mint": tape.mint,
                "policy": acts}

    equity_path = [(t_entry, float(cash + qty * px0))]
    # Cash immediately after the entry conversion (fresh entry: deploy already
    # swapped into tokens; continuation: handed in by the caller). The entry BUY is
    # a cash->token CONVERSION, not a cash outflow, so it must never appear as
    # `spent` in the reconciliation below (measured bug: double-counting it produced
    # a spurious cash_ledger_reconcile violation of exactly deploy_sol).
    cash_start = cash
    rungs = list(policy.tp_ladder)
    # EXIT_NOW is a REAL action: sell the whole position at the first eligible
    # tick. Without this branch an empty ladder/trail/stop policy would just hold
    # to the horizon and silently masquerade as an exit (measured bug, 2026-09-13:
    # EXIT_NOW and HOLD_TO_HORIZON returned identical numbers).
    exit_now = policy.name == "EXIT_NOW"
    rungs_filled, trail_armed, peak, exit_reason = 0, False, px0, "horizon"
    stop_px = px0 * (1.0 - policy.stop_bp / 1e4) if policy.stop_bp > 0 else None
    max_consulted = int(tape.tt[min(int(npri[0]), tape.tt.size - 1)])
    sell = lambda t_fill, t_dec, q: engine.quote_sell(tape, t_fill, t_dec, q)

    def do_sell(t_fill, t_dec, amount, tag):
        nonlocal qty, cash, legs
        q = sell(t_fill, t_dec, amount)
        if q is None:
            return False
        cash += q["sol_out"]
        qty -= amount
        for k, key in (("curve_fee_sol", "fee_sol"), ("slippage_sol", "slippage_sol"),
                       ("creator_fee_sol", "creator_sol"), ("priority_sol", "priority_sol")):
            costs[k] += float(q.get(key, 0.0))
        legs.append((tag, t_fill, q["sol_out"], q))
        return True

    for k in range(ticks.size):
        tM, i = int(ticks[k]), int(npri[k])
        if i < 0 or (tM - t_entry) < policy.min_hold_ms:
            continue
        cur = tape.price_at_or_before(int(tape.tt[i]), tM, "mgmt_price")
        if cur is None or not np.isfinite(cur) or cur <= 0:
            continue
        max_consulted = max(max_consulted, int(tape.tt[i]))
        t_fill = int(tape.tt[i])
        peak = max(peak, cur)
        if qty <= 1e-12:
            break
        # 0. explicit exit-now: leave the whole position at the first eligible tick
        if exit_now:
            if do_sell(t_fill, tM, qty, "EXIT"):
                exit_reason = "exit_now"
            break
        # 1. hard stop (checked before ladder so a crash cannot be ladder-filled)
        if stop_px is not None and cur <= stop_px:
            if do_sell(t_fill, tM, qty, "STOP"):
                exit_reason = "stop"
            break
        # 2. trailing stop (ratchet: only ever exits the remainder)
        if policy.trail_distance_bp > 0 and trail_armed:
            trigger = peak * (1.0 - policy.trail_distance_bp / 1e4)
            if cur <= trigger:
                if do_sell(t_fill, tM, qty, "TRAIL"):
                    exit_reason = "trail"
                break
        if (policy.trail_activate_bp > 0
                and cur >= px0 * (1.0 + policy.trail_activate_bp / 1e4)):
            trail_armed = True
        # 3. TP ladder rungs (scale out fixed fractions of the ORIGINAL size)
        while rungs:
            gain_bp, frac = rungs[0]
            if cur < px0 * (1.0 + gain_bp / 1e4):
                break
            amount = min(qty, frac * qty_init)
            if amount > 1e-12 and do_sell(t_fill, tM, amount, f"TP{gain_bp:g}"):
                rungs_filled += 1
                if policy.move_stop_to_breakeven and rungs_filled == 1:
                    stop_px = max(stop_px or px0, px0)
            rungs.pop(0)
        equity = cash + qty * cur
        chk("equity_finite", bool(np.isfinite(equity)))
        chk("equity_nonneg", equity >= -1e-9)
        chk("cash_nonneg", cash >= -1e-9)
        chk("qty_nonneg", qty >= -1e-12)
        if np.isfinite(equity):
            equity_path.append((tM, float(equity)))

    jm = min(int(npri[-1]), tape.tt.size - 1)
    mark = float(tape.px[jm]) if np.isfinite(tape.px[jm]) else px0
    if int(tape.tt[jm]) > int(tape.tt[-1]):
        raise LookaheadError("terminal mark beyond tape")
    final_equity = cash + qty * mark
    net = final_equity - capital_sol
    equity_path.append((int(tape.tt[jm]), float(final_equity)))
    chk("no_lookahead", max_consulted <= max(int(ticks[-1]), t_entry))
    spent = sum(l[2] for l in legs if l[0] == "ADD")
    recvd = sum(l[2] for l in legs if l[0] not in ("BUY", "ADD"))
    chk("cash_ledger_reconcile",
        abs((cash_start - spent + recvd) - cash) < 1e-9)
    for tag, val in (("final_equity", final_equity), ("net_sol", net)):
        chk(f"lamport_roundtrip_{tag}",
            abs(to_lamports_checked(val, tag) / LAMPORTS_PER_SOL - val) <= 5e-10)
    return {"status": "ok", "mint": tape.mint, "policy": acts,
            "t_dec_ms": int(t_dec_ms), "t_entry_ms": t_entry, "entry_px": px0,
            "legs": [{"action": l[0], "t_ms": l[1], "sol": l[2]} for l in legs],
            "rungs_filled": rungs_filled, "rungs_total": len(policy.tp_ladder),
            "trail_armed": bool(trail_armed), "exit_reason": exit_reason,
            "exit_ts_ms": int(equity_path[-1][0]),
            "final_equity_sol": final_equity, "net_sol_returned": net,
            "qty_tokens_left": qty, "cash_sol": cash,
            "costs_sol": {k: round(v, 12) for k, v in costs.items()},
            "total_cost_sol": sum(costs.values()),
            "equity_path": [[int(t), float(v)] for t, v in equity_path],
            "conservation_checks": checks, "violations": violations,
            "state": causal_state(tape, t_dec_ms, px0)}


def causal_state(tape, t_dec_ms: int, entry_px: float) -> dict:
    """Causal fingerprint of the decision, for CONDITIONAL evaluation.

    Only information at/before t_dec_ms: age, prior-trade count, graduation
    status and net SOL flow. NOTE: trader concentration is NOT available here -
    reward_engine.Tape carries (tt, sol, tokens, venue, side, px) with no trader
    field, so it can only come from the canonical trades (which do have `trader`).
    Used to check that a policy behaves differently across regimes on mints it
    never trained on - the adaptivity requirement - with no future information.
    """
    tt = tape.tt
    n = int(np.searchsorted(tt, t_dec_ms, side="right"))
    age_ms = (t_dec_ms - int(tt[0])) if tt.size else 0
    sol = np.asarray(tape.sol[:n], dtype=float) if hasattr(tape, "sol") else np.zeros(0)
    px = float(entry_px)
    gx = tape.venue_change_index()
    return {"age_s": round(age_ms / 1000.0, 1), "n_prior": n,
            "graduated": bool(gx is not None and int(tt[gx]) <= t_dec_ms),
            "net_flow_sol": round(float(sol.sum()) / LAMPORTS_PER_SOL, 4) if sol.size else 0.0}


def _self_check() -> int:
    fails = []
    if len(POLICY_NAMES) < 5:
        fails.append("template_set_too_small")
    for nm, p in TEMPLATES.items():
        if p.name != nm:
            fails.append(f"template_name_mismatch:{nm}")
    # ladder fractions must not exceed the position
    for nm, p in TEMPLATES.items():
        tot = sum(f for _, f in p.tp_ladder)
        if tot > 1.0 + 1e-9:
            fails.append(f"ladder_over_100pct:{nm}={tot}")
    # shoreline: every BASELINE policy must pass the mechanical guards at the real floor
    for nm in BASELINE_POLICIES:
        v = validate_policy(TEMPLATES[nm], cost_floor_bps=100)
        if v:
            fails.append(f"baseline_fails_guard:{nm}:{v}")
    # rejected policies must actually BE rejected (they are documented negatives)
    if not any(validate_policy(TEMPLATES[nm], cost_floor_bps=100)
               for nm in REJECTED_POLICIES):
        fails.append("rejected_policies_unexpectedly_pass_guards")
    # the guard must catch a constructed negative ratchet
    bad = ExitPolicy("BAD", trail_activate_bp=100.0, trail_distance_bp=2000.0)
    if not validate_policy(bad, cost_floor_bps=100):
        fails.append("guard_missed_negative_ratchet")
    # the noise guard must fire for a sub-noise stop once sigma is supplied
    if not any("STOP_INSIDE_NOISE" in x
               for x in validate_policy(TEMPLATES["SCALP_TIGHT"], cost_floor_bps=100,
                                        sigma_bps=900.0)):
        fails.append("noise_guard_missed_tight_stop")
    print(json.dumps({"suite": "exit_policy", "failed": len(fails), "failures": fails,
                      "templates": list(POLICY_NAMES),
                      "baselines": list(BASELINE_POLICIES),
                      "rejected": list(REJECTED_POLICIES)}, indent=1))
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