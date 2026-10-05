"""fill_drift — the MEASURED decision->fill gap, consumed by the reward.

WHY A MODULE AND NOT A CONSTANT IN rl_reward_v3. The entry is priced in
`simulate_v3` at the reserve fill, while the RL prompt states the price at the
DECISION INSTANT (`price_lamports_per_raw_token` on the DECISION CLOCK line).
Those are different trades, and the gap between them is a real cost that the
reward used to charge as nothing. The magnitude is measured over our own decisions
by `calibrate_fill_drift.py` and frozen in reports/FILL_DRIFT_C15.json; the reward
reads THAT file at call time. A literal in the reward is exactly the failure the
readiness gate's [M] class refuses on, because a literal cannot be re-derived and
silently survives every change to the tape.

TRAIN ON THE MEASURED ADVERSE QUANTILE, GATE ON THE STRESS LEVEL - the same shape
exit_mechanics uses for the exit cost. 'realistic' is the measured p90 of the
signed drift (the adverse side for a buy); 'pessimistic' doubles it and also takes
at least the worst venue's measured p90, so a run cannot be accepted on the level
it trained under.

There is deliberately NO reference floor (unlike exit_mechanics, which had another
codebase's Rust fixtures to fall back on): no other measurement of this gap exists
in the repo, and adopting a remembered number is what the operator ruling forbids.
An absent freeze therefore charges NOTHING and labels itself `unfrozen_zero` - a
state the readiness gate refuses. The label rides on every scored episode, so such
a run cannot be reported as measured.
"""
from __future__ import annotations

import argparse
import json

BPS_ONE = 10_000
FILL_DRIFT_FREEZE = "/training/v2/reports/FILL_DRIFT_C15.json"
# Mirrors exit_mechanics.PESSIMISTIC_FACTOR. Duplicated rather than imported: the
# two costs are independently recalibrated, and a shared constant would let a change
# to one silently move the other's stress level.
PESSIMISTIC_FACTOR = 2
UNFROZEN_FALLBACK = {"entry_slippage_bps": 0, "entry_slippage_bps_stress": 0}
_CACHE = None


def fill_drift_source() -> dict:
    """The frozen, MEASURED decision->fill drift (calibrate_fill_drift.py)."""
    global _CACHE
    if _CACHE is None:
        try:
            with open(FILL_DRIFT_FREEZE, encoding="utf-8") as fh:
                obj = json.load(fh)
            acc = obj.get("accounting") or {}
            _CACHE = {
                "source": "frozen",
                "values": {k: int(v) for k, v in obj["values"].items()},
                "quantile": acc.get("quantile"),
                "population": acc.get("population"),
                "code_sha256": obj.get("code_sha256"),
                "sign_convention": acc.get("sign_convention"),
                "justification": bool(obj.get("justification")),
            }
        except Exception:
            _CACHE = {"source": "unfrozen_zero", "values": dict(UNFROZEN_FALLBACK),
                      "quantile": None, "population": None, "code_sha256": None,
                      "sign_convention": None, "justification": False}
    return _CACHE


def entry_drift_bps(level: str = "realistic") -> dict:
    """The drift charged on an entry, in basis points of price, per level.

    'realistic' -> the measured p90 of the signed drift (adverse side for a buy).
    'pessimistic' -> PESSIMISTIC_FACTOR x that, floored by the worst venue's measured
    p90, so the gate's stress level is strictly worse than the training level.
    """
    src = fill_drift_source()
    base = int(src["values"].get("entry_slippage_bps") or 0)
    stress = int(src["values"].get("entry_slippage_bps_stress") or 0)
    bps = max(base * PESSIMISTIC_FACTOR, stress) if level == "pessimistic" else base
    return {"entry_slippage_bps": int(bps), "source": src["source"], "level": level,
            "quantile": src["quantile"], "population": src["population"]}


def charge_entry_fill(tokens_raw: float, bps: int) -> float:
    """Tokens actually received when the fill prints `bps` worse than the decision price.

    A positive `bps` means we paid MORE, so the same notional buys FEWER tokens.
    Monotone (never increases with bps), exact at bps=0 (returns the input
    unchanged), and bounded above by the input for all bps >= 0.
    """
    b = max(0, int(bps))
    if b == 0:
        return float(tokens_raw)
    return float(tokens_raw) * BPS_ONE / (BPS_ONE + b)


def entry_drift_cost_sol(sol_in: float, bps: int) -> float:
    """Immediate mark value of the charged drift, as its own cost bucket.

    The episode's cash ledger already reflects it through the reduced token count;
    this bucket exists so a diagnostic can say WHICH cost ate the trade - the same
    reason impairment_sol and terminal_loss_sol are separate from fee_sol.
    """
    b = max(0, int(bps))
    if b == 0:
        return 0.0
    return float(sol_in) * b / (BPS_ONE + b)


def _self_check() -> int:
    checks, fails = 0, []

    def chk(tag, cond, extra=""):
        nonlocal checks
        checks += 1
        if not cond:
            fails.append(tag if not extra else "%s (%s)" % (tag, extra))

    src = fill_drift_source()
    chk("source_labelled", src["source"] in ("frozen", "unfrozen_zero"), src["source"])
    real, pess = entry_drift_bps("realistic"), entry_drift_bps("pessimistic")
    chk("realistic_nonneg", real["entry_slippage_bps"] >= 0, real["entry_slippage_bps"])
    chk("pessimistic_strictly_worse", pess["entry_slippage_bps"] > real["entry_slippage_bps"]
        or real["entry_slippage_bps"] == 0, "%s vs %s"
        % (pess["entry_slippage_bps"], real["entry_slippage_bps"]))
    if src["source"] == "frozen":
        chk("frozen_value_positive", real["entry_slippage_bps"] > 0)
        chk("quantile_recorded", bool(src["quantile"]))
        chk("sign_convention_recorded", "fill" in str(src["sign_convention"]))
        chk("code_sha_pinned", bool(src["code_sha256"]))

    # Charge is exact at zero, monotone, and never increases the received tokens.
    chk("charge_identity_at_zero", charge_entry_fill(1234.5, 0) == 1234.5)
    a, b = charge_entry_fill(1234.5, 100), charge_entry_fill(1234.5, 500)
    chk("charge_monotone_decreasing", a < 1234.5 and b < a)
    chk("charge_bounded", b > 0)
    # The entry basis worsens by EXACTLY the charged bps - the number the reward
    # reports is the number it charged.
    px_ratio = (1.0 / charge_entry_fill(1.0, 250)) - 1.0
    chk("charge_is_exact_bps", abs(px_ratio * BPS_ONE - 250.0) < 1e-6,
        str(px_ratio * BPS_ONE))
    chk("cost_identity_at_zero", entry_drift_cost_sol(1.0, 0) == 0.0)
    c = entry_drift_cost_sol(1.0, 1863)
    chk("cost_positive_and_capped", 0.0 < c < 1.0, str(c))

    print(json.dumps({"checks": checks, "failed": fails,
                      "verdict": "PASS" if not fails else "FAIL",
                      "source": src["source"],
                      "values": {"realistic": real["entry_slippage_bps"],
                                 "pessimistic": pess["entry_slippage_bps"]}}, indent=1))
    return 0 if not fails else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true")
    ap.add_argument("--show", action="store_true")
    a = ap.parse_args()
    if a.self_check:
        return _self_check()
    print(json.dumps({"source": fill_drift_source(),
                      "realistic": entry_drift_bps("realistic"),
                      "pessimistic": entry_drift_bps("pessimistic")}, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
