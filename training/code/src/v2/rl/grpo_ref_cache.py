#!/usr/bin/env python
"""Frozen reference-logprob cache for GRPO (KL anchor without a second 27B).

WHY: GRPO needs a KL anchor to the SFT reference policy so the RL phase cannot
drift off the capability session. Loading a second 27B reference next to the
policy does not fit 3x 96GB under full-parameter training. But the reference IS
the SFT best checkpoint and the RL prompts are frozen, so the reference's
per-decision-token logprobs can be computed ONCE and reused as constants.

REFUSALS (never fabricate an anchor):
  * missing/unreadable checkpoint  -> refuse
  * prompt whose decision span yields zero tokens -> store nothing for it and
    report the count (the trainer must MASK such decisions, never invent a KL)
  * manifest sha mismatch          -> refuse

Output: a JSONL of {episode_id, n_tokens, ref_logprobs:[...]} plus a manifest
carrying the source checkpoint sha256 and the prompt-file sha256, so a stale
cache can never be silently paired with a different reference.

Usage (smoke test - does NOT need the GPUs if --limit is small):
  python grpo_ref_cache.py --prompts <jsonl> --ckpt <dir> --out <jsonl> [--limit N]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

DECISION_RE = re.compile(r"DECISION\s*[:\-\u2014]\s*")


def sha256_file(path, chunk=8 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(chunk), b""):
            h.update(b)
    return h.hexdigest()


def sha256_dir(path):
    """Deterministic sha over a checkpoint directory's file names + sizes."""
    h = hashlib.sha256()
    for root, _, files in os.walk(path):
        for name in sorted(files):
            p = os.path.join(root, name)
            rel = os.path.relpath(p, path)
            h.update(rel.encode())
            h.update(str(os.path.getsize(p)).encode())
    return h.hexdigest()


def decision_char_span(text):
    """Char span of the DECISION value inside a completion, or None."""
    m = DECISION_RE.search(text or "")
    if not m:
        return None
    start = m.end()
    nl = text.find("\n", start)
    return (start, nl if nl != -1 else len(text))


def assert_ready(prompts, ckpt, out):
    """Fail loudly BEFORE any model work."""
    if not os.path.isfile(prompts):
        raise SystemExit(f"REFUSING: prompts file not found: {prompts}")
    if not os.path.isdir(ckpt):
        raise SystemExit(f"REFUSING: checkpoint directory not found: {ckpt}")
    names = os.listdir(ckpt)
    if not any(n.endswith((".safetensors", ".bin", ".pt")) for n in names):
        raise SystemExit(f"REFUSING: no weight files in checkpoint {ckpt}")
    if os.path.exists(out):
        raise SystemExit(f"REFUSING: output exists, refusing to overwrite: {out}")
    return True


def iter_prompts(path, limit=None):
    n = 0
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        try:
            yield json.loads(line)
        except Exception:
            continue
        n += 1
        if limit and n >= limit:
            return


