#!/usr/bin/env python
"""GRPO trainer for the mev_bot trading brain (group-relative, counterfactual).

DESIGN (principal-engineer review, 2026-09-13) - five binding choices:

1. LOSS MASKED TO THE DECISION SPAN. Our action is structured, not prose. Gradient
   flows ONLY through the DECISION field of the completion (plus the SIZE field on
   a BUY); the rationale/evidence prose is never optimized. Training on the whole
   completion teaches the model to narrate its way to reward.
2. COUNTERFACTUAL ADVANTAGE, not sampled-group advantage. For every decision the
   simulator deterministically scores EVERY candidate action on the same reserves
   (grpo_reward_bridge.score_decision). The advantage of a sampled action is its
   exact leave-one-out advantage over the alternatives - lower variance than
   sampling G completions and hoping they differ.
3. KL TO A FROZEN REFERENCE, VIA A CACHED LOGPROB TABLE. Loading a second 27B
   reference alongside the policy does not fit 3x 96GB with full-parameter
   training. The reference IS the SFT best checkpoint and the prompts are frozen,
   so its per-decision-token logprobs are precomputed ONCE
   (grpo_ref_cache.py) and read as constant KL anchors. If the cache is missing the
   trainer REFUSES to run rather than silently training without a KL anchor.
4. CHECKPOINT SELECTION BY PREREGISTERED GATE. Never by training reward. A
   checkpoint is selectable only if the lower 95% bound of net return per episode
   on the frozen evaluation partition is above the preregistered floor.
5. HONEST REFUSALS. Unparseable completions, refused episodes and decisions with
   no usable tape are MASKED (no gradient), never graded as zero.

This module is import-safe and CPU-testable: main() --self-check exercises the
mask builder, the advantage assembly and the gate on synthetic inputs.

CARRY-THROUGH (2026-09-21). Every per-decision field the objective needs must already be
on the record, because advantages are precomputed ONCE at build time and merely looked up
here (choice 3). The build path (grpo_dataset.py / emit_rl_management_group.py) now carries
`group_loss_weight` and a compact `cvar` diagnostic. `read_group_loss_weight` is the ONE
reader of the weight: ABSENT MEANS 1.0 (every pre-v2 record was built with lambda_cvar
pinned OFF, so its true weight IS 1.0), and a PRESENT-but-malformed weight is refused
loudly rather than coerced. Today the weight is 1.0 everywhere, so reading it is an exact
no-op; `rl_cvar_noop_gate.py` proves the build side changes no advantage.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys
from dataclasses import dataclass, asdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


@dataclass(frozen=True)
class GRPOConfig:
    group_size: int = 4              # samples per prompt (decode breadth)
    temperature: float = 0.9
    top_p: float = 0.95
    kl_coef: float = 0.02
    lr: float = 5e-7                 # RL LR << SFT LR: the policy is already good
    max_new_tokens: int = 320
    mask_rationale: bool = True      # choice (1): never optimize the prose
    min_usable_candidates: int = 2   # below this the decision is masked
    # preregistered profitability gate (choice 4)
    gate_floor_net_sol: float = 0.0
    gate_min_episodes: int = 2000
    gate_confidence: float = 0.95

    def as_dict(self) -> dict:
        return asdict(self)


DEFAULT_CONFIG = GRPOConfig()


def decision_span_mask(offsets, text: str, want_size: bool = True) -> list:
    """Token mask selecting ONLY the DECISION field (and SIZE when asked).

    `offsets` is the tokenizer's per-token (char_start, char_end) for the FULL
    sequence; `text` the full decoded sequence. A token is selected when its char
    span intersects the DECISION field's span, computed from the fixed answer
    format ('DECISION: X' ... '\n'), so the mask is data-derived, not hardcoded
    token ids.
    """
    import re
    mask = [0] * len(offsets)
    spans = []
    m = re.search(r"DECISION\s*[:\-\u2014]\s*", text)
    if m:
        start = m.end()
        nl = text.find("\n", start)
        spans.append((start, nl if nl != -1 else len(text)))
    if want_size:
        s = re.search(r"SIZE[^:\n]*[:\-]\s*", text)
        if s:
            start = s.end()
            nl = text.find("\n", start)
            spans.append((start, nl if nl != -1 else len(text)))
    for i, (a, b) in enumerate(offsets):
        if a is None or b is None:
            continue
        for (s, e) in spans:
            if a < e and b > s:
                mask[i] = 1
                break
    return mask


def read_group_loss_weight(decision: dict) -> float:
    """The per-decision GROUP LOSS MULTIPLIER, defaulting to 1.0 when the field is absent.

    ABSENCE IS 1.0, NOT AN ERROR. Records are precomputed at build time; every record
    emitted before the v2 schema carries no such field, and those were built with
    lambda_cvar pinned OFF, so their true weight IS 1.0. Substituting anything else here
    would re-tune the objective from the trainer side.

    A PRESENT-but-malformed weight REFUSES loudly: coercing a corrupt weight to 1.0 would
    silently drop a real objective factor - exactly the drift the frozen RewardConfig
    exists to prevent.
    """
    raw = (decision or {}).get("group_loss_weight")
    if raw is None:
        return 1.0
    if isinstance(raw, bool) or not isinstance(raw, (int, float)) \
            or not math.isfinite(float(raw)) or float(raw) < 0.0:
        raise ValueError(f"REFUSING: malformed group_loss_weight {raw!r} - expected a "
                         f"finite float >= 0, or the field absent (absent means 1.0)")
    return float(raw)


def assemble_advantages(samples: list, decision: dict,
                        cfg: GRPOConfig = DEFAULT_CONFIG) -> dict:
    """Attach a counterfactual advantage to each sampled completion.

    `samples`: [{'action': 'BUY'|'SKIP'|..., 'text': ..., 'refusal': bool}, ...]
    `decision`: output of grpo_reward_bridge.score_decision for this prompt, or a
    precomputed dataset record carrying the same keys.
    Returns {usable: bool, reason, per_sample_advantages: [...], selected: [...]}.

    A sample whose parsed action is unparseable or not in the scored candidate set
    is MASKED, never assigned a fabricated advantage. `group_loss_weight` is read (and
    `cvar` passed through for observability) so the caller never has to re-derive either.
    """
    glw = read_group_loss_weight(decision)          # refuses loudly on a corrupt value
    if not decision.get("group_usable"):
        return {"usable": False, "reason": "no_usable_counterfactual_group",
                "per_sample_advantages": [], "selected": [],
                "group_loss_weight": glw, "cvar": decision.get("cvar")}
    adv = decision["advantages"]
    out, sel = [], []
    for s in samples:
        a = s.get("action")
        if s.get("refusal") or a is None or a not in adv:
            out.append(None)          # masked: no gradient
            sel.append(False)
            continue
        out.append(float(adv[a]))
        sel.append(True)
    usable = sum(1 for x in sel if x)
    if usable < cfg.min_usable_candidates:
        return {"usable": False, "reason": f"only {usable} usable samples",
                "per_sample_advantages": out, "selected": sel,
                "group_loss_weight": glw, "cvar": decision.get("cvar")}
    return {"usable": True, "reason": None, "per_sample_advantages": out,
            "selected": sel, "advantage_mode": decision.get("advantage_mode"),
            "group_loss_weight": glw, "cvar": decision.get("cvar")}


def _lb(r, cfg):
    """Normal-approx lower one-sided bound on a single list of returns."""
    import math
    n = len(r)
    if n == 0:
        return {"n": 0, "mean": None, "lower_bound": None}
    mean = sum(r) / n
    if n < 2:
        return {"n": n, "mean": mean, "lower_bound": None}
    var = sum((x - mean) ** 2 for x in r) / (n - 1)
    se = math.sqrt(var / n)
    z = {0.90: 1.2816, 0.95: 1.6449, 0.99: 2.3263}.get(cfg.gate_confidence, 1.6449)
    return {"n": n, "mean": mean, "se": se, "lower_bound": mean - z * se}


def gate_lower_bound(returns, cfg: GRPOConfig = DEFAULT_CONFIG, regimes=None) -> dict:
    """Lower one-sided confidence bound on mean net return (preregistered gate).

    Normal-approximation bound: mean - z * s / sqrt(n). Reported WITH the sample
    size so a thin partition can never masquerade as a pass.

    `regimes`, when given, is a parallel list of per-return venue labels
    ("amm" | "bonding_curve"). The result then carries a `by_regime` block so a
    pooled pass driven by venue MIX (AMM is systematically higher-EV than curve)
    is visible instead of silently crediting the edge. The pooled gate is still
    what `passes` reports; `by_regime` is the regime-correct view.
    """
    r = [float(x) for x in returns]
    n = len(r)
    if n == 0:
        return {"n": 0, "mean": None, "lower_bound": None, "passes": False,
                "reason": "no episodes"}
    mean = sum(r) / n
    if n < 2:
        return {"n": n, "mean": mean, "lower_bound": None, "passes": False,
                "reason": "fewer than 2 episodes"}
    lb = _lb(r, cfg)
    ok = (n >= cfg.gate_min_episodes and lb["lower_bound"] > cfg.gate_floor_net_sol)
    out = {"n": n, "mean": mean, "se": lb["se"], "lower_bound": lb["lower_bound"],
           "passes": bool(ok),
           "reason": None if ok else (
               f"needs n>={cfg.gate_min_episodes} (have {n})" if n < cfg.gate_min_episodes
               else f"lower bound {lb['lower_bound']:.6f} <= floor {cfg.gate_floor_net_sol}")}
    if regimes is not None:
        by_regime = {}
        for x, reg in zip(r, regimes):
            by_regime.setdefault(reg, []).append(x)
        out["by_regime"] = {reg: _lb(vals, cfg) for reg, vals in by_regime.items()}
    return out


def _self_check() -> int:
    fails = []
    # mask builder: only the DECISION value token is selected
    text = "EVIDENCE: x\nDECISION: BUY\nSIZE: 0.5\nINVALIDATION: y"
    # tokenize on non-whitespace runs: tokens must not merge fields across newlines,
    # which is what a real BPE tokenizer does for these fields.
    import re as _re
    offs = [(m.start(), m.end()) for m in _re.finditer(r"\S+", text)]
    mask = decision_span_mask(offs, text)
    picked = [text[a:b] for (a, b), m in zip(offs, mask) if m]
    if "BUY" not in picked:
        fails.append(f"decision_not_selected: {picked}")
    if any(p in ("EVIDENCE:", "INVALIDATION:") for p in picked):
        fails.append(f"prose_leaked_into_mask: {picked}")
    # advantage assembly masks the unusable sample instead of grading it
    dec = {"group_usable": True, "advantage_mode": "counterfactual_loo",
           "advantages": {"SKIP": 0.16, "BUY": -0.31}}
    a = assemble_advantages([{"action": "BUY"}, {"action": "SKIP"},
                             {"action": None}, {"action": "BUY", "refusal": True}], dec)
    if not (a["usable"] and a["per_sample_advantages"][:2] == [-0.31, 0.16]
            and a["per_sample_advantages"][2:] == [None, None]):
        fails.append(f"advantage_assembly_wrong: {a}")
    # CARRY-THROUGH: a record without the field reads as 1.0 (never fabricated)...
    if a["group_loss_weight"] != 1.0:
        fails.append(f"absent_weight_not_one: {a.get('group_loss_weight')!r}")
    # ...a carried weight is surfaced verbatim, with the cvar diagnostic alongside...
    dec_w = dict(dec, group_loss_weight=2.5, cvar={"applied": True, "lambda_cvar": 1.0})
    aw = assemble_advantages([{"action": "BUY"}, {"action": "SKIP"}], dec_w)
    if not (aw["usable"] and aw["group_loss_weight"] == 2.5
            and aw["cvar"] == {"applied": True, "lambda_cvar": 1.0}):
        fails.append(f"carried_weight_lost: {aw}")
    # ...and the weight NEVER touches the advantage numbers: it is an objective-side
    # multiplier applied by the loss, so the precomputed advantages must be identical.
    if aw["per_sample_advantages"] != a["per_sample_advantages"][:2]:
        fails.append("carried_weight_moved_advantage_numbers")
    # a corrupt weight refuses loudly instead of being silently coerced to 1.0
    for junk in ("2.5", True, -1.0, float("nan")):
        try:
            read_group_loss_weight({"group_loss_weight": junk})
            fails.append(f"malformed_weight_not_refused:{junk!r}")
        except ValueError:
            pass
    g_fail = gate_lower_bound([0.1] * 10)
    if g_fail["passes"]:
        fails.append("gate_passed_on_thin_partition")
    g_ok = gate_lower_bound([0.2] * 3000)
    if not g_ok["passes"]:
        fails.append("gate_failed_on_strong_partition")
    print(json.dumps({"suite": "grpo_trainer", "failed": len(fails),
                      "failures": fails, "mask_picked": picked}, indent=1))
    return 1 if fails else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true")
    a = ap.parse_args()
    if a.self_check:
        return _self_check()
    raise SystemExit(
        "grpo_trainer: driving loop not wired to GPUs yet - run --self-check. "
        "The live loop requires the frozen SFT best checkpoint and a built "
        "reference-logprob cache (grpo_ref_cache.py); it refuses to train without it.")


if __name__ == "__main__":
    sys.exit(main())