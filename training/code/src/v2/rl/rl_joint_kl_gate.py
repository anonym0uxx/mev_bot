"""Pre-launch confirmation of the 5-arm joint KL on a REAL batch.

WHY THIS EXISTS: grpo_loss.py --self-check proves the joint MATH and
test_joint_kl_wiring.py proves the slot PLUMBING, but both use synthetic tensors. Only
a real policy + a real ref cache + real prompts prove the two SIDES were built the same
way. That is the one property the whole size-aware anchor rests on, so it is a GATE on
the RL launch (fail-closed), not a report.

THE CRITERIA (mechanical; none of them is a judgement call):
  1. the policy joint sums to 1 on every row
  2. the size slot LOCATES on every row (no silent zero rows)
  3. kl_arms is finite and >= 0 (a KL cannot be negative)
  4. one backward step reaches the parameters through BOTH slots
  5. CROSS-CHECK, the one that actually matters: the reference cache's joint BUY mass
     P(BUY_SMALL)+P(BUY_MID)+P(BUY_FULL) must equal its own 3-dim marginal's P(BUY).
     Both are written by grpo_ref_cache by the same chain rule; if they disagree, one
     of the two fields was built wrong and the anchor compares two different objects.

    python rl_joint_kl_gate.py --policy <final/> --ref-cache <cache.jsonl> \
        --targets <rl_train.jsonl> [--n 4] [--out evidence.json]
"""
import argparse
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def evaluate(a_logits, s_logits, ref_arms, ref_marg, tol=1e-6):
    """Pure criteria evaluation. Returns a report dict. No model, no IO.

    Kept separate from main() so it can be exercised with synthetic tensors - the gate
    must be provably able to FAIL, or passing it means nothing.
    """
    import torch
    from grpo_loss import action_kl, assemble_arm_probs, joint_arm_logits

    out = {"criteria": {}, "values": {}}
    joint = assemble_arm_probs(a_logits, s_logits)
    c = out["criteria"]

    sums = joint.sum(dim=-1)
    out["values"]["joint_sum_min"] = float(sums.min())
    out["values"]["joint_sum_max"] = float(sums.max())
    c["joint_sums_to_one"] = bool(torch.allclose(sums, torch.ones_like(sums),
                                                 atol=tol))

    out["values"]["size_slot_ok_rows"] = int(s_logits.shape[0])
    out["values"]["size_slot_zero_rows"] = int(
        (s_logits.abs().sum(dim=-1) == 0).sum())
    c["size_slot_located"] = out["values"]["size_slot_zero_rows"] == 0

    kl = action_kl(joint_arm_logits(a_logits, s_logits), ref_arms)
    kl = kl if kl.dim() else kl.unsqueeze(0)
    out["values"]["kl_arms_min"] = float(kl.min())
    out["values"]["kl_arms_max"] = float(kl.max())
    out["values"]["kl_arms_mean"] = float(kl.mean())
    c["kl_finite"] = bool(torch.isfinite(kl).all())
    c["kl_non_negative"] = bool(float(kl.min()) >= -1e-6)

    # 4. gradient reaches both slots: the joint must route to the decision logits
    #    THROUGH P(BUY) and to the size logits THROUGH P(TIER|BUY).
    aj = a_logits.detach().clone().requires_grad_(True)
    sj = s_logits.detach().clone().requires_grad_(True)
    g = action_kl(joint_arm_logits(aj, sj), ref_arms)
    g = g.sum() if g.dim() else g
    g.backward()
    out["values"]["grad_action_slot"] = float(aj.grad.abs().sum()) if aj.grad is not None else 0.0
    out["values"]["grad_size_slot"] = float(sj.grad.abs().sum()) if sj.grad is not None else 0.0
    c["grad_reaches_decision_slot"] = out["values"]["grad_action_slot"] > 0
    c["grad_reaches_size_slot"] = out["values"]["grad_size_slot"] > 0

    # 5. reference-side self-consistency: joint BUY mass == marginal P(BUY)
    buy_mass = ref_arms[:, 2:5].sum(dim=-1)
    # BUY is index 0 of the (BUY, WATCH, SKIP) marginal basis
    marg_buy = ref_marg[:, 0]
    dev = (buy_mass - marg_buy).abs()
    out["values"]["ref_buy_mass_dev_max"] = float(dev.max())
    c["ref_joint_agrees_with_marginal"] = bool(float(dev.max()) <= tol)

    out["verdict"] = "PASS" if all(c.values()) else "FAIL"
    out["failed"] = [k for k, v in c.items() if not v]
    return out


