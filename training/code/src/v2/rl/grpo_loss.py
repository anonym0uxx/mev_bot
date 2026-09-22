#!/usr/bin/env python
"""grpo_loss — the masked, counterfactual GRPO objective. Pure torch, CPU-testable.

FIVE INVARIANTS (each has a test below; do not weaken them):

I1  GRADIENT ONLY THROUGH THE DECISION FIELD. The loss is masked to the DECISION
    (and SIZE) tokens. The rationale/evidence prose is never optimized: training
    on the whole completion teaches the model to narrate its way to reward.
I2  A MASKED SAMPLE CONTRIBUTES NOTHING - not the policy term, not the KL term.
    An unparseable completion is not a zero-advantage sample; it has no gradient.
I3  ADVANTAGES SUM TO ZERO ACROSS THE GROUP. With SKIP pinned at 0.0 this makes
    "do nothing" a real competitor rather than a free pass, and it is what stops
    the always-SKIP collapse (always-SKIP earns negative advantage whenever the
    average arm is positive).
I4  KL USES CACHED REFERENCE LOGPROBS (k3 estimator, non-negative by construction).
    A missing reference is a REFUSAL, never a silent kl_coef=0.
I5  THE LOSS IS INVARIANT TO PROMPT LENGTH. Normalisation is by the number of
    SELECTED tokens, not by sequence length, so long rationale fields can't dilute
    the decision gradient.
I6  A PER-COMPLETION LOSS WEIGHT (the CVaR `group_loss_weight`) ENTERS AS A
    WEIGHTED MEAN. It scales numerator and denominator together, so the gradient
    scale never moves with the weight magnitude; the default weight (None, i.e.
    all-ones) takes an arithmetic-free branch and is bit-identical to the
    unweighted objective. Non-finite / negative / wrong-shape weights REFUSE.
"""
from __future__ import annotations

import json
import math
from dataclasses import dataclass, asdict

import torch
import torch.nn.functional as F


@dataclass(frozen=True)
class GRPOLossConfig:
    kl_coef: float = 0.02
    normalize_advantage: bool = True
    adv_clip: float = 5.0
    kl_clip: float = 10.0

    def as_dict(self):
        return asdict(self)


DEFAULT_LOSS_CONFIG = GRPOLossConfig()


def k3_kl(logp: torch.Tensor, ref_logp: torch.Tensor) -> torch.Tensor:
    """Per-token KL(policy || reference) via the k3 estimator.

    d = ref - policy;  k3 = exp(d) - d - 1 >= 0 always. Unbiased and lower
    variance than the naive -log(p/r) form when the policy has drifted.
    """
    d = ref_logp - logp
    return torch.exp(d.clamp(max=20.0)) - d - 1.0


def action_kl(policy_action_logits: torch.Tensor,
              ref_action_probs: torch.Tensor) -> torch.Tensor:
    """KL(pi || ref) over the DISCRETE ACTION distribution, per sample. [B, A] in.

    WHY THIS EXISTS (a real gap in the token-level design): grpo_ref_cache caches
    reference logprobs for the DECISION span OF THE SFT COMPLETION. At RL time the
    policy generates DIFFERENT tokens, so those cached per-token logprobs do not
    align with the sampled ones and the token-level KL is not computable.

    The action distribution is the right anchor: it is 3 numbers per decision
    (BUY/WATCH/SKIP), it is well defined no matter what the policy samples, and it
    is exactly the object the policy is being trained to choose.
    """
    logp = torch.log_softmax(policy_action_logits.float(), dim=-1)
    p = logp.exp()
    ref = ref_action_probs.float().clamp_min(1e-12)
    ref = ref / ref.sum(dim=-1, keepdim=True)
    return (p * (logp - ref.log())).sum(dim=-1)


