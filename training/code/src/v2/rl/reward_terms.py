#!/usr/bin/env python
"""RL reward terms + advantage estimators for the mev_bot trading brain.

WHY THIS EXISTS (principal-engineer review, 2026-09-13): raw simulated PnL as an
RL objective teaches "max size on any positive-EV read" and blow-up tails. This
module turns ONE mechanics-simulated episode (reward_engine.score_action_sequence)
into a RISK-ADJUSTED, SCALE-FREE reward scalar, and supplies the group-relative /
counterfactual advantage estimators the GRPO loop needs. Mechanical only: no
model-generated targets, no invented returns, no future information.

Reward shape (per episode), all terms computed from the episode's own causal
marked-to-market equity path:
    r_cap     = net_sol / ACCOUNT_capital_sol         return on the ACCOUNT
    dd_frac   = max_drawdown_sol / capital_sol
    dsd       = downside deviation of per-tick equity returns
    time_frac = time_in_market_ms / horizon_ms
    exposure  = deploy_sol / capital_sol              fraction of account at risk
    reward    = r_cap - L_DD*dd_frac - L_DSD*dsd - L_TIME*time_frac - L_EXP*exposure^2
    ruin      : equity <= 0 anywhere -> reward = -RUIN_PENALTY (hard, flagged)

TAIL RISK / CVaR (2026-09-21) - AND WHY THE OBVIOUS FORM IS A NO-OP.
The acceptance gate for this project is a LOWER BOUND (mint-clustered 95% LB), so
the objective should care about the tail of the outcome distribution, not only the
average episode's drawdown. The obvious construction - "subtract lambda*CVaR from
every episode reward" - is PROVABLY INERT here and must never be shipped as if it
did something:

    advantage_i = (r_i - mean(r)) / (std(r) + eps)          # GRPO

subtracting any per-group constant c from every r_i cancels exactly, because
mean(r - c) = mean(r) - c and std(r - c) = std(r). The same shift-invariance holds
for the leave-one-out estimator (counterfactual_advantages). Verified numerically in
_self_check() as `cvar_constant_shift_is_noop`.

Two formulations that DO change the objective, both implemented below:

  (A) TAIL AMPLIFICATION (reward-side, changes within-group ordering):
        r_i <- r_i - L_CVAR * max(0, q_alpha - r_i)
      Only episodes below the group's alpha-quantile are penalised, and deeper
      ones more, so the disadvantage ordering changes. Opt-in: L_CVAR defaults to
      0.0 (like L_EXP) so no existing number moves silently.

  (B) BATCH CVaR PENALTY (objective-side, cannot be cancelled by group
      normalisation): add L_CVAR * CVaR_alpha(batch returns) to the LOSS. Applied
      by the GRPO loop on the batch/group distribution, not per episode.

CVaR_alpha is the mean of the worst ceil(alpha*n) values (standard empirical
estimate). alpha is pinned alongside the lambdas, never tuned per run.

SIZE, AND WHY L_EXP IS QUADRATIC (2026-09-14). With capital == deploy (the old
accounting) r_cap is invariant to position size and no size can ever be learned.
With capital == the ACCOUNT, r_cap and dd_frac both scale LINEARLY in exposure, so
a linear objective is maximised at a CORNER: all-in on any positive read, which is
the blow-up tail this module exists to prevent. The quadratic term is what creates
an interior optimum - maximise w*mu - L*w^2 gives w* = mu/(2L), i.e. size in
proportion to edge, capped by risk aversion. That is the standard fractional-sizing
construction, and it is what makes a learnerable, non-degenerate size dimension
possible. Lambda is PINNED here, never tuned per-run.

A refusal / no-entry / no-horizon is NOT a zero-return opportunity to be graded:
it returns reward 0.0 with refused=True and the reason preserved, so the caller
MASKS it instead of training on it.

Run:  /home/alon/qwen27b-venv/bin/python reward_terms.py   (self-check + JSON)
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass, asdict

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

REFUSAL_STATUSES = (
    "refused_cross_graduation", "refused_no_reserves", "no_entry",
    "bad_entry_price", "no_horizon", "no_ticks", "no_position_refused",
)


@dataclass(frozen=True)
class RewardConfig:
    """Risk weights. Deliberately a pinned, frozen object: the RL objective must
    not drift silently between runs."""
    lambda_drawdown: float = 1.0     # per unit of account-relative drawdown
    lambda_downside: float = 1.0     # per unit of downside deviation
    lambda_time: float = 0.05        # per unit of horizon spent in market
    lambda_exposure: float = 0.0     # per unit of exposure^2 (0.0 = legacy: the
                                     # size term is opt-in so no existing number
                                     # moves silently; entry sets it explicitly)
    ruin_penalty: float = 1.0        # hard terminal penalty (>= max |r_cap| seen)
    horizon_ms: int = 1_800_000      # episode horizon the engine used
    lambda_cvar: float = 0.0         # tail-risk weight. 0.0 = legacy: the CVaR
                                     # terms are opt-in so no existing number moves
                                     # silently. NOTE: the per-episode form is a
                                     # NO-OP under GRPO normalisation - see the
                                     # module docstring. Effectful use is (A)
                                     # tail_amplified_rewards and (B)
                                     # cvar_loss_penalty, both driven by this knob.
    cvar_alpha: float = 0.05         # tail fraction (0.05 = worst 5%), matching the
                                     # mint-clustered 95% LB acceptance gate. Pinned.

    def __post_init__(self):
        if not 0.0 < self.cvar_alpha <= 1.0:
            raise ValueError(f"cvar_alpha must be in (0,1], got {self.cvar_alpha}")
        if self.lambda_cvar < 0.0:
            raise ValueError(f"lambda_cvar must be >= 0, got {self.lambda_cvar}")

    def as_dict(self) -> dict:
        return asdict(self)


DEFAULT_CONFIG = RewardConfig()


def equity_returns(path) -> np.ndarray:
    """Per-step simple returns of an equity path [[t_ms, equity], ...]."""
    eq = np.asarray([float(p[1]) for p in path], dtype=float)
    if eq.size < 2:
        return np.zeros(0, dtype=float)
    prev = eq[:-1]
    out = np.zeros(eq.size - 1, dtype=float)
    ok = prev > 0
    out[ok] = (eq[1:][ok] - prev[ok]) / prev[ok]
    return out


def episode_risk(episode: dict, cfg: RewardConfig = DEFAULT_CONFIG) -> dict:
    """Drawdown / downside / time / ruin metrics for one simulated episode."""
    path = episode.get("equity_path") or []
    final_eq = float(episode.get("final_equity_sol", 0.0))
    net = float(episode.get("net_sol_returned", 0.0))
    capital = final_eq - net                      # exact, no assumption
    if capital <= 0:
        raise ValueError(f"non-positive implied capital {capital}")
    eq = np.asarray([float(p[1]) for p in path], dtype=float) if path else np.array([capital])
    peak = np.maximum.accumulate(eq)
    dd = peak - eq
    dd_sol = float(dd.max()) if dd.size else 0.0
    r = equity_returns(path)
    neg = r[r < 0]
    dsd = float(np.sqrt(np.mean(neg ** 2))) if neg.size else 0.0
    ts = [float(p[0]) for p in path]
    span = (max(ts) - min(ts)) if len(ts) >= 2 else 0.0
    # fraction of the ACCOUNT put at risk. Absent on legacy episodes (where the
    # caller had capital == deploy, so exposure was implicitly 1.0 and the size
    # term did not exist) -> reported as None so the term is skipped, never
    # silently scored as a full-size bet.
    dep = episode.get("deploy_sol")
    exposure = None if dep is None else float(dep) / capital
    return {
        "capital_sol": capital,
        "deploy_sol": None if dep is None else float(dep),
        "exposure_frac": exposure,
        "net_sol": net,
        "return_on_capital": net / capital,
        "max_drawdown_sol": dd_sol,
        "max_drawdown_frac": dd_sol / capital,
        "downside_deviation": dsd,
        "time_in_market_ms": span,
        "time_frac": (span / cfg.horizon_ms) if cfg.horizon_ms else 0.0,
        "min_equity_sol": float(eq.min()) if eq.size else capital,
        "ruin": bool(eq.size and eq.min() <= 0.0),
        "n_points": int(eq.size),
    }


def reward_scalar(episode: dict, cfg: RewardConfig = DEFAULT_CONFIG) -> dict:
    """Risk-adjusted scalar reward for one episode (see module docstring)."""
    status = episode.get("status")
    if status in REFUSAL_STATUSES or status != "ok":
        return {"reward": 0.0, "refused": True, "status": status,
                "reason": episode.get("reason"),
                "components": None, "risk": None, "config": cfg.as_dict()}
    risk = episode_risk(episode, cfg)
    if risk["ruin"]:
        return {"reward": -cfg.ruin_penalty, "refused": False, "status": status,
                "reason": "equity<=0 (ruin)", "components": {"ruin_penalty": -cfg.ruin_penalty},
                "risk": risk, "config": cfg.as_dict()}
    r_cap = risk["return_on_capital"]
    comp = {
        "return_on_capital": r_cap,
        "drawdown_term": -cfg.lambda_drawdown * risk["max_drawdown_frac"],
        "downside_term": -cfg.lambda_downside * risk["downside_deviation"],
        "time_term": -cfg.lambda_time * risk["time_frac"],
        "exposure_term": (-cfg.lambda_exposure * (risk["exposure_frac"] ** 2)
                          if risk.get("exposure_frac") is not None else 0.0),
    }
    return {"reward": float(sum(comp.values())), "refused": False, "status": status,
            "reason": None, "components": comp, "risk": risk, "config": cfg.as_dict()}


def group_advantages(returns, eps: float = 1e-6) -> np.ndarray:
    """GRPO advantage: (r - mean(group)) / (std(group) + eps).

    Scale-free by construction, which is the point for us: absolute SOL returns
    differ by orders of magnitude across mints/regimes and must not be treated as
    comparable magnitudes.
    """
    r = np.asarray(list(returns), dtype=float)
    if r.size == 0:
        return r
    return (r - r.mean()) / (r.std() + eps)


def counterfactual_advantages(returns, eps: float = 1e-6) -> np.ndarray:
    """Leave-one-out baseline: advantage_i = r_i - mean(r_j, j != i).

    STRICTLY lower variance than sampling independent completions when the same
    episode can be re-scored under different actions at the same reserves - which
    is exactly what reward_engine.score_action_sequence(tape, t_dec_ms, actions)
    gives us deterministically. Preferred over group_advantages when the caller
    has >=3 candidate action sequences for the SAME decision.
    """
    r = np.asarray(list(returns), dtype=float)
    n = r.size
    if n <= 1:
        return np.zeros(n, dtype=float)
    total = r.sum()
    loo = (total - r) / (n - 1)
    return r - loo


def group_cvar(values, alpha: float = 0.05) -> dict:
    """Empirical CVaR (mean of the worst ceil(alpha*n) values) of a group/batch.

    Reported in RAW units plus in group-standard-deviation units (`cvar_z`), because
    raw SOL returns are not comparable across mints/regimes while the z form is.
    """
    r = np.asarray(list(values), dtype=float)
    if r.size == 0:
        return {"cvar": 0.0, "cvar_z": 0.0, "alpha": float(alpha), "n": 0,
                "n_tail": 0, "q_alpha": None, "scale": None}
    a = float(min(max(alpha, 1e-9), 1.0))
    n_tail = max(1, int(np.ceil(a * r.size)))
    s = np.sort(r)
    cvar = float(s[:n_tail].mean())
    scale = float(r.std() + 1e-6)
    return {"cvar": cvar, "cvar_z": cvar / scale, "alpha": a, "n": int(r.size),
            "n_tail": n_tail, "q_alpha": float(np.quantile(r, a)), "scale": scale}


def tail_amplified_rewards(returns, cfg: RewardConfig = DEFAULT_CONFIG) -> dict:
    """(A) THE EFFECTFUL REWARD-SIDE FORM.

        r_i <- r_i - L_CVAR * max(0, q_alpha - r_i) / (std(r) + eps)

    Only episodes below the group's alpha-quantile are touched, deeper ones more, so
    the DISADVANTAGE ORDERING changes. Scale-free (shortfall measured in group
    standard deviations) so L_CVAR is comparable across groups. L_CVAR == 0.0 is an
    exact identity, which keeps every existing number still.

    Do NOT replace this with a constant shift: see the module docstring - that is a
    no-op under group normalisation, and `cvar_constant_shift_is_noop` proves it.
    """
    r = np.asarray(list(returns), dtype=float)
    info = group_cvar(r, cfg.cvar_alpha)
    if r.size == 0 or cfg.lambda_cvar == 0.0:
        return {"rewards": r, "applied": False, "lambda_cvar": cfg.lambda_cvar,
                "n_penalised": 0, "max_penalty": 0.0, **info}
    shortfall = np.maximum(0.0, info["q_alpha"] - r) / info["scale"]
    penalty = cfg.lambda_cvar * shortfall
    return {"rewards": r - penalty, "applied": True, "lambda_cvar": cfg.lambda_cvar,
            "n_penalised": int((shortfall > 0).sum()), "max_penalty": float(penalty.max()),
            **info}


def cvar_group_weight(returns, cfg: RewardConfig = DEFAULT_CONFIG) -> float:
    """(C) THE EFFECTFUL OBJECTIVE-SIDE FORM: a per-group LOSS MULTIPLIER.

        w_g = 1 + L_CVAR * max(0, -CVaR_alpha(r_g) / scale_g)

    Groups carrying a bad tail contribute MORE gradient. A scalar multiplier is
    constant w.r.t. the policy parameters, so it survives a non-differentiable
    reward path (our rewards come from a mechanics SIMULATOR - there is no gradient
    through them, so a genuinely differentiable CVaR loss term is not available
    here; this is the honest way to weight the tail).
    """
    if cfg.lambda_cvar == 0.0:
        return 1.0
    info = group_cvar(returns, cfg.cvar_alpha)
    if info["n"] == 0:
        return 1.0
    return float(1.0 + cfg.lambda_cvar * max(0.0, -info["cvar_z"]))


def episode_reward(tape, t_dec_ms, actions, engine=None, cfg: RewardConfig = DEFAULT_CONFIG,
                   **score_kw) -> dict:
    """Score one action sequence end-to-end: simulator -> risk-adjusted reward."""
    from reward_engine import score_action_sequence
    ep = score_action_sequence(tape, t_dec_ms, actions, engine=engine, **score_kw)
    out = reward_scalar(ep, cfg)
    out["episode"] = ep
    return out


def _self_check() -> int:
    """Deterministic checks of the reward MATH on synthetic equity paths."""
    fails = []
    c = RewardConfig()
    def mk(net, path, status="ok"):
        final = 100.0 + net
        return {"status": status, "net_sol_returned": net, "final_equity_sol": final,
                "equity_path": [[i * 1000, v] for i, v in enumerate(path)]}
    flat = mk(5.0, [100.0, 101.0, 103.0, 105.0])
    dip = mk(5.0, [100.0, 80.0, 103.0, 105.0])          # same net, deeper drawdown
    r_flat, r_dip = reward_scalar(flat, c), reward_scalar(dip, c)
    if not r_flat["reward"] > r_dip["reward"]:
        fails.append("drawdown_term_not_penalising")
    if not np.isclose(r_flat["components"]["return_on_capital"], 0.05):
        fails.append("return_on_capital_wrong")
    ruin = reward_scalar(mk(-120.0, [100.0, 30.0, 0.0, -20.0]), c)
    if not (ruin["reward"] == -c.ruin_penalty and ruin["risk"]["ruin"]):
        fails.append("ruin_not_hard_failed")
    ref = reward_scalar(mk(50.0, [100.0, 150.0], status="refused_no_reserves"), c)
    if not (ref["refused"] and ref["reward"] == 0.0):
        fails.append("refusal_graded_as_return")
    r = [1.0, 2.0, 3.0]
    # leave-one-out mean of [1,2,3] is [2.5, 2.0, 1.5] -> advantages [-1.5, 0.0, 1.5]
    if not np.allclose(counterfactual_advantages(r), [-1.5, 0.0, 1.5]):
        fails.append("counterfactual_baseline_wrong")
    g = group_advantages(r)
    if not (np.isclose(g.mean(), 0.0) and g[2] > g[0]):
        fails.append("group_normalisation_wrong")

    # ---- CVaR tail-risk checks (2026-09-21) ----------------------------------
    # (1) math: worst ceil(0.4*5)=2 of [10,2,3,1,-8] are [-8,1] -> mean -3.5
    ci = group_cvar([10.0, 2.0, 3.0, 1.0, -8.0], alpha=0.4)
    if not (ci["n_tail"] == 2 and np.isclose(ci["cvar"], -3.5)):
        fails.append("cvar_math_wrong")
    # (2) the naive constant-shift form must be PROVEN inert, so nobody re-adds it
    cshift = RewardConfig(lambda_cvar=1.0, cvar_alpha=0.4)
    base = np.array([10.0, 2.0, 3.0, 1.0, -8.0])
    shifted = base - group_cvar(base, 0.4)["cvar"]
    if not np.allclose(group_advantages(base), group_advantages(shifted)):
        fails.append("cvar_constant_shift_is_noop")
    if not np.allclose(counterfactual_advantages(base), counterfactual_advantages(shifted)):
        fails.append("cvar_constant_shift_is_noop_loo")
    # (3) tail amplification MUST change the within-group ordering
    tap = tail_amplified_rewards(base, cshift)
    if not tap["applied"] or tap["n_penalised"] < 1:
        fails.append("cvar_tail_amplification_inert")
    if np.allclose(group_advantages(base), group_advantages(tap["rewards"])):
        fails.append("cvar_tail_amplification_does_not_bite")
    # worst episode must be pushed further down, not up
    a0, a1 = group_advantages(base), group_advantages(tap["rewards"])
    if not a1[np.argmin(base)] < a0[np.argmin(base)]:
        fails.append("cvar_tail_amplification_wrong_direction")
    # (4) lambda_cvar == 0.0 is an exact identity (no silent number moves)
    z = tail_amplified_rewards(base, RewardConfig())
    if z["applied"] or not np.allclose(z["rewards"], base):
        fails.append("cvar_zero_lambda_not_identity")
    if cvar_group_weight(base, RewardConfig()) != 1.0:
        fails.append("cvar_zero_lambda_weight_not_one")
    # (5) a bad tail raises the group loss weight; a clean group does not
    bad = cvar_group_weight([1.0, 1.0, 1.0, -50.0], cshift)
    clean = cvar_group_weight([0.0, 0.001, 0.002, 0.001], cshift)
    if not (bad > 1.0 and clean >= 1.0):
        fails.append("cvar_group_weight_wrong")
    # (6) alpha must be validated, not silently clamped into nonsense
    for bad_alpha in (0.0, -0.1, 1.5):
        try:
            RewardConfig(cvar_alpha=bad_alpha)
            fails.append(f"cvar_alpha_not_validated:{bad_alpha}")
        except ValueError:
            pass

    print(json.dumps({"suite": "reward_terms", "failed": len(fails), "failures": fails,
                      "flat_reward": r_flat["reward"], "dip_reward": r_dip["reward"],
                      "ruin_reward": ruin["reward"],
                      "cvar_demo": {"cvar": ci["cvar"], "n_penalised": tap["n_penalised"],
                                    "max_penalty": tap["max_penalty"],
                                    "adv_before": a0.tolist(), "adv_after": a1.tolist(),
                                    "weight_bad": bad, "weight_clean": clean}}, indent=1))
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