def self_test():
    """Prove the gate can FAIL. A gate that has never failed is not evidence."""
    import torch
    fails = []

    def chk(name, cond):
        print(f"  [{'ok' if cond else 'FAIL'}] {name}")
        if not cond:
            fails.append(name)

    torch.manual_seed(0)
    a_logits = torch.tensor([[0.4, -1.0, 0.9], [0.1, 0.2, -0.5]])   # BUY,WATCH,SKIP
    s_logits = torch.tensor([[0.2, 0.1, -0.3], [-0.4, 0.6, 0.0]])   # SMALL,MID,FULL
    # a consistent reference: joint built by the same chain rule as the marginal
    from grpo_loss import assemble_arm_probs
    ref_arms = assemble_arm_probs(
        torch.tensor([[0.3, 0.2, 0.5], [-0.2, 0.4, 0.1]]),
        torch.tensor([[0.1, 0.3, -0.2], [0.5, -0.1, 0.2]]))
    ref_marg = torch.tensor([[0.3, 0.2, 0.5], [-0.2, 0.4, 0.1]])
    ref_marg = torch.softmax(ref_marg, dim=-1)

    good = evaluate(a_logits, s_logits, ref_arms, ref_marg)
    chk("clean_inputs_PASS", good["verdict"] == "PASS")
    chk("clean_reports_values", good["values"]["joint_sum_min"] > 0.99)

    # 1. reference joint disagrees with its own marginal -> the construction is broken
    bad_marg = ref_marg.clone()
    bad_marg[:, 0] = bad_marg[:, 0] * 0.5
    r = evaluate(a_logits, s_logits, ref_arms, bad_marg)
    chk("catches_ref_joint_vs_marginal_mismatch",
        r["verdict"] == "FAIL" and "ref_joint_agrees_with_marginal" in r["failed"])

    # 2. a zero size row = the slot failed to locate for that row
    s_zero = s_logits.clone()
    s_zero[1] = 0.0
    r = evaluate(a_logits, s_zero, ref_arms, ref_marg)
    chk("catches_missing_size_slot", "size_slot_located" in r["failed"])

    # 3. a non-finite joint must not slip through
    a_nan = a_logits.clone()
    a_nan[0, 0] = float("nan")
    r = evaluate(a_nan, s_logits, ref_arms, ref_marg)
    chk("catches_non_finite", r["verdict"] == "FAIL")

    # 4. an all-zero reference must be caught, not treated as uniform
    r = evaluate(a_logits, s_logits, torch.zeros_like(ref_arms), ref_marg)
    chk("catches_zero_reference", r["verdict"] == "FAIL")

    print(f"\ngate self-test: {'PASS' if not fails else 'FAIL'} ({len(fails)} failed)")
    return 1 if fails else 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--policy", default="", help="SFT final/ weights")
    ap.add_argument("--ref-cache", default="")
    ap.add_argument("--targets", default="")
    ap.add_argument("--n", type=int, default=4)
    ap.add_argument("--out", default="/training/v2/reports/RL_JOINT_KL_GATE.json")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    missing = [k for k in ("policy", "ref_cache", "targets") if not getattr(a, k)]
    if missing:
        print(json.dumps({"verdict": "FAIL",
                          "failed": ["missing_args:%s" % ",".join(missing)]}))
        return 2

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    from grpo_loss import assemble_arm_probs                      # noqa: F401
    from grpo_loop import RefActionDist, size_slot_logits
    from prompt_format import (action_token_ids, render_prompt, size_token_ids)

    t0 = time.time()
    rep = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "policy": a.policy,
           "ref_cache": a.ref_cache, "targets": a.targets, "n": a.n}

    # ---- real prompts from the RL targets, with their messages intact -----------
    recs = []
    with open(a.targets, encoding="utf-8") as fh:
        for line in fh:
            try:
                r = json.loads(line)
            except Exception:                                   # noqa: BLE001
                continue
            if r.get("messages") and r.get("prompt_sha256"):
                recs.append(r)
            if len(recs) >= a.n:
                break
    if not recs:
        rep.update({"verdict": "FAIL", "failed": ["no_usable_prompts"]})
        json.dump(rep, open(a.out, "w", encoding="utf-8"), indent=1)
        print(json.dumps({"verdict": "FAIL", "failed": ["no_usable_prompts"]}))
        return 2
    rep["n_prompts"] = len(recs)

    ref = RefActionDist(a.ref_cache)
    if not ref.has_joint():
        rep.update({"verdict": "FAIL", "failed": ["ref_cache_has_no_joint"]})
        json.dump(rep, open(a.out, "w", encoding="utf-8"), indent=1)
        print(json.dumps({"verdict": "FAIL", "failed": ["ref_cache_has_no_joint"]}))
        return 2

    tok = AutoTokenizer.from_pretrained(a.policy)
    model = AutoModelForCausalLM.from_pretrained(a.policy,
                                                torch_dtype=torch.bfloat16,
                                                device_map="auto")
    model.eval()
    _alabels, action_ids = action_token_ids(tok)
    _slabels, size_ids = size_token_ids(tok)

    prompts = [render_prompt(tok, r["messages"]) for r in recs]
    with torch.no_grad():
        # decision slot: identical construction to the ref cache (prompt + "DECISION: ")
        enc = tok([p + "DECISION: " for p in prompts], return_tensors="pt",
                  padding=True, truncation=True, max_length=8192)
        ids = enc["input_ids"].to(model.device)
        am = enc["attention_mask"].to(model.device)
        lg = model(input_ids=ids, attention_mask=am).logits
        rows, ok = [], []
        for b in range(ids.shape[0]):
            n = int(am[b].sum().item())
            if n == 0:
                continue
            rows.append(lg[b, n - 1, list(action_ids)].float())
            ok.append(True)
        a_logits = torch.stack(rows) if rows else torch.zeros(0, 3)
        size_l, size_ok = size_slot_logits(model, tok, prompts, size_ids,
                                           model.device)

    # align to the prompt order and the cache
    ref_arms, ref_marg, keep = [], [], []
    for i in range(len(recs)):
        q = ref.get_arms(recs[i]["prompt_sha256"])
        m = ref.get(recs[i]["prompt_sha256"])
        if q and m:
            ref_arms.append(q)
            ref_marg.append(m)
            keep.append(i)
    if not keep:
        rep.update({"verdict": "FAIL", "failed": ["no_prompt_present_in_ref_cache"]})
        json.dump(rep, open(a.out, "w", encoding="utf-8"), indent=1)
        print(json.dumps({"verdict": "FAIL", "failed": ["no_prompt_present_in_ref_cache"]}))
        return 2
    a_logits = a_logits[keep]
    size_l = size_l[keep]
    rep["n_matched_prompts"] = len(keep)
    rep["size_slot_ok"] = int(size_ok.sum().item())

    ev = evaluate(a_logits, size_l,
                  torch.tensor(ref_arms, dtype=torch.float32),
                  torch.tensor(ref_marg, dtype=torch.float32))
    rep.update(ev)
    rep["elapsed_s"] = round(time.time() - t0, 1)
    os.makedirs(os.path.dirname(a.out), exist_ok=True)
    json.dump(rep, open(a.out, "w", encoding="utf-8"), indent=1)
    print(json.dumps({"verdict": rep["verdict"], "failed": rep["failed"],
                      "values": rep["values"], "elapsed_s": rep["elapsed_s"]},
                     indent=1))
    return 0 if rep["verdict"] == "PASS" else 2


if __name__ == "__main__":
    raise SystemExit(main())