# --------------------------------------------------------------- arm joint ----
# A BUY is not one action: it is (action, size tier). The RL anchor therefore has to
# be the JOINT over arms, not the 3-dim decision marginal - otherwise the size token
# is graded by the advantage but never regularised toward the reference, and a
# drifting size head has nothing holding it to the SFT prior.
ARM_ORDER = ("SKIP", "WATCH", "BUY_SMALL", "BUY_MID", "BUY_FULL")
BUY_ACTIONS = ("BUY", "WATCH", "SKIP")


def assemble_arm_probs(action_logits: torch.Tensor, size_logits: torch.Tensor,
                       actions=BUY_ACTIONS) -> torch.Tensor:
    """[B, A] decision logits + [B, S] size logits -> [B, 5] arm PROBABILITIES.

    Chain rule, so no extra machinery is needed to make the size slot coherent:
        P(BUY_TIER) = P(BUY) * P(TIER | BUY)
    P(TIER | BUY) is read at the size slot under the teacher-forced BUY prefix, which
    is exactly how grpo_ref_cache builds the reference - the two objects are therefore
    constructed identically and the KL compares like with like.

    Differentiable in BOTH slot logits: gradient flows to the decision slot through
    P(BUY) and to the size slot through P(TIER | BUY).
    """
    pa = torch.softmax(action_logits.float(), dim=-1)
    pt = torch.softmax(size_logits.float(), dim=-1)
    idx = {a: i for i, a in enumerate(actions)}
    for need in ("BUY", "SKIP", "WATCH"):
        if need not in idx:
            raise SystemExit(f"assemble_arm_probs: action basis lacks {need}")
    if pt.shape[-1] != 3:
        raise SystemExit("assemble_arm_probs: size basis must have 3 tiers")
    p_buy = pa[:, idx["BUY"]]
    out = torch.zeros(pa.shape[0], len(ARM_ORDER), dtype=torch.float32,
                      device=pa.device)
    out[:, 0] = pa[:, idx["SKIP"]]
    out[:, 1] = pa[:, idx["WATCH"]]
    out[:, 2] = p_buy * pt[:, 0]
    out[:, 3] = p_buy * pt[:, 1]
    out[:, 4] = p_buy * pt[:, 2]
    return out


def joint_arm_logits(action_logits: torch.Tensor, size_logits: torch.Tensor,
                     actions=BUY_ACTIONS) -> torch.Tensor:
    """log of the arm joint, shaped for action_kl() (which log_softmaxes its input).

    softmax(log(joint)) == joint because the joint already sums to 1, so passing this
    to action_kl is exactly KL(pi || ref) over the 5 arms - it does not double-count
    the normalisation.
    """
    return assemble_arm_probs(action_logits, size_logits, actions).clamp_min(1e-30).log()