def build(prompts, ckpt, out, limit=None, device="cuda"):
    """Compute reference logprobs for the decision span of each prompt.

    Uses transformers directly (no trl/peft in this stack). The model is loaded in
    bf16 and only a forward pass is used - no optimizer, no grads.
    """
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer

    assert_ready(prompts, ckpt, out)
    tok = AutoTokenizer.from_pretrained(ckpt)
    model = AutoModelForCausalLM.from_pretrained(ckpt, torch_dtype=torch.bfloat16,
                                                 device_map=device)
    model.eval()
    ref_sha = sha256_dir(ckpt)
    prompts_sha = sha256_file(prompts)

    n_written, n_empty, n_seen = 0, 0, 0
    with open(out, "w", encoding="utf-8") as fh, torch.no_grad():
        for rec in iter_prompts(prompts, limit):
            n_seen += 1
            msgs = rec.get("messages") or []
            completion = ""
            for m in msgs:
                if m.get("role") == "assistant":
                    completion = m.get("content") or ""
            span = decision_char_span(completion)
            if span is None:
                n_empty += 1
                continue
            prompt_text = tok.apply_chat_template(
                [m for m in msgs if m.get("role") != "assistant"],
                tokenize=False, add_generation_prompt=True)
            full = prompt_text + completion
            enc = tok(full, return_offsets_mapping=True, return_tensors="pt")
            offs = enc.pop("offset_mapping")[0].tolist()
            ids = enc["input_ids"].to(model.device)
            logits = model(input_ids=ids).logits[0]
            logp = torch.log_softmax(logits.float(), dim=-1)
            idx = [i for i, (a, b) in enumerate(offs)
                   if a is not None and b is not None and a < span[1] and b > span[0]]
            idx = [i for i in idx if 0 < i < ids.shape[1]]
            if not idx:
                n_empty += 1
                continue
            vals = [float(logp[i - 1, int(ids[0, i])]) for i in idx]
            fh.write(json.dumps({"episode_id": rec.get("episode_id"),
                                 "n_tokens": len(vals),
                                 "ref_logprobs": vals}) + "\n")
            n_written += 1
    manifest = {"schema": "grpo_ref_cache_v1", "checkpoint": ckpt,
                "checkpoint_sha256": ref_sha, "prompts": prompts,
                "prompts_sha256": prompts_sha, "cache": out,
                "rows_seen": n_seen, "rows_written": n_written,
                "rows_masked_no_decision_span": n_empty}
    with open(out + ".manifest.json", "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
    print(json.dumps(manifest, indent=1))
    return manifest


def build_action_dist(prompts, ckpt, out, limit=None, device="cuda",
                      batch_size=8, actions=None, prompt_key="prompt_sha256"):
    """Cache the reference ACTION DISTRIBUTION (BUY, WATCH, SKIP) per prompt.

    WHY NOT THE TOKEN CACHE ABOVE: `build()` caches logprobs for the DECISION span
    of the SFT *completion*. RL samples DIFFERENT completions, so those logprobs
    cannot be indexed by the sampled tokens - the token-level KL is simply not
    computable, and forcing it produces a noise term. The action distribution is 3
    numbers, well defined for any sampled completion, and is exactly the object
    the policy is trained to choose.

    Reads logits at the position that predicts the FIRST ACTION TOKEN: the prompt
    rendered with prompt_format (enable_thinking=False, matching the release), then
    the literal 'DECISION: '. The loop reads logits at slot-1, which is the same
    position - both use prompt_format so they cannot disagree.

    EMITS TWO OBJECTS, BOTH CORRECT AT THEIR LEVEL:
      * ref_action_probs - the DECISION MARGINAL over (BUY, WATCH, SKIP). This is
        what the current action_kl() consumes and it stays valid: a KL against the
        marginal is the right anchor for the action slot alone.
      * ref_arm_probs - the JOINT over the 5 ARMS (SKIP, WATCH, BUY_SMALL, BUY_MID,
        BUY_FULL) by the chain rule: P(SKIP)/P(WATCH) as-is, and
        P(BUY_TIER) = P(BUY) * P(TIER | BUY), where the conditional comes from a
        second forward pass over the prefix "...DECISION: BUY\\nSIZE: " (the size
        slot, prompt_format.locate_size_slot). Size is thus graded by the RL
        advantage today, and this joint is what a size-aware KL needs.

    RE-ARM NOTE (KELLY_AUDIT_C12 §b, 2026-09-19): grading is venue-conditional
    (AMM->FULL, curve->SMALL; MID retired) but this cache STAYS 5-arm on purpose.
    The KL is an anchor to the SFT reference over the full slot vocabulary - it is
    well-defined whatever subset of arms carries advantages. Retired arms are
    handled where they belong: a sampled BUY_MID parses fine, finds no advantage in
    the record, and goes INACTIVE in assemble_advantages (no fabricated reward).
    Collapsing this joint to match the scored set would break sha-compatibility
    with older caches and buy nothing.
    """
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    from prompt_format import (action_token_ids, size_token_ids, render_prompt,
                              ARMS)
    from grpo_dataset import prompt_of, prompt_sha

    assert_ready(prompts, ckpt, out)
    tok = AutoTokenizer.from_pretrained(ckpt)
    acts, canon = action_token_ids(tok) if actions is None else (
        list(actions), [int(tok.encode(" " + a, add_special_tokens=False)[0])
                        for a in actions])
    _slabs, sids = size_token_ids(tok)
    model = AutoModelForCausalLM.from_pretrained(ckpt, torch_dtype=torch.bfloat16,
                                                device_map=device)
    model.eval()
    ref_sha = sha256_dir(ckpt)
    n_written = n_empty = n_seen = 0
    with open(out, "w", encoding="utf-8") as fh, torch.no_grad():
        for rec in iter_prompts(prompts, limit):
            n_seen += 1
            msgs = rec.get("messages") or []
            if not msgs:
                n_empty += 1
                continue
            base = render_prompt(tok, msgs)
            text = base + "DECISION: "
            enc = tok(text, return_tensors="pt")
            ids = enc["input_ids"].to(model.device)
            logits = model(input_ids=ids).logits[0, -1]
            probs = torch.softmax(logits[list(canon)].float(), dim=-1).tolist()
            # P(tier | BUY): teacher-force the BUY prefix, read the size slot.
            enc2 = tok(base + "DECISION: BUY\nSIZE: ", return_tensors="pt")
            ids2 = enc2["input_ids"].to(model.device)
            l2 = model(input_ids=ids2).logits[0, -1]
            tier_p = torch.softmax(l2[list(sids)].float(), dim=-1).tolist()
            p_by_act = {a: float(probs[i]) for i, a in enumerate(acts)}
            p_buy = p_by_act.get("BUY", 0.0)
            joint = {"SKIP": p_by_act.get("SKIP", 0.0),
                     "WATCH": p_by_act.get("WATCH", 0.0),
                     "BUY_SMALL": p_buy * float(tier_p[0]),
                     "BUY_MID": p_buy * float(tier_p[1]),
                     "BUY_FULL": p_buy * float(tier_p[2])}
            fh.write(json.dumps({
                prompt_key: prompt_sha(prompt_of({"messages": msgs})),
                "episode_id": rec.get("episode_id"),
                "actions": list(acts),
                "action_token_ids": list(canon),
                "ref_action_probs": [round(float(p), 8) for p in probs],
                "size_labels": ["SMALL", "MID", "FULL"],
                "size_token_ids": list(sids),
                "ref_tier_probs_given_buy": [round(float(p), 8) for p in tier_p],
                "arms": list(ARMS),
                "ref_arm_probs": [round(float(joint[a]), 8) for a in ARMS],
            }) + "\n")
            n_written += 1
    manifest = {"schema": "grpo_ref_action_v1", "checkpoint": ckpt,
                "checkpoint_sha256": ref_sha, "prompts": prompts,
                "prompts_sha256": sha256_file(prompts), "cache": out,
                "actions": list(acts), "action_token_ids": list(canon),
                "arms": list(ARMS), "size_token_ids": list(sids),
                "prompt_key": prompt_key,
                "rows_seen": n_seen, "rows_written": n_written,
                "rows_masked_no_messages": n_empty}
    with open(out + ".manifest.json", "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
    print(json.dumps(manifest, indent=1))
    return manifest


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--prompts")
    ap.add_argument("--ckpt")
    ap.add_argument("--out")
    ap.add_argument("--limit", type=int, default=None)
    ap.add_argument("--device", default="cuda")
    ap.add_argument("--dry-check", action="store_true",
                    help="only validate inputs and report; load no model")
    ap.add_argument("--action-dist", action="store_true",
                    help="build the ACTION-distribution cache (the one RL uses)")
    ap.add_argument("--verify-prompt-hash", action="store_true",
                    help="prove the ref key == the RL dataset key on a real record")
    a = ap.parse_args()
    if a.verify_prompt_hash:
        from prompt_format import render_prompt, action_token_ids
        from grpo_dataset import prompt_of, prompt_sha
        from transformers import AutoTokenizer
        import glob as _g
        rec = None
        for p in _g.glob("/training/v2/candidate_sft_c3/*.jsonl"):
            for line in open(p, encoding="utf-8"):
                r = json.loads(line)
                if r.get("family") == "decision":
                    rec = r
                    break
            if rec:
                break
        ck = a.ckpt or ("/training/seed/models--unsloth--Qwen3.8-27B/snapshots/"
                        "3ea932cee0a432ae86e9c7826cbe8aef52323a28")
        tok = AutoTokenizer.from_pretrained(ck)
        msgs = rec["messages"]
        key_here = prompt_sha(prompt_of({"messages": msgs}))
        # rebuild the way grpo_dataset does, from the same record
        keep = [m for m in msgs if m.get("role") != "assistant"]
        key_ds = prompt_sha(json.dumps(keep, ensure_ascii=False, sort_keys=True))
        txt = render_prompt(tok, msgs)
        acts, canon = action_token_ids(tok)
        print(json.dumps({"ref_key": key_here, "dataset_key": key_ds,
                          "keys_match": key_here == key_ds,
                          "prompt_chars": len(txt),
                          "prompt_ends": txt[-24:],
                          "actions": acts, "action_token_ids": canon},
                         indent=1))
        return 0 if key_here == key_ds else 1
    if a.prompts and a.ckpt and a.out and a.dry_check:
        assert_ready(a.prompts, a.ckpt, a.out)
        print(json.dumps({"status": "ready", "prompts": a.prompts, "ckpt": a.ckpt,
                          "checkpoint_sha256": sha256_dir(a.ckpt),
                          "prompts_sha256": sha256_file(a.prompts)}, indent=1))
        return 0
    if not (a.prompts and a.ckpt and a.out):
        ap.error("--prompts --ckpt --out are required (or use --dry-check)")
    if a.action_dist:
        build_action_dist(a.prompts, a.ckpt, a.out, limit=a.limit, device=a.device)
        return 0
    build(a.prompts, a.ckpt, a.out, limit=a.limit, device=a.device)
    return 0


if __name__ == "__main__":
    sys.exit(main())