def grpo_loss(logp: torch.Tensor, ref_logp: torch.Tensor, adv: torch.Tensor,
              mask: torch.Tensor, active: torch.Tensor | None = None,
              cfg: GRPOLossConfig = DEFAULT_LOSS_CONFIG,
              weight: torch.Tensor | None = None) -> tuple:
    """Masked counterfactual GRPO loss.

    logp     [B, T]  per-token logprob of the SAMPLED completion under the policy
    ref_logp [B, T]  the same under the frozen SFT reference (cached)
    adv      [B]     per-completion counterfactual advantage
    mask     [B, T]  1 on DECISION/SIZE tokens only
    active   [B]     1 where the completion is usable (parsed + in the group)
    weight   [B]     OPTIONAL per-completion LOSS WEIGHT (the group's
                     `group_loss_weight`, i.e. the CVaR tail term's group
                     multiplier). None means all-ones.
    Returns (loss, stats).

    THE WEIGHT FORMS A WEIGHTED MEAN, NEVER A WEIGHTED SUM (I6). It is folded into
    the SAME per-token mask that is summed for the denominator, so numerator and
    denominator move together and the gradient SCALE cannot drift with the
    magnitude of the weights: a uniform weight of 2.0 leaves the loss value and
    every gradient UNCHANGED, and only a completion's weight RELATIVE to its
    group-mates affects the step. A weighted SUM would silently make the effective
    LR a function of how big the CVaR penalty happened to be - an unmeasured
    coupling between the risk term and the optimizer, which is exactly why the
    pinned default (lambda_cvar=0.0 -> weight 1.0) must stay a no-op.

    `weight=None` takes a SEPARATE branch that never multiplies by 1.0, so the
    unweighted objective stays bit-identical to the pre-weight implementation.
    """
    if logp.shape != ref_logp.shape or logp.shape != mask.shape:
        raise ValueError(f"shape mismatch: {tuple(logp.shape)} "
                         f"{tuple(ref_logp.shape)} {tuple(mask.shape)}")
    if adv.shape[0] != logp.shape[0]:
        raise ValueError("adv must be [B]")
    B = logp.shape[0]
    if active is None:
        active = torch.ones(B, device=logp.device, dtype=logp.dtype)
    active = active.to(logp.dtype).reshape(B)
    base = (mask.to(logp.dtype) * active[:, None])
    if weight is None:
        # Identity: NO multiply is issued (multiplying by 1.0 would be exact anyway,
        # but a branch with no arithmetic is a stronger claim than "1.0 is exact").
        m = base
        w_mean = 1.0
    else:
        w = torch.as_tensor(weight).to(device=logp.device,
                                       dtype=logp.dtype).reshape(-1)
        if int(w.shape[0]) != B:
            raise ValueError(f"weight must be [B={B}], got shape {tuple(w.shape)}")
        if not bool(torch.isfinite(w).all()):
            raise ValueError("weight contains a non-finite entry (inf/nan); a "
                             "non-finite loss weight would poison the gradient - "
                             "refusing rather than silently substituting 1.0")
        if bool((w < 0).any()):
            raise ValueError("weight contains a negative entry; a negative loss "
                             "weight is not a weighted mean (it would invert the "
                             "advantage of that completion) - refusing")
        m = base * w[:, None]
        w_mean = float(w.detach().mean())
    denom = m.sum().clamp(min=1.0)

    a = adv.to(logp.dtype).reshape(B)
    if cfg.normalize_advantage:
        sel = active > 0
        if int(sel.sum()) > 1:
            mu = a[sel].mean()
            sd = a[sel].std(unbiased=False)
            if float(sd) > 1e-6:
                a = (a - mu) / (sd + 1e-6)
    a = a.clamp(-cfg.adv_clip, cfg.adv_clip)

    pg = -(a[:, None] * logp * m).sum() / denom
    kl_tok = k3_kl(logp, ref_logp).clamp(max=cfg.kl_clip) * m
    kl = kl_tok.sum() / denom
    loss = pg + cfg.kl_coef * kl
    with torch.no_grad():
        stats = {
            "loss": float(loss.detach()),
            "policy_term": float(pg.detach()),
            "kl_term": float(kl.detach()),
            "n_selected_tokens": int(base.sum().item()),
            "n_active_samples": int((active > 0).sum().item()),
            "n_samples": int(B),
            "loss_weight_mean": w_mean,
            "adv_mean": float(a.detach().mean()) if B else 0.0,
            "adv_absmax": float(a.detach().abs().max()) if B else 0.0,
        }
    return loss, stats


def assemble_advantages(records: list, parsed_actions: list, *,
                        min_usable: int = 2) -> list:
    """Per-completion advantage from the precomputed counterfactual group.

    `records[i]` is one RL record (has `group` and `advantages`);
    `parsed_actions[i]` is the action parsed out of completion i, or None.
    Returns [{'adv': float|None, 'active': bool, 'action': str|None, 'reason': str}]
    A completion that did not parse, or whose action is not a scored arm, is
    INACTIVE - never assigned a fabricated advantage.
    """
    out = []
    for rec, act in zip(records, parsed_actions):
        if act is None:
            out.append({"adv": None, "active": False, "action": None,
                        "reason": "unparseable_completion"})
            continue
        adv = (rec.get("advantages") or {}).get(act)
        if adv is None:
            out.append({"adv": None, "active": False, "action": act,
                        "reason": "action_not_in_scored_group"})
            continue
        out.append({"adv": float(adv), "active": True, "action": act,
                    "reason": None})
    n = sum(1 for o in out if o["active"])
    if n < min_usable:
        for o in out:
            if o["active"]:
                o["active"] = False
                o["reason"] = f"only {n} usable completions (<{min_usable})"
    return out


_ACTIONS = ("BUY", "WATCH", "SKIP")

# The RL ARM SPACE. A BUY is not one action any more: the size IS part of the
# decision, so the arm is (action, tier). WATCH/SKIP carry no size.
ARMS = ("SKIP", "WATCH", "BUY_SMALL", "BUY_MID", "BUY_FULL")
SIZE_TEXT_TO_TIER = {"SMALL": "SMALL", "MID": "MID", "FULL": "FULL",
                     "0.25": "SMALL", "0.5": "MID", "0.50": "MID",
                     "1.0": "FULL", "1.00": "FULL", "1": "FULL"}


def parse_size(text: str):
    """Tier from a SIZE field, or None when it is absent/unknown/unpriceable."""
    import re
    m = re.search(r"^SIZE\s*[:\-\u2014]\s*([A-Za-z0-9.]+)", text or "", re.M)
    if not m:
        return None
    tok = m.group(1).strip().upper()
    if tok in ("NONE", "NA", "N/A"):
        return None
    return SIZE_TEXT_TO_TIER.get(tok)


def parse_decision(text: str):
    """Pull the ARM out of a completion: DECISION (+ SIZE when BUY).

    Returns None if it does not parse - the caller must MASK, never guess and never
    default to a value. A BUY without a usable SIZE does NOT parse: the size is part
    of the decision, and masking an unstated size is honest where defaulting one
    would invent a bet the policy never made.
    """
    import re
    if not text:
        return None
    m = re.search(r"DECISION\s*[:\-\u2014]\s*([A-Za-z_]+)", text)
    if not m:
        return None
    a = m.group(1).strip().upper()
    if a not in _ACTIONS:
        for cand in _ACTIONS:                 # tolerate DECISION: buy_...
            if a.startswith(cand):
                a = cand
                break
        else:
            return None
    if a in ("SKIP", "WATCH"):
        return a
    tier = parse_size(text)
    return ("BUY_" + tier) if tier else None


def _selftest() -> int:
    torch.manual_seed(0)
    fails, checks = [], 0

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            fails.append(tag)

    B, T = 4, 12
    ref = -torch.rand(B, T) * 2.0
    # I4: k3 KL is non-negative
    logp = ref - 0.3
    kl = k3_kl(logp, ref)
    chk("I4_kl_nonneg", bool((kl >= -1e-6).all()))
    chk("I4_kl_zero_when_equal", float(k3_kl(ref, ref).abs().max()) < 1e-6)

    # I1 + I5: gradient is exactly zero where the mask is zero
    mask = torch.zeros(B, T)
    mask[:, 3:5] = 1.0                        # only the DECISION span
    adv = torch.tensor([2.0, -1.0, 0.5, -1.5])
    x = (ref - 0.4).clone().requires_grad_(True)
    loss, st = grpo_loss(x, ref, adv, mask)
    loss.backward()
    g = x.grad * mask
    chk("I1_grad_only_on_selected", float(x.grad[~mask.bool()].abs().max()) < 1e-9)
    chk("I1_grad_present_when_selected", float(g.abs().max()) > 1e-9)

    # I5: appending unmasked tokens does not change the loss value
    big = torch.cat([x.detach(), -torch.rand(B, 6) * 3.0], dim=1)
    big_ref = torch.cat([ref, -torch.rand(B, 6) * 3.0], dim=1)
    big_mask = torch.cat([mask, torch.zeros(B, 6)], dim=1)
    l2, _ = grpo_loss(big, big_ref, adv, big_mask)
    chk("I5_length_invariant", abs(float(l2) - float(loss.detach())) < 1e-5)

    # I2: an inactive sample contributes nothing at all
    act = torch.tensor([1.0, 0.0, 1.0, 1.0])
    z = (ref - 0.7).clone().requires_grad_(True)
    l3, st3 = grpo_loss(z, ref, adv, mask, active=act)
    l3.backward()
    chk("I2_inactive_no_grad", float(z.grad[1].abs().max()) < 1e-12)
    chk("I2_inactive_counted", st3["n_active_samples"] == 3)
    z2 = z.detach().clone(); z2[1] = z2[1] + 5.0     # perturb the inactive row
    l4, _ = grpo_loss(z2, ref, adv, mask, active=act)
    chk("I2_inactive_invariant", abs(float(l4) - float(l3.detach())) < 1e-9)

    # ---- I6: the per-completion loss weight is a WEIGHTED MEAN ----------------
    # Reference: the objective EXACTLY AS IT WAS before `weight` existed. The
    # pinned default (lambda_cvar=0.0 -> weight 1.0 -> weight=None) must be an
    # EXACT no-op, so the None path is compared with torch.equal, not a tolerance:
    # "multiply by 1.0" is not the claim, "no arithmetic at all" is.
    def _ref_unweighted(logp_, ref_, adv_, mask_, active_=None,
                        cfg_=DEFAULT_LOSS_CONFIG):
        b = logp_.shape[0]
        if active_ is None:
            active_ = torch.ones(b, device=logp_.device, dtype=logp_.dtype)
        active_ = active_.to(logp_.dtype).reshape(b)
        mm = (mask_.to(logp_.dtype) * active_[:, None])
        dd = mm.sum().clamp(min=1.0)
        aa = adv_.to(logp_.dtype).reshape(b)
        if cfg_.normalize_advantage:
            sel_ = active_ > 0
            if int(sel_.sum()) > 1:
                mu_ = aa[sel_].mean()
                sd_ = aa[sel_].std(unbiased=False)
                if float(sd_) > 1e-6:
                    aa = (aa - mu_) / (sd_ + 1e-6)
        aa = aa.clamp(-cfg_.adv_clip, cfg_.adv_clip)
        pg_ = -(aa[:, None] * logp_ * mm).sum() / dd
        kl_ = (k3_kl(logp_, ref_).clamp(max=cfg_.kl_clip) * mm).sum() / dd
        return pg_ + cfg_.kl_coef * kl_

    def _raises(fn):
        try:
            fn()
        except (ValueError, SystemExit):
            return True
        return False

    actw = torch.tensor([1.0, 0.0, 1.0, 1.0])
    xw = (ref - 0.25).clone()
    l_now, st_w = grpo_loss(xw, ref, adv, mask, active=actw)
    chk("I6_weight_none_bit_identical",
        torch.equal(l_now.detach(),
                    _ref_unweighted(xw, ref, adv, mask, actw).detach()))
    # the backward must be bit-identical too: a value-only check would miss a term
    # that enters the gradient and cancels in the forward.
    xg = (ref - 0.25).clone().requires_grad_(True)
    lg, _ = grpo_loss(xg, ref, adv, mask, active=actw)
    lg.backward()
    xr = (ref - 0.25).clone().requires_grad_(True)
    _ref_unweighted(xr, ref, adv, mask, actw).backward()
    chk("I6_weight_none_grad_bit_identical", torch.equal(xg.grad, xr.grad))
    chk("I6_stats_report_unit_weight", float(st_w["loss_weight_mean"]) == 1.0)
    # a UNIFORM weight leaves value AND gradient unchanged (the weighted MEAN
    # normalises the magnitude out). A weighted SUM would double the value here.
    lu, _ = grpo_loss(xw, ref, adv, mask, active=actw,
                      weight=torch.full((4,), 2.0))
    chk("I6_uniform_weight_value_invariant",
        abs(float(lu.detach()) - float(l_now.detach())) < 1e-6)
    xu = (ref - 0.25).clone().requires_grad_(True)
    lu3, st_u = grpo_loss(xu, ref, adv, mask, active=actw,
                          weight=torch.full((4,), 3.0))
    lu3.backward()
    chk("I6_uniform_weight_grad_invariant",
        float((xu.grad - xg.grad).abs().max()) < 1e-6)
    chk("I6_stats_report_mean_weight", float(st_u["loss_weight_mean"]) == 3.0)
    # a NON-uniform weight must move the loss: the instrument is not inert.
    lnu, _ = grpo_loss(xw, ref, adv, mask, active=actw,
                       weight=torch.tensor([1.0, 1.0, 1.0, 5.0]))
    chk("I6_nonuniform_weight_moves_loss",
        abs(float(lnu.detach()) - float(l_now.detach())) > 1e-6)
    # refusals: wrong shape, non-finite and negative must all be LOUD (never a
    # silent substitute of 1.0 or of the raw weight).
    chk("I6_wrong_shape_refuses",
        _raises(lambda: grpo_loss(xw, ref, adv, mask, weight=torch.ones(3))))
    chk("I6_nonfinite_refuses",
        _raises(lambda: grpo_loss(xw, ref, adv, mask,
                                  weight=torch.tensor([1.0, 1.0, float("nan"), 1.0]))))
    chk("I6_negative_refuses",
        _raises(lambda: grpo_loss(xw, ref, adv, mask,
                                  weight=torch.tensor([1.0, 1.0, 1.0, -1.0]))))

    # I3: counterfactual advantages sum to zero, and BUY wins only if it is best
    import sys, os
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from rl_reward_v3 import counterfactual_stats
    adv3, stt = counterfactual_stats({"SKIP": 0.0, "WATCH": 0.0, "BUY": 0.25})
    chk("I3_advantages_zero_sum", abs(sum(adv3.values())) < 1e-9)
    chk("I3_best_arm_has_max_advantage", stt["best_action"] == "BUY"
        and adv3["BUY"] == max(adv3.values()))
    chk("I3_skip_positive_when_buy_negative",
        counterfactual_stats({"SKIP": 0.0, "WATCH": 0.0, "BUY": -0.25})[0]["SKIP"] > 0)

    # action-level KL (the anchor that IS computable at rollout time)
    import torch.nn.functional as _F
    pl = torch.tensor([[2.0, 0.5, 0.1], [0.1, 0.2, 3.0]], dtype=torch.float64)
    rp = _F.softmax(torch.tensor([[2.0, 0.5, 0.1], [0.1, 0.2, 3.0]],
                                 dtype=torch.float64), dim=-1)
    k = action_kl(pl, rp)
    chk("AKL_zero_when_equal", float(k.abs().max()) < 1e-6)   # float32 roundoff
    chk("AKL_nonneg", bool((action_kl(torch.tensor([[0.0, 0.0, 0.0]],
                                                   dtype=torch.float64), rp[:1]) >= -1e-12).all()))
    chk("AKL_positive_when_different",
        float(action_kl(torch.tensor([[0.0, 0.0, 5.0]], dtype=torch.float64), rp[:1])[0]) > 0.1)
    ll = torch.tensor([[2.0, 0.5, 0.1]], requires_grad=True)
    action_kl(ll, rp[:1]).backward()
    chk("AKL_differentiable", ll.grad is not None and float(ll.grad.abs().sum()) > 1e-9)

    # parse + assembly
    chk("parse_plain", parse_decision("DECISION: BUY\nSIZE: MID") == "BUY_MID")
    chk("parse_numeric_size", parse_decision("DECISION: BUY\nSIZE: 0.5") == "BUY_MID")
    chk("parse_full", parse_decision("DECISION: BUY\nSIZE: 1.0") == "BUY_FULL")
    chk("parse_buy_without_size_is_none",
        parse_decision("DECISION: BUY\nPRICE LIMIT: 1") is None)
    chk("parse_unknown_size_is_none", parse_decision("DECISION: BUY\nSIZE: 0.4") is None)
    chk("parse_size_none_is_none", parse_decision("DECISION: BUY\nSIZE: NONE") is None)
    chk("parse_em_dash", parse_decision("DECISION \u2014 SKIP") == "SKIP")

    # ---- arm joint: the size-aware KL anchor ---------------------------------
    al = torch.tensor([[0.4, -1.0, 0.9]], requires_grad=True)    # BUY, WATCH, SKIP
    sl = torch.tensor([[0.2, 0.1, -0.3]], requires_grad=True)    # SMALL, MID, FULL
    joint = assemble_arm_probs(al, sl)
    chk("joint_shape", tuple(joint.shape) == (1, 5))
    chk("joint_sums_to_one", abs(float(joint.detach().sum()) - 1.0) < 1e-5)
    p_buy = float(torch.softmax(al.detach(), -1)[0, 0])
    chk("joint_recovers_buy_marginal",
        abs(float(joint[0, 2:].sum()) - p_buy) < 1e-6)
    chk("joint_skip_is_action_skip",
        abs(float(joint[0, 0]) - float(torch.softmax(al.detach(), -1)[0, 2])) < 1e-6)
    # self-anchored KL is exactly 0 (softmax(log(joint)) == joint)
    chk("joint_kl_self_is_zero",
        float(action_kl(joint_arm_logits(al, sl), joint.detach())) < 1e-6)
    # a different reference is a strictly positive KL, and it is a real gradient
    other = torch.tensor([[0.7, 0.1, 0.05, 0.1, 0.05]])
    kl = action_kl(joint_arm_logits(al, sl), other)
    chk("joint_kl_positive_for_other_ref", float(kl) > 1e-4)
    kl.backward()
    chk("joint_kl_grad_flows_to_action_slot",
        al.grad is not None and float(al.grad.abs().sum()) > 1e-9)
    chk("joint_kl_grad_flows_to_size_slot",
        sl.grad is not None and float(sl.grad.abs().sum()) > 1e-9)
    # a degenerate size basis must not silently produce a valid-looking anchor
    bad = False
    try:
        assemble_arm_probs(al.detach(), torch.zeros(1, 2))
    except SystemExit:
        bad = True
    chk("joint_wrong_size_basis_refuses", bad)

    chk("parse_garbage_is_none", parse_decision("I would buy this") is None)
    a = assemble_advantages(
        [{"advantages": {"BUY": 0.1, "SKIP": -0.05}}, {"advantages": {"BUY": 0.1}},
         {"advantages": {"SKIP": -0.05}}],
        ["BUY", None, "WATCH"], min_usable=1)
    chk("assemble_masks_unparsed", a[1]["active"] is False)
    chk("assemble_masks_unscored", a[2]["active"] is False)
    chk("assemble_keeps_valid", a[0]["active"] and abs(a[0]["adv"] - 0.1) < 1e-12)
    print(json.dumps({"suite": "grpo_loss", "checks": checks, "failed": fails,
                      "verdict": "PASS" if not fails else "FAIL"}, indent=1))
    return 1 if fails else 0


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-check", action="store_true")
    a = ap.parse_args()
    if a.self_check:
        raise SystemExit(_selftest())
    print(json.dumps(DEFAULT_LOSS_CONFIG.as_dict(), indent=1))
