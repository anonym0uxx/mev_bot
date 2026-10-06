#!/usr/bin/env python
"""grpo_loop — the RL driving loop. The only file that spends GPU time in RL.

WHAT IT DOES, per step:
  1. read a batch of prompts from the mint-stratified RL dataset
  2. sample `group_size` completions per prompt from the policy
  3. parse DECISION out of each completion (unparseable -> masked, never guessed)
  4. look up the PRECOMPUTED counterfactual advantage for that action
  5. score policy logprobs on the DECISION/SIZE span only (mask, I1)
  6. KL against the CACHED REFERENCE ACTION DISTRIBUTION (not token logprobs -
     the cached reference belongs to the SFT completion, not the sampled one)
  7. backward, clip, step; save/resume checkpoints
  8. evaluate on the FORWARD WALL and apply the preregistered gate

MODES
  --selfcheck   CPU-only: dataset + batch + advantage + loss on CPU tensors.
                Proves the wiring without a GPU or a model.
  --plan        print config + dataset counts + the resolve plan; no model.
  --train       the real loop (requires accelerate launch; 3 ranks).

REFUSALS ARE LOUD BY DESIGN: a missing reference cache, a wall leak, an empty
dataset or a missing init_from all abort before the first forward pass.

THREE IN-LOOP ADDITIONS (2026-09-21), ALL EXACT NO-OPS AT THE PINNED DEFAULTS:

  * GROUP LOSS WEIGHT. A record may carry `group_loss_weight` (the group's CVaR
    multiplier). It is read by grpo_trainer.read_group_loss_weight (absent -> the
    identity 1.0; malformed -> refusal), travels on the record exactly like
    `advantages`, and is handed to grpo_loss as a WEIGHTED-MEAN weight. When every
    weight is 1.0 (the pinned lambda_cvar=0.0 case) the argument is None and the
    loss is bit-identical to the unweighted objective.

  * CURRICULUM (easy-to-hard ORDERING), gated behind `loop.curriculum`, DEFAULT OFF
    (absent -> file order, the legacy uniform schedule). THE DIFFICULTY METRIC IS
    FROZEN AND DECLARED; it is a property of the PRECOMPUTED record, computable
    before any sampling, forward pass, advantage lookup or loss, depending on no
    model output and no training statistic:

        D(row) = max_a r_a - min_a r_a
        over a in the row's SCORED arms (= keys of row["advantages"]),
        with r_a = row["group"][a] the arm's precomputed counterfactual return.

    i.e. the ARM SPREAD - the discriminability among the decision's candidate
    counterfactual rewards. Easy = large spread (clearly separable arms, crisp
    advantage signal); hard = small spread (near-tied arms, marginal decision). The
    schedule is ASCENDING in D. A row whose D cannot be computed REFUSES loudly
    (reported by row, never imputed). The curriculum only PERMUTES rows: membership
    is identical to the legacy set, only the ORDER/grouping into gradient windows
    changes. `stages` cuts the ascending order into difficulty bands and epoch e
    starts at band min(e, stages-1), wrapping - every epoch still trains every row.

  * WALL-EVAL OBSERVABILITY (evaluate_wall): per-arm action HISTOGRAM, arm-distribution
    ENTROPY both raw (nats) and as a ratio against log(K) over the emittable arm basis
    (1.0 uniform, 0.0 collapsed), and a knowing-doing CONSISTENCY probe comparing the
    EMITTED action against the action implied by the completion's own rationale text.
    Unparseable cases are reported as their own counts and are NEVER counted as
    agreement. Observation only: sampling, advantages and the loss are untouched.

  * MEMORIZATION RECORDS FOR THE CANDIDATE UNDER SELECTION (SELF-FEEDING VETO), gated
    behind `selection.emit_memorization_records` (or `--emit-mem-records`), DEFAULT OFF.

    Why the switch exists. `memorization_precheck` scores the FIXED file
    reports/MEMORIZATION_RECORDS.jsonl. That file says "in-sample vs unseen, through
    the SAME policy", but a single fixed filename does not say WHICH policy produced
    it: a stale file left by an earlier checkpoint would be scored and its CLEAN
    verdict would silently authorise a DIFFERENT checkpoint than the one being
    selected. The instrument would still be fail-closed and still be answering the
    wrong question.

    THE CORRESPONDENCE GUARANTEE (what the switch turns on). When it is ON, the
    records are (re)emitted from the CANDIDATE POLICY ITSELF - the model object this
    process is holding for the checkpoint about to be judged - immediately before the
    verdict, through the same rollout/parse path training uses
    (grpo_loop.rollout -> grpo_loss.parse_decision) and the same engine pricing
    (score_entry). Every emitted record carries
    provenance.ckpt = the candidate checkpoint directory and provenance.step, and
    after emission the loop READS BACK the file and REFUSES if the provenance does
    not name the candidate it just scored. Therefore, with the switch ON, a verdict
    cannot belong to any policy but the candidate being judged, and a pre-existing
    file is OVERWRITTEN rather than trusted.

    THE DEFAULT IS THE SAFE, UNCHANGED BEHAVIOUR: OFF. The gate reads whatever is at
    MEM_RECORDS - including nothing, which is a REFUSAL - exactly as before, so a
    run that does not ask for the correspondence guarantee cannot get a silent pass
    from this change. Turning it ON can only ever ADD a refusal (emission failure or
    a mismatched provenance is fail-closed), never remove one.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import sys
import time
import types

HERE = os.path.dirname(os.path.abspath(__file__))

# Memorization gate inputs (see memorization_detector.py). Paths are fixed on purpose:
# a "pass" that depends on an env var an operator can forget to set is not a gate.
MEM_PREREG = os.environ.get("RL_MEM_PREREG",
                            "/training/v2/reports/MEMORIZATION_BOUNDS.json")
MEM_RECORDS = os.environ.get("RL_MEM_RECORDS",
                             "/training/v2/reports/MEMORIZATION_RECORDS.jsonl")
MEM_SCORE = os.environ.get("RL_MEM_SCORE",
                           "/training/v2/reports/MEMORIZATION_SCORE.json")
sys.path.insert(0, HERE)

# The SFT trainer's proven FSDP2 guards live here. They are IMPORTED (read-only),
# never copied and never modified, so the RL run enforces byte-identical startup
# assertions to the SFT run that proved them on this exact stack.
REF_DIR = "/training/code/qwen27b"
FSDP2_PLAN_PATH = os.path.join(REF_DIR, "fsdp_full_shard_native.json")
WALL_JSON = "/training/v2/reports/FORWARD_WALL_V1.json"

import torch  # noqa: E402
from torch.utils.data import Dataset, DataLoader, DistributedSampler  # noqa: E402

from grpo_loss import (  # noqa: E402
    ARM_ORDER, DEFAULT_LOSS_CONFIG, GRPOLossConfig, action_kl, assemble_advantages,
    assemble_arm_probs, grpo_loss, joint_arm_logits, parse_decision,
)
from prompt_format import (  # noqa: E402
    ACTIONS, SIZE_LABELS, action_token_ids, action_variants, decision_marker_ids,
    locate_decision_slot, render_prompt, size_token_ids,
)

IGNORE_INDEX = -100


def record_group_weight(rec: dict) -> float:
    """The per-group LOSS weight carried on a precomputed decision record.

    Delegates to grpo_trainer.read_group_loss_weight - the ONE reader of the weight
    (see its docstring) - so the trainer-side and loop-side refusal rules cannot
    drift apart. ABSENT means the identity 1.0 (every pre-v2 record was built with
    lambda_cvar pinned OFF, so its true weight IS 1.0); a present-but-malformed
    weight REFUSES rather than being coerced, because a fabricated identity would
    silently drop a real objective factor and look exactly like a healthy run.
    """
    from grpo_trainer import read_group_loss_weight
    return read_group_loss_weight(rec)


class RLDataset(Dataset):
    """One item = one decision: the SFT-faithful prompt plus its scored group."""

    def __init__(self, path: str, tokenizer, max_prompt_tokens: int = 4096):
        self.rows = []
        with open(path, encoding="utf-8") as f:
            for line in f:
                try:
                    r = json.loads(line)
                except Exception:                               # noqa: BLE001
                    continue
                if not r.get("group_usable"):
                    continue                                    # masked, not zeroed
                self.rows.append(r)
        self.tok = tokenizer
        self.max_prompt_tokens = max_prompt_tokens

    def __len__(self):
        return len(self.rows)

    def __getitem__(self, i):
        r = self.rows[i]
        msgs = json.loads(r["prompt"])
        text = render_prompt(self.tok, msgs)
        enc = self.tok(text, return_tensors="pt", truncation=True,
                       max_length=self.max_prompt_tokens)
        return {
            "input_ids": enc["input_ids"][0],
            "attention_mask": enc["attention_mask"][0],
            "prompt_sha256": r["prompt_sha256"],
            "episode_id": r["episode_id"],
            "group": r["group"],
            "advantages": r["advantages"],
            "mint": r.get("mint"),
            "t_dec_ms": r.get("t_dec_ms"),
            # The group's LOSS WEIGHT (the CVaR tail term's per-group multiplier,
            # grpo_reward_bridge._tail_adjust). It travels exactly like `advantages`
            # does - both are precomputed per record and only LOOKED UP in the loop.
            # A record with no such field (an older dataset) yields None here and is
            # read as the identity 1.0 in train_step; the field is never invented.
            "group_loss_weight": r.get("group_loss_weight"),
            # the RENDERED prompt text: train_step needs it to rebuild the
            # (prompt + sampled completion) sequence for the scoring forward, and
            # re-rendering it there would be a second, drift-prone definition.
            "prompt_text": text,
        }


def collate(batch, pad_id: int):
    """Left-pad prompts so the DECISION position is at a fixed index from the end."""
    ids = [b["input_ids"] for b in batch]
    am = [b["attention_mask"] for b in batch]
    L = max(int(x.shape[0]) for x in ids)
    out_ids, out_am = [], []
    for x, m in zip(ids, am):
        n = L - int(x.shape[0])
        out_ids.append(torch.cat([torch.full((n,), pad_id, dtype=x.dtype), x]))
        out_am.append(torch.cat([torch.zeros(n, dtype=m.dtype), m]))
    return {
        "input_ids": torch.stack(out_ids),
        "attention_mask": torch.stack(out_am),
        "meta": [{k: b[k] for k in ("prompt_sha256", "episode_id", "group",
                                    "advantages", "group_loss_weight", "mint",
                                    "t_dec_ms", "prompt_text")}
                 for b in batch],
    }


# action_token_ids / action_variants / locate_decision_slot / render_prompt are
# imported from prompt_format: ONE definition shared with the reference-cache
# builder, so the prompt, the action basis and the slot can never drift apart.


class RefActionDist:
    """prompt_sha256 -> reference action distribution over (BUY, WATCH, SKIP).

    A MISSING ENTRY IS A REFUSAL for that prompt (the decision is skipped), never
    treated as a uniform reference - a silent uniform anchor would quietly turn
    the KL term into noise.
    """

    def __init__(self, path: str, actions=("BUY", "WATCH", "SKIP")):
        self.path = path
        self.actions = list(actions)
        self.map = {}
        self.arms_map = {}
        self.manifest = {}
        if not os.path.isfile(path):
            raise SystemExit(f"REFUSING: reference cache not found: {path}")
        for line in open(path, encoding="utf-8"):
            try:
                r = json.loads(line)
            except Exception:                                   # noqa: BLE001
                continue
            p = r.get("ref_action_probs")
            if p and len(p) == len(self.actions):
                self.map[r["prompt_sha256"]] = p
            q = r.get("ref_arm_probs")
            if q and len(q) == len(ARM_ORDER):
                self.arms_map[r["prompt_sha256"]] = q
        mf = path + ".manifest.json"
        if os.path.isfile(mf):
            self.manifest = json.load(open(mf, encoding="utf-8"))
            if self.manifest.get("actions") and \
                    list(self.manifest["actions"]) != self.actions:
                raise SystemExit(
                    f"REFUSING: ref cache action order "
                    f"{self.manifest['actions']} != {self.actions}")
        if not self.map:
            raise SystemExit(f"REFUSING: reference cache is empty: {path}")

    def get(self, sha: str):
        return self.map.get(sha)

    def get_arms(self, sha: str):
        """The 5-arm JOINT reference for a prompt, or None.

        None is a REFUSAL for that sample, not a licence to substitute a uniform
        anchor: a uniform joint would quietly turn the size part of the KL into noise,
        which is exactly the failure mode the marginal map already guards against.
        """
        return self.arms_map.get(sha)

    def has_joint(self) -> bool:
        return bool(self.arms_map)

    def coverage(self, shas) -> dict:
        have = sum(1 for s in shas if s in self.map)
        return {"n": len(shas), "covered": have,
                "frac": (have / len(shas)) if shas else 0.0}


@torch.no_grad()
def rollout(model, tokenizer, batch, cfg, device, completions=None):
    """Sample G completions per prompt; return decoded texts + action ids.

    `completions` is the TEST-ONLY seam (see load_test_completions): when given it
    is a {prompt_sha256: [text, ...]} mapping that replaces generation, because a
    tiny randomly-initialised model cannot emit a parseable DECISION. It is inert
    (None) on every real run.
    """
    ids = batch["input_ids"].to(device)
    am = batch["attention_mask"].to(device)
    G = int(cfg["loop"]["group_size"])
    if completions is not None:
        out = []
        for b_i, m in enumerate(batch["meta"]):
            chunk = list(completions.get(m["prompt_sha256"]) or [])
            out.append({"prompt_index": b_i, "completions": chunk,
                        "actions": [parse_decision(t) for t in chunk]})
        return out
    out = []
    was = model.training
    model.eval()
    _mnt = int(cfg["loop"]["max_new_tokens"])
    gen = model.generate(
        input_ids=ids, attention_mask=am,
        do_sample=True, temperature=float(cfg["loop"]["temperature"]),
        top_p=float(cfg["loop"]["top_p"]),
        max_new_tokens=_mnt,
        # FSDP2 collectives happen inside EVERY forward of the decode loop, so the
        # NUMBER of decode steps must be identical on all ranks or the run hangs in an
        # all-gather. Without min_new_tokens, each rank stops its own rows at their own
        # EOS (prompts differ per rank) and the ranks immediately diverge. Forcing the
        # pinned length costs ~1 generation step of wasted work on rows that would have
        # stopped early and is the only way multi-rank FSDP2 rollout is collective-safe.
        min_new_tokens=_mnt,
        eos_token_id=None,
        num_return_sequences=G,
        pad_token_id=tokenizer.pad_token_id or tokenizer.eos_token_id,
    )
    new = gen[:, ids.shape[1]:]
    texts = tokenizer.batch_decode(new, skip_special_tokens=True)
    for b_i in range(ids.shape[0]):
        chunk = texts[b_i * G:(b_i + 1) * G]
        out.append({"prompt_index": b_i, "completions": chunk,
                    "actions": [parse_decision(t) for t in chunk]})
    if was:
        model.train()
    return out


def _row_prompt_len(prompt_len, b: int) -> int:
    """prompt_len may be a single int (uniform batch) or a per-row sequence.

    It MUST be per-row: collate() left-pads the ROLLOUT prompts, but the scoring
    forward tokenizes (prompt+completion) with RIGHT padding, so every row's prompt
    boundary sits at its own unpadded length. Passing row 0's length for the whole
    batch mislocates the decision slot (and the prompt boundary of the
    reconstruction teacher-forcing) on every row whose prompt differs - which is
    every real batch, since prompts come from different mints/episodes.
    """
    try:
        return int(prompt_len[b])
    except TypeError:
        return int(prompt_len)


def masked_token_logprobs(logits: torch.Tensor, seq_ids: torch.Tensor,
                          prompt_len, tok, full_texts, want_size: bool = True):
    """Per-token logprob of the generated span plus the DECISION/SIZE span mask.

    logits: [B, T, V] from a forward pass over (prompt + completion).
    prompt_len: int (uniform) or one length PER ROW (the correct form).
    The mask is computed from the decoded text via `decision_span_mask`, then
    mapped onto tokens with the tokenizer's offset mapping - this reuse means the
    RL mask and the SFT mask are the SAME definition, not two drifting copies.
    """
    from grpo_trainer import decision_span_mask
    B, T, V = logits.shape
    lp = torch.log_softmax(logits.float(), dim=-1)
    # on logits.device: the indexing below uses the (device) sequence ids and the
    # result feeds the loss, which lives on the compute device.
    out_lp = torch.zeros(B, T, device=logits.device)
    out_m = torch.zeros(B, T, device=logits.device)
    for b in range(B):
        plen = _row_prompt_len(prompt_len, b)
        ids = seq_ids[b]
        # logprob of each generated token under the policy, teacher-forced
        rows = torch.arange(max(int(plen) - 1, 0), T - 1)
        if rows.numel() == 0:
            continue
        tgt = ids[rows + 1]
        out_lp[b, rows + 1] = lp[b, rows, tgt]
        enc = tok(full_texts[b], return_offsets_mapping=True,
                  add_special_tokens=False)
        offs = enc["offset_mapping"]
        m = decision_span_mask(offs, full_texts[b], want_size=want_size)
        n = min(len(m), T)
        out_m[b, :n] = torch.tensor(m[:n], dtype=torch.float32,
                                    device=logits.device)
        out_m[b, :int(plen)] = 0.0                # never optimize the prompt
    return out_lp, out_m


def policy_action_logits(policy_out, seq_ids, prompt_len, action_ids,
                         marker_ids=None):
    """[B, A] policy logits over the action vocabulary at the decision slot.

    `prompt_len` is int or PER-ROW, for the same reason as
    masked_token_logprobs: the decision slot search must start at each row's own
    prompt boundary. `marker_ids` (decision_marker_ids) makes the search prefer the
    action token that follows the DECISION marker instead of the first action token
    after the prompt - the prompt itself can mention BUY/WATCH/SKIP.
    """
    logits = policy_out.logits
    B = logits.shape[0]
    found = [locate_decision_slot(seq_ids[b], _row_prompt_len(prompt_len, b),
                                  action_ids, marker_ids)
             for b in range(B)]
    rows = []
    for b, (i, _how) in enumerate(found):
        if i is None or i == 0:
            rows.append(None)
            continue
        rows.append(logits[b, i - 1, list(action_ids)].float())
    out = torch.zeros(B, len(action_ids), device=logits.device)
    ok = torch.zeros(B, dtype=torch.bool)
    for b, r in enumerate(rows):
        if r is not None:
            out[b] = r
            ok[b] = True
    return out, ok


def size_slot_logits(model, tok, prompt_texts, size_ids, device):
    """[B, 3] policy logits at the SIZE slot under the teacher-forced BUY prefix.

    WHY TEACHER-FORCED, when the decision slot is read off the sampled sequence: a
    SKIP or WATCH completion never writes a SIZE field, so for those samples the size
    slot does not exist in the sequence at all. Reading it under the fixed prefix
    "...DECISION: BUY\\nSIZE: " yields P(TIER | BUY) for EVERY prompt, and it is the
    SAME construction grpo_ref_cache uses to build the reference joint - so the KL
    compares like with like instead of measuring a policy object against a
    differently-defined reference.

    One extra forward pass over prefix-only rows (no completion tokens), so it is
    cheaper than the main pass. Returns (logits, ok); ok is False for a row that
    tokenised to nothing, and such a row is MASKED by the caller - never given a
    uniform anchor.
    """
    texts = [pt + "DECISION: BUY\nSIZE: " for pt in prompt_texts]
    enc = tok(texts, return_tensors="pt", padding=True, truncation=True,
              max_length=8192)
    ids = enc["input_ids"].to(device)
    am = enc["attention_mask"].to(device)
    out = model(input_ids=ids, attention_mask=am)
    B = ids.shape[0]
    rows = torch.zeros(B, len(size_ids), device=device)
    ok = torch.zeros(B, dtype=torch.bool)
    for b in range(B):
        n = int(am[b].sum().item())
        if n == 0:
            continue
        # padding_side is pinned to 'right' by the driver, so n-1 is this row's last
        # real token and its logits are the ones that predict the size token.
        rows[b] = out.logits[b, n - 1, list(size_ids)].float()
        ok[b] = True
    return rows, ok


def build_batch_tensors(tok, prompt_texts, comp_texts, device):
    """Tokenize (prompt+completion) for the policy forward pass.

    Returns ids, attention mask and the prompt lengths so the decision slot and the
    prompt boundary are both exact.

    PADDING SIDE MATTERS HERE: `tok.padding_side` is pinned to 'right' by the
    driver, so a short row's prompt keeps its own token offsets and the per-row
    prompt length below is the true boundary. Under left padding every row would be
    shifted by its own pad length and the offsets in `masked_token_logprobs` (which
    re-tokenizes each row UNPADDED) would no longer align.
    """
    full = [p + c for p, c in zip(prompt_texts, comp_texts)]
    enc = tok(full, return_tensors="pt", padding=True, truncation=True,
              max_length=8192)
    ids = enc["input_ids"]
    plens = [len(tok(p, add_special_tokens=False)["input_ids"]) for p in prompt_texts]
    return ids, enc["attention_mask"], plens, full


def train_step(model, tok, batch, cfg, ref: RefActionDist, device,
               action_ids, actions, cfg_loss: GRPOLossConfig, optimizer=None,
               backward=False, completions=None, grad_scale: float = 1.0,
               size_ids=None):
    """One gradient step: rollout -> advantage -> masked loss -> backward.

    Returns (loss_or_None, stats). A batch with no active sample returns
    (None, {...skipped}) and the caller must NOT step the optimizer.

    `backward=True` only accumulates: the optimizer step happens in the driver's
    loop, after `grad_accum` micro-batches, so that gradients stay SHARDED for the
    whole accumulation window (no no_sync / set_requires_gradient_sync(False)).
    `grad_scale` divides the loss so the accumulated gradient is the mean over the
    accumulation window instead of its sum.
    """
    prompts = batch["meta"]
    G = int(cfg["loop"]["group_size"])
    roll = rollout(model, tok, batch, cfg, device, completions)
    flat_rec, flat_act, flat_prompt = [], [], []
    for p_i, r in enumerate(roll):
        for c_i, txt in enumerate(r["completions"]):
            flat_rec.append(prompts[p_i])
            flat_act.append(r["actions"][c_i])
            flat_prompt.append(txt)
    asm = assemble_advantages(flat_rec, flat_act,
                              min_usable=int(cfg["loop"]["min_usable_candidates"]))
    # everything that meets the loss lives on the compute device (the sharded
    # policy's device), so no CPU/CUDA mix reaches grpo_loss.
    adv = torch.tensor([0.0 if a["adv"] is None else a["adv"] for a in asm],
                       dtype=torch.float32, device=device)
    active = torch.tensor([1.0 if a["active"] else 0.0 for a in asm],
                          dtype=torch.float32, device=device)
    # ---- per-group LOSS weight (the CVaR tail term's group multiplier) --------
    # It is threaded to grpo_loss exactly the way the advantage is: both are
    # PRECOMPUTED per record and merely looked up here. The weight is per GROUP,
    # not per completion, so every completion drawn from the same prompt carries
    # the same value - which is what makes the weighted mean in grpo_loss reduce
    # to "down/up-weight this whole group's gradient" rather than reshuffling
    # completions inside a group.
    weights = [record_group_weight(rec) for rec in flat_rec]
    n_nonunit_w = sum(1 for w in weights if w != 1.0)
    # The pinned default (lambda_cvar=0.0) produces an all-1.0 vector. Passing it
    # explicitly would be harmless but pointless; passing None keeps grpo_loss on
    # its arithmetic-free identity branch, so the no-CVaR run is bit-identical to
    # the pre-weight implementation rather than a multiply-by-one look-alike.
    w_arg = None if n_nonunit_w == 0 else torch.tensor(weights, dtype=torch.float32,
                                                       device=device)
    stats = {"n_samples": int(active.numel()),
             "n_active": int((active > 0).sum().item()),
             "n_unparseable": sum(1 for a in asm if a["reason"] ==
                                  "unparseable_completion"),
             "n_nonunit_group_weights": n_nonunit_w,
             "loss": None, "skipped": None}

    # ---- reference anchor: cached ACTION distribution, per prompt ------------
    ref_rows, ref_ok = [], []
    for rec in flat_rec:
        p = ref.get(rec["prompt_sha256"])
        if p is None:
            ref_ok.append(0.0)
            ref_rows.append([1.0 / len(actions)] * len(actions))
        else:
            ref_ok.append(1.0)
            ref_rows.append(p)
    stats["n_ref_missing"] = int(sum(1 for x in ref_ok if x == 0.0))
    ref_probs = torch.tensor(ref_rows, dtype=torch.float32, device=device)

    # Control-flow agreement (see _dist_all): ranks hold DIFFERENT prompts, so one
    # rank's batch can be entirely unusable while another's is fine. Returning here
    # on one rank only would desynchronise every subsequent collective.
    if not _dist_all(float(active.sum()) >= 1, device, op="or"):
        stats["skipped"] = "no_active_samples"
        return None, stats
    if not _dist_all(stats["n_ref_missing"] < len(flat_rec), device, op="or"):
        stats["skipped"] = "no_reference_rows"
        return None, stats

    # ---- policy forward over (prompt + sampled completion) -------------------
    # `prompt_text` is the RENDERED prompt carried through collate(). Re-rendering
    # it here from r["prompt"] (the earlier code) is a KeyError: collate()'s meta
    # dicts never carried a "prompt" key.
    prompt_texts = [r["prompt_text"] for r in flat_rec]
    ids, am, plens, full_txt = build_batch_tensors(tok, prompt_texts,
                                                   flat_prompt, device)
    ids = ids.to(device)
    am = am.to(device)
    out = model(input_ids=ids, attention_mask=am)
    # plens (PER ROW), not plens[0]: the batch is right-padded and prompts differ in
    # length, so a single row's length would mislocate the decision slot and the
    # prompt boundary on every other row.
    logp, tmask = masked_token_logprobs(out.logits, ids, plens, tok, full_txt,
                                        want_size=bool(cfg["loop"]["mask_rationale"]))
    a_logits, slot_ok = policy_action_logits(out, ids, plens, action_ids,
                                             decision_marker_ids(tok))
    stats["n_no_decision_slot"] = int((~slot_ok).sum().item())

    # a completion with neither a decision slot nor a parsed action is dead
    active = active * torch.tensor([1.0 if bool(s) else 0.0
                                    for s in slot_ok.tolist()], dtype=torch.float32)
    if not _dist_all(float(active.sum()) >= 1, device, op="or"):
        stats["skipped"] = "no_decision_slot"
        return None, stats

    loss, lst = grpo_loss(logp, logp.detach(), adv, tmask, active=active,
                          cfg=cfg_loss, weight=w_arg)
    lst["n_nonunit_group_weights"] = n_nonunit_w
    # ---- reference anchor ----------------------------------------------------
    # The MARGINAL KL regularises the action slot alone, so the size token would
    # drift unsupervised. When the cache carries the 5-arm joint, anchor THAT: it
    # contains the marginal and additionally holds the size conditional to the SFT
    # prior. The marginal is still reported, so a size-only shift is visible in the
    # stats instead of hiding inside one number.
    kl = action_kl(a_logits, ref_probs)
    stats["kl_action"] = float((kl * active).sum() / active.sum().clamp(min=1.0))
    if ref.has_joint():
        if size_ids is None:
            _slabels, size_ids = size_token_ids(tok)
        joint_rows, joint_ok = [], []
        for rec in flat_rec:
            q = ref.get_arms(rec["prompt_sha256"])
            joint_ok.append(bool(q))
            joint_rows.append(q if q else [0.0] * len(ARM_ORDER))
        stats["n_joint_ref_missing"] = int(sum(1 for x in joint_ok if not x))
        # a sample with no joint reference is a REFUSAL for that sample (a substituted
        # uniform joint would make the size KL pure noise)
        if not _dist_all(any(joint_ok), device, op="or"):
            stats["skipped"] = "no_joint_reference_rows"
            return None, stats
        active = active * torch.tensor([1.0 if x else 0.0 for x in joint_ok],
                                       dtype=torch.float32, device=device)
        s_logits, s_ok = size_slot_logits(model, tok, prompt_texts, size_ids, device)
        stats["n_no_size_slot"] = int((~s_ok).sum().item())
        active = active * torch.tensor([1.0 if bool(x) else 0.0
                                        for x in s_ok.tolist()],
                                       dtype=torch.float32, device=device)
        if not _dist_all(float(active.sum()) >= 1, device, op="or"):
            stats["skipped"] = "no_size_slot"
            return None, stats
        ref_arm = torch.tensor(joint_rows, dtype=torch.float32, device=device)
        kl = action_kl(joint_arm_logits(a_logits, s_logits), ref_arm)
        stats["kl_arms"] = float((kl * active).sum() / active.sum().clamp(min=1.0))
    loss = loss + float(cfg_loss.kl_coef) * (kl * active).sum() / \
        active.sum().clamp(min=1.0)
    stats.update(lst)
    stats["loss"] = float(loss.detach())
    if optimizer is not None:
        # legacy single-batch path (no accumulation): unchanged semantics
        loss.backward()
        if float(torch.nn.utils.clip_grad_norm_(
                [p for p in model.parameters() if p.requires_grad],
                float(cfg["optim"]["max_grad_norm"]))) >= 0:
            optimizer.step()
        optimizer.zero_grad(set_to_none=True)
    elif backward:
        # accumulate only; the driver clips and steps after grad_accum batches
        (loss * float(grad_scale)).backward()
    return loss, stats


def _selfcheck() -> int:
    """CPU-only wiring proof: the parts of the step that need no model."""
    import tempfile
    fails, checks = [], 0

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            fails.append(tag)

    # decision slot is found at the first action token after the prompt
    chk("slot_found", locate_decision_slot([9, 9, 9, 42, 7, 8], 3,
                                           [42, 43, 44])[0] == 3)
    chk("slot_absent_is_none", locate_decision_slot([9, 9, 9, 7, 8], 3,
                                                    [42, 43, 44])[0] is None)

    # action logits are read at slot-1 restricted to the action ids
    lg = torch.zeros(1, 6, 100)
    lg[0, 2, 42] = 5.0
    out, ok = policy_action_logits(type("O", (), {"logits": lg})(),
                                   torch.tensor([[9, 9, 9, 42, 7, 8]]), 3,
                                   [42, 43, 44])
    chk("action_logits_ok", bool(ok[0]) and float(out[0, 0]) == 5.0)
    chk("action_logits_no_slot", not bool(policy_action_logits(
        type("O", (), {"logits": lg})(), torch.tensor([[9, 9, 9, 7, 8, 8]]), 3,
        [42, 43, 44])[1][0]))

    # ---- per-row prompt lengths (the plens[0] bug) --------------------------
    # A batch is RIGHT-padded and its prompts DIFFER in length, so row 0's length is
    # not the batch's prompt boundary. Row 1 here has an action token (42) inside its
    # PROMPT, which is exactly what a decision prompt does when it names BUY/WATCH/
    # SKIP - so searching from the wrong boundary finds an action token in the prompt
    # and reads the action distribution off the wrong position.
    chk("row_len_int", _row_prompt_len(5, 0) == 5)
    chk("row_len_per_row", _row_prompt_len([2, 6], 1) == 6)
    seqs = torch.zeros(2, 11, dtype=torch.long)
    seqs[0, :7] = torch.tensor([9, 9, 42, 9, 7, 8, 8])            # prompt len 2
    seqs[1] = torch.tensor([9, 9, 42, 9, 9, 9, 9, 9, 42, 7, 8])   # prompt len 6
    lg2 = torch.zeros(2, 11, 100)
    lg2[0, 1, 42] = 1.0                        # correct slot for row 0
    lg2[1, 7, 42] = 3.0                        # correct slot for row 1
    lg2[1, 1, 42] = 9.0                        # WRONG slot (inside row 1's prompt)
    _o, ok_rows = policy_action_logits(type("O", (), {"logits": lg2})(), seqs,
                                       [2, 6], [42, 43, 44])
    _o0, _ok_bug = policy_action_logits(type("O", (), {"logits": lg2})(), seqs,
                                        [2, 2], [42, 43, 44])
    chk("per_row_slot_ok", bool(ok_rows[0]) and bool(ok_rows[1]))
    chk("per_row_reads_completion_slot", float(_o[1, 0]) == 3.0)
    chk("plens0_reads_prompt_slot_BUG",
        float(_o0[1, 0]) == 9.0 and float(_o0[1, 0]) != float(_o[1, 0]))

    # masked_token_logprobs: the per-row boundary is what keeps the mask off the
    # PROMPT when the prompt itself contains a DECISION field.
    class _CharTok:
        """1 token per character, offsets = char spans (offsets are what matter)."""
        padding_side = "right"

        def __call__(self, text, return_offsets_mapping=False,
                     add_special_tokens=False, **kw):
            return {"input_ids": [ord(c) for c in text],
                    "offset_mapping": [(i, i + 1) for i in range(len(text))]}

    stok = _CharTok()
    full = ["ABDECISION: X", "ABCDEFDECISION: X"]
    seq_ids = torch.zeros(2, len(full[1]), dtype=torch.long)
    for _b, _t in enumerate(full):
        seq_ids[_b, :len(_t)] = torch.tensor([ord(c) for c in _t])
    lg3 = torch.zeros(2, seq_ids.shape[1], 128)     # V covers ord() of ASCII
    _lp, mask = masked_token_logprobs(lg3, seq_ids, [2, 6], stok, full)
    chk("mask_per_row_prompt_is_zero", float(mask[1, :6].sum()) == 0.0)
    chk("mask_per_row_decision_only",
        float(mask[1, 16]) == 1.0 and float(mask[1, :16].sum()) == 0.0)

    # reference cache: present, ordered, and a MISSING row is reported
    tmp = os.path.join(tempfile.mkdtemp(), "ref.jsonl")
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"prompt_sha256": "a",
                             "ref_action_probs": [0.6, 0.3, 0.1]}) + "\n")
    json.dump({"schema": "grpo_ref_action_v1", "actions": ["BUY", "WATCH", "SKIP"]},
              open(tmp + ".manifest.json", "w"))
    r = RefActionDist(tmp)
    chk("ref_hit", r.get("a") == [0.6, 0.3, 0.1])
    chk("ref_miss_is_none", r.get("zzz") is None)
    chk("ref_coverage_reported", r.coverage(["a", "zzz"])["covered"] == 1)

    # a wrong action order in the manifest must REFUSE
    bad = os.path.join(tempfile.mkdtemp(), "bad.jsonl")
    with open(bad, "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"prompt_sha256": "a",
                             "ref_action_probs": [0.6, 0.3, 0.1]}) + "\n")
    json.dump({"actions": ["SKIP", "WATCH", "BUY"]}, open(bad + ".manifest.json", "w"))
    refused = False
    try:
        RefActionDist(bad)
    except SystemExit:
        refused = True
    chk("ref_wrong_order_refuses", refused)

    # ---- (1) the per-group loss weight: read, default, refuse -----------------
    # absent field -> identity 1.0; present -> the value; present-but-garbage ->
    # REFUSE (never silently defaulted).
    chk("group_weight_absent_is_identity",
        record_group_weight({"episode_id": "x"}) == 1.0)
    chk("group_weight_present_is_used",
        record_group_weight({"group_loss_weight": 0.25}) == 0.25)
    _gw_bad = False
    try:
        record_group_weight({"episode_id": "x", "group_loss_weight": "oops"})
    except ValueError:
        _gw_bad = True
    chk("group_weight_garbage_refuses", _gw_bad)

    # ---- (2) curriculum: OFF by default, frozen metric, refuse-not-impute ----
    chk("curriculum_absent_is_off", curriculum_config({})["enabled"] is False)
    chk("curriculum_bool_on", curriculum_config({"curriculum": True})["enabled"])
    chk("curriculum_dict_stages",
        curriculum_config({"curriculum": {"enabled": True, "stages": 3}})
        == {"enabled": True, "stages": 3})

    def _row(eid, buy, skip=0.0, watch=0.0):
        return {"episode_id": eid, "prompt_sha256": eid,
                "advantages": {"SKIP": 0.0, "WATCH": 0.0, "BUY_FULL": 1.0},
                "group": {"SKIP": skip, "WATCH": watch, "BUY_FULL": buy}}

    chk("row_difficulty_is_spread", abs(row_difficulty(_row("a", 1.0)) - 1.0) < 1e-12)
    # easy = LARGE spread: the row with the biggest spread must come FIRST
    rows = [_row("mid", 0.5), _row("easy", 2.0), _row("hard", 0.05)]
    plan = curriculum_plan(rows, {"stages": 3})
    ord0, rep0 = curriculum_order(plan, epoch=0)
    chk("curriculum_ascending_easy_first", [r["episode_id"] for r in ord0]
        == ["hard", "mid", "easy"])
    # MEMBERSHIP IS PRESERVED: every epoch is a PERMUTATION of the same set
    _sets = [sorted(r["episode_id"] for r in curriculum_order(plan, e)[0])
             for e in (0, 1, 2, 5)]
    chk("curriculum_membership_preserved", all(s == sorted(["hard", "mid", "easy"])
                                               for s in _sets))
    chk("curriculum_epoch_rotates_start", rep0["start_band"] == 0
        and curriculum_order(plan, epoch=1)[1]["start_band"] == 1)
    # deterministic given the dataset: re-planning cannot move the schedule
    chk("curriculum_deterministic",
        curriculum_plan([_row("mid", 0.5), _row("easy", 2.0), _row("hard", 0.05)],
                        {"stages": 3})["order_idx"] == plan["order_idx"])
    # a row whose spread is NOT computable must REFUSE (report, never impute)
    _bad = [_row("ok", 0.5), {"episode_id": "one_arm", "advantages": {"BUY_FULL": 1.0},
                              "group": {"BUY_FULL": 1.0}}]
    _refused = False
    try:
        curriculum_plan(_bad, {"stages": 1})
    except SystemExit as exc:
        _refused = "cannot be computed" in str(exc) and "one_arm" in str(exc)
    chk("curriculum_refuses_uncomputable_row", _refused)

    # ---- (3) observability: histogram, entropy(+ratio), knowing-doing ---------
    _arms = ["SKIP", "WATCH", "BUY_FULL"]
    h = arm_histogram_entropy(_arms, ["SKIP", "WATCH", "BUY_FULL"])
    chk("hist_entropy_uniform_ratio_1", abs(h["entropy_ratio"] - 1.0) < 1e-9
        and not h["collapsed"])
    h2 = arm_histogram_entropy(["SKIP", "SKIP"], ["SKIP", "WATCH", "BUY_FULL"])
    chk("hist_entropy_collapsed_ratio_0", h2["entropy_nats"] == 0.0
        and h2["entropy_ratio"] == 0.0 and h2["collapsed"])
    h3 = arm_histogram_entropy(["SKIP", None], ["SKIP", "WATCH", "BUY_FULL"])
    chk("hist_unparseable_reported", h3["n_unparseable"] == 1
        and h3["n_parsed"] == 1)
    # knowing-doing: a rationale arguing WATCH while DECISION says BUY is a MISMATCH
    _c_ok = "The breakout is clean, I will buy here.\nDECISION: BUY\nSIZE: FULL"
    _c_bad = "I will watch for a reclaim and wait.\nDECISION: SKIP"
    _c_amb = "I could buy, or maybe wait and watch.\nDECISION: BUY\nSIZE: FULL"
    _c_none = "DECISION: WATCH"
    chk("kd_buy_agrees",
        rationale_implied_family(_c_ok) == "BUY"
        and _ARM_FAMILY[parse_decision(_c_ok)] == "BUY")
    chk("kd_watch_vs_skip_mismatch",
        rationale_implied_family(_c_bad) == "WATCH"
        and _ARM_FAMILY[parse_decision(_c_bad)] == "SKIP")
    chk("kd_ambiguous_is_unparseable",
        rationale_implied_family(_c_amb) is None)
    chk("kd_no_rationale_is_unparseable",
        rationale_implied_family(_c_none) is None)

    print(json.dumps({"suite": "grpo_loop", "checks": checks, "failed": fails,
                      "verdict": "PASS" if not fails else "FAIL"}, indent=1))
    return 1 if fails else 0


def _plan(cfg_path: str, train_path: str, ref_path: str) -> int:
    cfg = json.load(open(cfg_path, encoding="utf-8"))
    n = sum(1 for _ in open(train_path, encoding="utf-8")) \
        if os.path.isfile(train_path) else 0
    ref = None
    if os.path.isfile(ref_path):
        ref = sum(1 for _ in open(ref_path, encoding="utf-8"))
    print(json.dumps({"run_id": cfg.get("run_id"), "init_from": cfg.get("init_from"),
                      "train_records": n, "ref_rows": ref,
                      "group_size": cfg["loop"]["group_size"],
                      "kl_coef": cfg["loop"]["kl_coef"],
                      "lr": cfg["optim"]["lr"],
                      "horizon_ms": (cfg.get("data") or {}).get("horizon_ms")},
                     indent=1))
    return 0


# ===========================================================================
# --train : the accelerate + FSDP2 GRPO driver
#
# CONTRACT (mirrored from /training/code/qwen27b/train_qwen27b.py, the proven
# reference; the guards are IMPORTED from it, never re-implemented):
#   1. bf16 is declared AT THE FSDP LAYER (MixedPrecisionPolicy(param_dtype=bf16))
#      with Accelerator.mixed_precision='no'. accelerate upcasts every FSDP flat
#      parameter to fp32 when mixed_precision != 'no' (+53.7GB/rank measured).
#   2. adamw_bnb_8bit is bound to the LOCAL SHARD after the model is sharded, via
#      fsdp2_local_shard_bnb (the DTensor-dispatch path is provably dead).
#   3. Gradients stay SHARDED through accumulation: no no_sync() /
#      set_requires_gradient_sync(False); the accumulation plugin declares
#      sync_each_batch=True (a guard refuses any other value).
#   4. Model checkpoint = DCP shards; OPTIMIZER state = PER-RANK files
#      (optimizer_0_rank<r>.bin, accelerate's use_dcp=False path, because the DCP
#      optimizer path cannot carry bnb's nested quantisation tensors). Scheduler
#      and RNG state are saved too. mode=resume loads optimizer+step and skips
#      completed prompts by prompt_sha256; it NEVER silently restarts.
#   6. train_step() keeps its loss definition; this driver only drives it.
#   7. Every refusal is LOUD and happens before the first forward pass.
#   8. Checkpoint selection uses the PRE-REGISTERED wall gate only.
# ===========================================================================


class Refusal(SystemExit):
    """A loud refusal with a non-zero exit, raised BEFORE any forward pass.

    Every refusal message carries the REFUSING prefix so a run log can be grepped
    for the one thing that matters, and the process exits non-zero.
    """

    def __init__(self, msg: str):
        super().__init__(f"REFUSING: {msg}")


def require(cond, msg: str):
    if not cond:
        raise Refusal(msg)


# ===========================================================================
# CURRICULUM (easy-to-hard ordering). OFF BY DEFAULT.
#
# `loop.curriculum` absent (or enabled=false) -> file order, i.e. byte-for-byte the
# legacy path. It is off because an ordering is a TRAINING-SIGNAL change: it moves
# which examples share a gradient-accumulation window, so it belongs behind an
# explicit arm, never on by default.
#
# THE DIFFICULTY METRIC IS FROZEN AND DECLARED - there is deliberately NO adaptive
# or model-derived metric (a difficulty you re-derive as the policy learns is a
# second, unmeasured training signal):
#
#       D(row) = max_a r_a  -  min_a r_a
#
#   a ranges over the row's SCORED arms = the keys of row["advantages"] (exactly the
#   candidates counterfactual_stats found usable); r_a = row["group"][a] is that arm's
#   PRECOMPUTED counterfactual return. D is the arm SPREAD: the discriminability
#   among the decision's candidate counterfactual rewards, i.e. how separable the
#   best arm is from the worst for THIS decision.
#
#   D is a property of the DATASET, not of the run: it is computable the instant a
#   record is read - before any sampling, forward pass, advantage lookup or loss -
#   it depends on no model output and no training statistic, and it is
#   byte-deterministic given the dataset. That is what makes the schedule auditable.
#
#   Easy = LARGE spread (clearly separable arms -> crisp advantage signal).
#   Hard = SMALL spread (near-tied arms -> a marginal decision, small signal).
#   The schedule is therefore ASCENDING in D: clear-signal decisions first.
#
# REFUSAL, NOT IMPUTATION: a row with no `group`/`advantages`, with fewer than two
# scored arms, or with a scored arm missing from `group` or non-finite has NO
# computable D. curriculum_plan() then REFUSES loudly and names the rows. A defaulted
# difficulty would be an invented ordering that nobody can audit, so it is refused
# rather than guessed.
#
# MEMBERSHIP IS PRESERVED: the curriculum only PERMUTES rows, so the SET trained in
# an epoch is identical to the legacy set - only the ORDER (and the grouping into
# gradient windows) changes. `stages` cuts the ascending order into contiguous
# difficulty bands; epoch e STARTS at band min(e, stages-1) and wraps around, so
# every epoch still trains every row - a multi-epoch run just begins each epoch
# further up the difficulty ladder. Nothing is dropped, nothing is duplicated.
# ===========================================================================


def curriculum_config(loop: dict) -> dict:
    """Read `loop.curriculum`. Absent = OFF (the legacy uniform schedule)."""
    c = (loop or {}).get("curriculum")
    if c is None:
        return {"enabled": False, "stages": 1}
    if isinstance(c, bool):
        return {"enabled": c, "stages": 1}
    require(isinstance(c, dict),
            f"loop.curriculum must be a bool or an object "
            f"{{enabled, stages}}, got {type(c).__name__}")
    stages = int(c.get("stages", 1))
    require(stages >= 1, f"loop.curriculum.stages must be >= 1, got {stages}")
    return {"enabled": bool(c.get("enabled", False)), "stages": stages}


def row_difficulty(row: dict) -> float:
    """The FROZEN difficulty metric D(row) = max_a r_a - min_a r_a over scored arms.

    Raises ValueError (with the reason) when the metric is not computable; the
    caller aggregates those into one loud refusal.
    """
    adv = row.get("advantages")
    grp = row.get("group")
    if not isinstance(adv, dict) or not isinstance(grp, dict):
        raise ValueError("no dict-valued 'advantages'/'group' (unscored row)")
    vals = []
    for a in sorted(adv):
        if a not in grp:
            raise ValueError(f"scored arm {a!r} is absent from 'group'")
        try:
            v = float(grp[a])
        except (TypeError, ValueError):
            raise ValueError(f"arm {a!r} has non-numeric group value {grp[a]!r}")
        if not math.isfinite(v):
            raise ValueError(f"arm {a!r} has non-finite group value {v!r}")
        vals.append(v)
    if len(vals) < 2:
        raise ValueError(f"only {len(vals)} scored arm(s); a spread needs >= 2")
    return max(vals) - min(vals)


def curriculum_plan(rows: list, cfg: dict) -> dict:
    """Score every row ONCE (in file order) and cut the ascending order into bands.

    Scoring is done against the file order and the permutation is a stable sort of
    (D, file index), so it is a pure function of the dataset: re-running the plan
    over an already-permuted list cannot drift the schedule on later epochs.
    """
    scored, bad = [], []
    for i, r in enumerate(rows):
        try:
            scored.append((row_difficulty(r), i))
        except ValueError as exc:
            bad.append((i, r.get("episode_id"), str(exc)))
    if bad:
        head = "; ".join(f"row {i} ({eid!r}): {why}" for i, eid, why in bad[:8])
        more = "" if len(bad) <= 8 else f" ... and {len(bad) - 8} more"
        raise Refusal(
            f"loop.curriculum is ON but the frozen difficulty metric (arm spread) "
            f"cannot be computed for {len(bad)}/{len(rows)} training row(s): {head}"
            f"{more}. Refusing rather than imputing a difficulty - a defaulted "
            f"difficulty is an invented ordering nobody can audit. Set "
            f"loop.curriculum.enabled=false to train in file order.")
    scored.sort(key=lambda t: (t[0], t[1]))            # stable, deterministic
    order = [i for _d, i in scored]
    n, s = len(order), max(1, min(int(cfg.get("stages", 1)), max(1, len(order))))
    b = [round(k * n / s) for k in range(s + 1)]
    return {"rows": rows, "order_idx": order,
            "bands": [order[b[k]:b[k + 1]] for k in range(s)],
            "stages": s, "spreads": [d for d, _i in scored], "n": n}


def curriculum_order(plan: dict, epoch: int = 0):
    """The ordered rows for `epoch` + the schedule report. Pure permutation."""
    rows, s, n = plan["rows"], plan["stages"], plan["n"]
    k0 = int(min(epoch, s - 1)) if s > 1 else 0
    seq = plan["bands"][k0:] + plan["bands"][:k0]
    idx = [i for band in seq for i in band]
    report = {
        "metric": "arm_spread = max(scored group return) - min(scored group return)",
        "stages": s, "epoch": int(epoch), "start_band": k0, "n_rows": n,
        "band_sizes": [len(x) for x in plan["bands"]],
        "spread_min": plan["spreads"][0],
        "spread_median": plan["spreads"][n // 2],
        "spread_max": plan["spreads"][-1],
        # a stable fingerprint of the ORDER actually fed to the loader, so a run log
        # proves which schedule produced the checkpoint.
        "order_sha256": hashlib.sha256(
            "".join(str(rows[i].get("prompt_sha256")) for i in idx).encode()
        ).hexdigest(),
    }
    return [rows[i] for i in idx], report


def _ref_contract():
    """Import the SFT trainer module (READ-ONLY) so the FSDP2 guards are the SAME
    code that proved the contract on this stack - not a copy that can drift."""
    if REF_DIR not in sys.path:
        sys.path.insert(0, REF_DIR)
    try:
        import train_qwen27b as ref
    except Exception as exc:                                    # noqa: BLE001
        raise Refusal(
            f"cannot import the SFT FSDP2 reference contract from {REF_DIR} "
            f"({type(exc).__name__}: {exc}). The pinned guards cannot be enforced, "
            f"so this driver refuses to train.") from exc
    return ref


class GuardShim:
    """The minimal `trainer` surface that the SFT guards read."""

    def __init__(self, accelerator, model, optimizer, optim_name, model_wrapped=None):
        self.accelerator = accelerator
        self.model = model
        self.model_wrapped = model_wrapped if model_wrapped is not None else model
        self.optimizer = optimizer
        self.args = types.SimpleNamespace(optim=optim_name)


# --------------------------------------------------------------------------
# config / plan resolution
# --------------------------------------------------------------------------
def load_config(path: str) -> dict:
    require(bool(path) and os.path.isfile(path), f"config not found: {path!r}")
    try:
        cfg = json.load(open(path, encoding="utf-8"))
    except Exception as exc:                                    # noqa: BLE001
        raise Refusal(f"config is not readable JSON: {path} ({exc})") from exc
    for k in ("data", "loop", "optim", "checkpoint", "gate"):
        require(isinstance(cfg.get(k), dict), f"config is missing the {k!r} block")
    require(int(cfg["loop"]["group_size"]) >= 1, "loop.group_size must be >= 1")
    require(int(cfg["loop"]["grad_accum"]) >= 1, "loop.grad_accum must be >= 1")
    require(float(cfg["optim"]["lr"]) > 0, "optim.lr must be > 0")
    require(cfg["optim"]["optim"] == "adamw_8bit",
            f"optim.optim={cfg['optim']['optim']!r} but this driver is pinned to "
            f"the 8-bit AdamW path (adamw_8bit) that the SFT run proved fit")
    return cfg


def resolve_plan(cfg: dict, decoder_cls: str):
    """Resolve the pinned FSDP2 plan into accelerate plugin keyword values.

    The resolution itself is delegated to the SFT trainer's fsdp_plugin_config so
    there is exactly ONE plan->plugin mapping on this box, and then every pinned
    knob is asserted (a silent plan change must not be able to reach the run).
    """
    ref = _ref_contract()
    path = (cfg.get("fsdp") or {}).get("plan") or FSDP2_PLAN_PATH
    require(os.path.isfile(path), f"pinned FSDP2 plan not found: {path}")
    pin = json.load(open(path, encoding="utf-8"))
    cfgd = ref.fsdp_plugin_config(pin, decoder_cls, int(pin.get("fsdp_version", 0)))
    require(int(cfgd["fsdp_version"]) == 2,
            f"the pinned plan declares fsdp_version={pin.get('fsdp_version')!r}; "
            f"the RL driver only runs the FSDP2 contract. REFUSING to train.")
    require(str(cfgd["fsdp_auto_wrap_policy"]).upper() == "TRANSFORMER_BASED_WRAP",
            f"the pinned plan auto-wrap policy is {cfgd['fsdp_auto_wrap_policy']!r}; "
            f"FSDP2 auto-wrap on the decoder layer is required")
    require(bool(cfgd["fsdp_reshard_after_forward"]) is True,
            "the pinned plan must reshard_after_forward=True (the FSDP2 "
            "full-shard declaration)")
    require(str(cfgd["fsdp_state_dict_type"]) == "SHARDED_STATE_DICT",
            f"state_dict_type={cfgd['fsdp_state_dict_type']!r}; SHARDED_STATE_DICT "
            f"is the pinned checkpoint format")
    require(str(cfgd["mixed_precision"]) == "bf16",
            f"the pinned plan declares mixed_precision={cfgd['mixed_precision']!r}; "
            f"bf16 must be declared AT THE FSDP LAYER")
    require(pin.get("cpu_offload") is False, "the pinned plan forbids cpu_offload")
    require(pin.get("bf16") is True, "the pinned plan requires bf16 parameters")
    require(pin.get("sync_each_batch") is True,
            "the pinned plan requires sync_each_batch=True: skipping the "
            "reduce-scatter during accumulation keeps a full unsharded gradient "
            "per rank (gradients must stay SHARDED)")
    require(pin.get("optim") == "adamw_bnb_8bit",
            f"the pinned plan optim is {pin.get('optim')!r}")
    return path, pin, cfgd


def resolve_device(pref: str = "auto") -> str:
    if pref in ("cpu", "cuda"):
        return pref
    try:
        if torch.cuda.is_available() and os.environ.get("CUDA_VISIBLE_DEVICES", "0") != "":
            return "cuda"
    except Exception:                                           # noqa: BLE001
        pass
    return "cpu"


def build_accelerator(cfg: dict, cfgd: dict, device: str):
    """Accelerator with mixed_precision='no' + the pinned FSDP2 plugin."""
    from accelerate import Accelerator
    from accelerate.utils import (FullyShardedDataParallelPlugin,
                                  GradientAccumulationPlugin)
    from torch.distributed.fsdp import MixedPrecisionPolicy

    layer_cls = cfgd["fsdp_transformer_layer_cls_to_wrap"]
    if isinstance(layer_cls, str):
        layer_cls = [layer_cls]
    # bf16 AT THE FSDP LAYER. param_dtype is what
    # train_qwen27b.assert_no_fp32_master_set proves is not fp32.
    mp = MixedPrecisionPolicy(param_dtype=torch.bfloat16,
                              reduce_dtype=torch.bfloat16,
                              output_dtype=torch.bfloat16)
    plugin = FullyShardedDataParallelPlugin(
        fsdp_version=2,
        reshard_after_forward=bool(cfgd["fsdp_reshard_after_forward"]),
        state_dict_type=str(cfgd["fsdp_state_dict_type"]),
        cpu_offload=False,
        auto_wrap_policy="transformer_based_wrap",
        transformer_cls_names_to_wrap=list(layer_cls),
        mixed_precision_policy=mp,
    )
    grad = GradientAccumulationPlugin(num_steps=int(cfg["loop"]["grad_accum"]),
                                      sync_each_batch=True,
                                      sync_with_dataloader=False)
    # mixed_precision='no' is what stops accelerate's fp32 master-weight upcast.
    acc = Accelerator(mixed_precision="no", cpu=(device == "cpu"),
                      fsdp_plugin=plugin, gradient_accumulation_plugin=grad)
    # accelerate only attaches the plugin when the backend is an accelerator
    # (MULTI_GPU/...), so on a CPU/gloo run `state.fsdp_plugin` would be None and
    # both fsdp2_prepare_model and the guards would lose the plan. Attach it
    # unconditionally: on a real accelerator run this is the same object.
    acc.state.fsdp_plugin = plugin
    require(getattr(acc, "mixed_precision", None) == "no",
            f"accelerator.mixed_precision={getattr(acc, 'mixed_precision', None)!r}; "
            f"it must be 'no' or accelerate upcasts every FSDP flat parameter to "
            f"fp32 (+53.7GB/rank measured) and the run OOMs at the first backward")
    require(bool(getattr(getattr(acc, "gradient_state", None), "plugin_kwargs", {})
                 .get("sync_each_batch", False)),
            "the gradient-accumulation plugin must declare sync_each_batch=True")
    require(getattr(plugin, "mixed_precision_policy", None) is not None
            and plugin.mixed_precision_policy.param_dtype == torch.bfloat16,
            "the FSDP2 plugin must carry MixedPrecisionPolicy(param_dtype=bf16)")
    return acc, plugin


def ensure_process_group(acc, device: str) -> str:
    """`fully_shard` needs a process group (it defaults the mesh to the default PG).

    A launcher that starts N>1 processes creates one. A single-process run - the
    only way FSDP2 can be exercised on this box while the GPUs are busy - does not,
    so create a 1-rank group on the compute backend before sharding. Never replaces
    an existing group, so the 3-rank production path is untouched.
    """
    if not torch.distributed.is_available():
        raise Refusal("torch.distributed is not available; FSDP2 cannot run")
    if torch.distributed.is_initialized():
        return "launcher_provided"
    # A torchrun/srun launch sets WORLD_SIZE/RANK but leaves the group to us.
    ws_env = int(os.environ.get("WORLD_SIZE", "1") or "1")
    rank_env = int(os.environ.get("RANK", "0") or "0")
    os.environ.setdefault("MASTER_ADDR", "127.0.0.1")
    os.environ.setdefault("MASTER_PORT", "29577")
    if ws_env > 1:
        torch.distributed.init_process_group(
            backend="nccl" if device == "cuda" else "gloo",
            world_size=ws_env, rank=rank_env)
        require(torch.distributed.is_initialized(), "process group init failed")
        return f"env_world_size_{ws_env}"
    torch.distributed.init_process_group(
        backend="nccl" if device == "cuda" else "gloo", world_size=1, rank=0)
    require(torch.distributed.is_initialized(), "process group init failed")
    return "created_single_process_group"


def prepare_fsdp2(acc, model, device: str = "cpu"):
    """Shard the model with accelerate's OWN FSDP2 path.

    `accelerate.utils.fsdp_utils.fsdp2_prepare_model` is the exact function
    `Accelerator.prepare()` calls on an FSDP2 run (auto-wrap bottom-up on the
    decoder layer, then embed/tail units, mp_policy and reshard_after_forward from
    the plugin). Calling it directly keeps ONE sharding implementation on both the
    3x-GPU path and the CPU/gloo test path - accelerate reaches it from
    `prepare()` only when the backend is a hardware accelerator.
    """
    from accelerate.utils.fsdp_utils import fsdp2_prepare_model
    pg = ensure_process_group(acc, device)
    if acc.is_fsdp2:
        model, _ = acc.prepare(model)
        return model, pg
    return fsdp2_prepare_model(acc, model), pg


def build_optimizer(model, cfg: dict):
    import bitsandbytes as bnb
    o = cfg["optim"]
    params = [p for p in model.parameters() if p.requires_grad]
    require(bool(params), "the model exposes no trainable parameter; a full-parameter "
                          "RL run cannot proceed")
    return bnb.optim.AdamW8bit(
        params, lr=float(o["lr"]), betas=(0.9, 0.95), eps=1e-8,
        weight_decay=float(o["weight_decay"]))


def build_scheduler(optimizer, cfg: dict):
    """warmup_ratio 0.0 -> constant LR, but as a real scheduler so its state (and
    therefore the resumed LR) round-trips through the checkpoint."""
    warmup = float(cfg["optim"].get("warmup_ratio") or 0.0)
    require(warmup == 0.0,
            f"optim.warmup_ratio={warmup} is not the pinned 0.0; a warmup schedule "
            f"that has not been measured on this run is refused")
    return torch.optim.lr_scheduler.LambdaLR(optimizer, lr_lambda=lambda _s: 1.0)


def clip_grads(model, max_norm: float) -> float:
    """Grad-norm clipping that is CORRECT for sharded (DTensor) gradients.

    FSDP2 provides `clip_grad_norm_` on the sharded module, which all-gathers the
    per-shard norms; torch's global function would clip each rank's shard against
    the full norm independently. Prefer the FSDP2 one whenever it exists."""
    fn = getattr(model, "clip_grad_norm_", None)
    if callable(fn):
        try:
            return float(fn(max_norm))
        except Exception:                                       # noqa: BLE001
            pass
    return float(torch.nn.utils.clip_grad_norm_(
        [p for p in model.parameters() if p.requires_grad], max_norm))


# --------------------------------------------------------------------------
# refusals that must fire BEFORE the first forward pass
# --------------------------------------------------------------------------
def check_init_from(path: str) -> str:
    require(bool(path), "init_from is empty. The RL run initialises from the SFT "
                        "best checkpoint; there is no default and no fallback.")
    require(os.path.isdir(path), f"init_from is not a directory: {path}")
    weights = [f for f in sorted(os.listdir(path))
               if f.endswith((".safetensors", ".bin", ".pt", ".pth"))]
    require(bool(weights), f"init_from has no weights file: {path}")
    return path


def check_ref_cache(cfg: dict, args) -> RefActionDist:
    path = args.ref_cache or (cfg.get("reference") or {}).get("action_dist_cache") or ""
    require(bool(path), "no reference action-distribution cache given (--ref-cache "
                        "or reference.action_dist_cache)")
    require(os.path.isfile(path), f"reference cache not found: {path}")
    require(os.path.getsize(path) > 0, f"reference cache is empty: {path}")
    ref = RefActionDist(path)           # refuses on action-order mismatch / empty
    # A cache without the 5-arm joint would silently demote the anchor to the 3-dim
    # decision marginal: size would then be graded by the advantage but held to the
    # SFT prior by NOTHING. Refuse at launch instead of training that quietly.
    require(ref.has_joint(),
            f"reference cache carries no 5-arm joint (ref_arm_probs): {path}. The "
            f"size token would be unregularised. Rebuild it with grpo_ref_cache "
            f"(it emits ref_arm_probs by the chain rule P(BUY_TIER)=P(BUY)*P(TIER|BUY)).")
    return ref


def wall_leaks(train_path: str, wall_ms: int) -> list:
    """The driver's OWN forward-wall leak check.

    Deliberately independent of launch_rl.py::wall_clean (which the launcher runs):
    a driver that relied on its launcher would train on the wall whenever it was
    started by hand. Uses the dataset builder's wall definition (the immutable
    wall report + sha256-verified id list).
    """
    from grpo_dataset import in_wall, wall_state
    w = wall_state()                    # refuses if the wall or ids are missing/changed
    require(int(w["wall_ms"]) == int(wall_ms),
            f"config data.wall_ms={wall_ms} != the recorded forward wall "
            f"{w['wall_ms']} - the wall CHANGED, refusing to trust either value")
    leaks = []
    with open(train_path, encoding="utf-8") as fh:
        for i, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except Exception:                                   # noqa: BLE001
                continue
            t = r.get("t_dec_ms")
            if t is None:
                leaks.append({"line": i, "reason": "record without t_dec_ms"})
            elif int(t) >= int(wall_ms) or in_wall(w, int(t), r.get("mint")):
                leaks.append({"line": i, "mint": r.get("mint"),
                              "t_dec_ms": int(t),
                              "episode_id": r.get("episode_id")})
            if len(leaks) >= 20:
                break
    return leaks


def test_wall_leaks(out=None) -> int:
    """Negative control for the wall leak check (used by the smoke test)."""
    import tempfile
    fails, checks = [], 0

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            fails.append(tag)

    from grpo_dataset import wall_state
    w = wall_state()
    chk("wall_loaded", bool(w.get("wall_ms")))
    tmp2 = os.path.join(tempfile.mkdtemp(), "leaky2.jsonl")
    with open(tmp2, "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"t_dec_ms": int(w["wall_ms"]) + 1, "mint": "x",
                             "episode_id": "b"}) + "\n")
    chk("past_wall_is_a_leak", len(wall_leaks(tmp2, w["wall_ms"])) == 1)
    tmp3 = os.path.join(tempfile.mkdtemp(), "no_tdec.jsonl")
    with open(tmp3, "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"mint": "x"}) + "\n")
    chk("missing_t_dec_is_a_leak", len(wall_leaks(tmp3, w["wall_ms"])) == 1)
    res = {"suite": "wall_leaks", "checks": checks, "failed": fails,
           "verdict": "PASS" if not fails else "FAIL"}
    if out:
        print(json.dumps(res))
    return 1 if fails else 0


# --------------------------------------------------------------------------
# checkpointing
# --------------------------------------------------------------------------
def ckpt_dir(out_root: str, step: int) -> str:
    return os.path.join(out_root, f"checkpoint-{step}")


def find_checkpoints(out_root: str) -> list:
    found = []
    if out_root and os.path.isdir(out_root):
        for name in os.listdir(out_root):
            if name.startswith("checkpoint-"):
                try:
                    found.append((int(name.split("-", 1)[1]), os.path.join(out_root, name)))
                except ValueError:
                    continue
    return sorted(found)


def _rng_bundle() -> dict:
    b = {"torch": torch.get_rng_state()}
    try:
        import random as _random
        b["python"] = _random.getstate()
    except Exception:                                           # noqa: BLE001
        pass
    try:
        import numpy as _np
        b["numpy"] = _np.random.get_state()
    except Exception:                                           # noqa: BLE001
        pass
    return b


def _restore_rng(bundle: dict):
    if "torch" in bundle:
        torch.set_rng_state(bundle["torch"])
    if "python" in bundle:
        import random as _random
        _random.setstate(bundle["python"])
    if "numpy" in bundle:
        import numpy as _np
        _np.random.set_state(bundle["numpy"])


def optimizer_state_digest(optimizer, optim_mod=None) -> dict:
    """Per-parameter state-tensor digest (sha256 + max abs). TEST-SEAM ONLY: on a
    27B run this hashes GBs per save, so it is computed only when the smoke test
    asks for it. Equal digests across save/load prove a bit-exact round-trip."""
    opt = optim_mod.unwrap_optimizer(optimizer) if optim_mod else optimizer
    out = {}
    for gi, g in enumerate(opt.param_groups):
        for pi, p in enumerate(g["params"]):
            st = opt.state.get(p) or {}
            ent = {}
            for k, v in st.items():
                if torch.is_tensor(v):
                    t = v.detach().to("cpu").contiguous()
                    ent[str(k)] = {
                        "dtype": str(t.dtype), "shape": list(t.shape),
                        "max_abs": float(t.abs().max()) if t.numel() else 0.0,
                        "sha256": hashlib.sha256(
                            t.view(torch.uint8).numpy().tobytes()).hexdigest(),
                    }
                else:
                    ent[str(k)] = {"value": v if isinstance(v, (int, float)) else str(v)}
            out[f"g{gi}p{pi}"] = ent
    return out


def _dist_all(flag: bool, device: str, op: str = "and") -> bool:
    """Agree a CONTROL-FLOW decision across ranks.

    FSDP2 shards parameters across ranks, so every forward/backward/generate step is
    a collective. Any branch that changes how many of those a rank performs must be
    identical on every rank, or the run deadlocks in an all-gather. Each rank sees
    DIFFERENT prompts (DistributedSampler), so "this batch was already completed" and
    "this batch has no usable sample" are per-rank facts that MUST be reduced before
    they are acted on.
    """
    if not (torch.distributed.is_available() and torch.distributed.is_initialized()):
        return bool(flag)
    if torch.distributed.get_world_size() <= 1:
        return bool(flag)
    t = torch.tensor([1.0 if flag else 0.0], device=device)
    torch.distributed.all_reduce(
        t, op=(torch.distributed.ReduceOp.MIN if op == "and"
               else torch.distributed.ReduceOp.MAX))
    return bool(float(t.item()) >= 0.5)


def _install_stackdump():
    """RL_STACKDUMP=1 registers a SIGUSR1 stack dump (faulthandler). Diagnostic only:
    it lets a hung multi-rank run be inspected without ptrace permissions."""
    if os.environ.get("RL_STACKDUMP"):
        import faulthandler
        import signal
        try:
            faulthandler.register(signal.SIGUSR1, all_threads=True, chain=False)
        except Exception:                                       # noqa: BLE001
            pass


def _world_rank(acc) -> int:
    """The PROCESS GROUP's rank.

    accelerate reports process_index=0 for every rank when the group was created by
    torchrun/srun with no visible CUDA device (distributed_type=NO). Two ranks would
    then write the same per-rank optimizer/RNG file and one would win.
    """
    if torch.distributed.is_available() and torch.distributed.is_initialized():
        return int(torch.distributed.get_rank())
    return int(acc.process_index)


def _is_main(acc) -> bool:
    """True only on the process group's rank 0 (see _world_rank)."""
    return _world_rank(acc) == 0


def _world_size(acc) -> int:
    if torch.distributed.is_available() and torch.distributed.is_initialized():
        return int(torch.distributed.get_world_size())
    return int(acc.num_processes)


def _sync(acc):
    """Explicit barrier when a real process group exists (accelerate's
    wait_for_everyone() is a no-op when it did not create the group itself, and the
    DCP save/load below is collective)."""
    if torch.distributed.is_available() and torch.distributed.is_initialized():
        torch.distributed.barrier()


def save_checkpoint(acc, plugin, model, optimizer, scheduler, step, out_root,
                    cfg, done_shas, meta: dict, digest=False, optim_mod=None):
    """DCP model shards + PER-RANK optimizer files + scheduler + RNG + step."""
    from accelerate.utils.fsdp_utils import save_fsdp_model, save_fsdp_optimizer
    d = ckpt_dir(out_root, step)
    os.makedirs(d, exist_ok=True)
    acc.wait_for_everyone()
    save_fsdp_model(plugin, acc, model, d)                       # torch.distributed.checkpoint
    save_fsdp_optimizer(plugin, acc, optimizer, model, d, 0, use_dcp=False)
    rank = _world_rank(acc)
    torch.save(_rng_bundle(), os.path.join(d, f"rng_rank{rank}.pt"))
    if digest:
        json.dump(optimizer_state_digest(optimizer, optim_mod),
                  open(os.path.join(d, f"optim_digest_rank{rank}.json"), "w"),
                  indent=1, sort_keys=True)
    _sync(acc)
    if _is_main(acc):
        torch.save(scheduler.state_dict(), os.path.join(d, "scheduler.pt"))
        state = {
            "step": int(step), "world_size": _world_size(acc),
            "done_prompt_sha256": sorted(done_shas),
            "config_sha256": meta.get("config_sha256"),
            "init_from": meta.get("init_from"),
            "wall_gate_history": meta.get("wall_gate_history") or [],
            "saved_at": time.time(),
        }
        tmp = os.path.join(d, "trainer_state.json.tmp")
        json.dump(state, open(tmp, "w"), indent=1, sort_keys=True)
        os.replace(tmp, os.path.join(d, "trainer_state.json"))
        json.dump({"schema": "rl_checkpoint_v1",
                   "model": "DCP sharded (torch.distributed.checkpoint)",
                   "optimizer": "per-rank torch.save files "
                                f"(optimizer_0_rank*.bin) - the DCP optimizer path "
                                f"cannot carry bnb nested quantisation tensors",
                   "scheduler": "scheduler.pt", "rng": f"rng_rank<r>.pt",
                   "state": "trainer_state.json",
                   "files": sorted(os.listdir(d))},
                  open(os.path.join(d, "checkpoint_manifest.json"), "w"),
                  indent=1, sort_keys=True)
    acc.wait_for_everyone()
    return d


def prune_checkpoints(acc, out_root: str, keep_last: int):
    if not _is_main(acc):
        return []
    cks = find_checkpoints(out_root)
    removed = []
    for step, path in cks[:-int(keep_last)] if keep_last > 0 else []:
        shutil.rmtree(path, ignore_errors=True)
        removed.append(path)
    return removed


def load_checkpoint(acc, plugin, model, optimizer, scheduler, step_dir, digest=False,
                    optim_mod=None):
    """Resume: DCP model shards + per-rank optimizer files + scheduler/RNG/step."""
    from accelerate.utils.fsdp_utils import load_fsdp_model, load_fsdp_optimizer
    state_path = os.path.join(step_dir, "trainer_state.json")
    require(os.path.isfile(state_path),
            f"checkpoint {step_dir} has no trainer_state.json; it cannot be resumed "
            f"(a partial checkpoint must not silently restart the run)")
    state = json.load(open(state_path, encoding="utf-8"))
    load_fsdp_model(plugin, acc, model, step_dir)
    load_fsdp_optimizer(plugin, acc, optimizer, model, step_dir, 0, use_dcp=False)
    sched_path = os.path.join(step_dir, "scheduler.pt")
    if os.path.isfile(sched_path) and scheduler is not None:
        scheduler.load_state_dict(torch.load(sched_path, map_location="cpu",
                                             weights_only=False))
    rng_path = os.path.join(step_dir, f"rng_rank{_world_rank(acc)}.pt")
    if os.path.isfile(rng_path):
        _restore_rng(torch.load(rng_path, map_location="cpu", weights_only=False))
    report = {"resumed_from_step": int(state["step"]),
              "loaded": ["model_shards", "optimizer_per_rank", "scheduler", "rng"],
              "done_prompt_sha256": len(state.get("done_prompt_sha256") or []),
              "world_size": state.get("world_size"),
              "optimizer_state_round_trip": None}
    if digest:
        dpath = os.path.join(step_dir, f"optim_digest_rank{_world_rank(acc)}.json")
        if os.path.isfile(dpath):
            want = json.load(open(dpath, encoding="utf-8"))
            got = optimizer_state_digest(optimizer, optim_mod)
            mism, deltas, n_t = 0, [], 0
            for gk, ent in want.items():
                for sk, ev in ent.items():
                    if "sha256" not in ev:
                        continue
                    n_t += 1
                    gv = (got.get(gk) or {}).get(sk)
                    if gv is None or gv.get("sha256") != ev["sha256"]:
                        mism += 1
                    else:
                        deltas.append(abs(float(gv["max_abs"]) - float(ev["max_abs"])))
            report["optimizer_state_round_trip"] = {
                "tensors_compared": n_t, "sha256_mismatches": mism,
                "max_abs_diff": (max(deltas) if deltas else 0.0),
                "verdict": "BIT-EXACT" if mism == 0 else "MISMATCH"}
    return state, report


# --------------------------------------------------------------------------
# forward wall eval + the PRE-REGISTERED gate
# --------------------------------------------------------------------------
def episode_evidence(rec: dict):
    """Every wall episode must expose the RESERVES it used plus staleness_ms."""
    prov = rec.get("provenance") or {}
    reserves = rec.get("reserves")
    if reserves is None:
        if "reserve_sources" in prov or "reserve_joins_count" in prov:
            reserves = {"sources": prov.get("reserve_sources"),
                        "joins": prov.get("reserve_joins_count")}
    require(reserves is not None,
            "wall episode exposes no reserves (need `reserves` or a provenance "
            "block with reserve_sources/reserve_joins_count)")
    stale = rec.get("staleness_ms")
    if stale is None:
        stale = prov.get("staleness_ms_max")
    require(stale is not None,
            "wall episode exposes no staleness_ms (need `staleness_ms` or "
            "provenance.staleness_ms_max)")
    return reserves, float(stale)


def load_wall_eval(cfg: dict):
    """The wall evaluation set. REFUSES, before any forward pass, if it cannot be
    resolved into scored episodes that carry a prompt, a counterfactual group and
    the reserves/staleness evidence - never fabricates a prompt or a return."""
    eval_path = (cfg.get("data") or {}).get("eval") or ""
    require(bool(eval_path) and os.path.isfile(eval_path),
            f"forward-wall eval set not found: {eval_path!r}")
    rows, bad = [], {}
    with open(eval_path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except Exception:                                   # noqa: BLE001
                bad["unparseable_json"] = bad.get("unparseable_json", 0) + 1
                continue
            missing = [k for k in ("prompt", "prompt_sha256", "group")
                       if not r.get(k)]
            if missing:
                for k in missing:
                    bad[k] = bad.get(k, 0) + 1
                continue
            try:
                episode_evidence(r)
            except SystemExit as exc:
                bad[f"evidence:{str(exc)[:40]}"] = \
                    bad.get(f"evidence:{str(exc)[:40]}", 0) + 1
                continue
            rows.append(r)
    require(bool(rows),
            f"the forward-wall eval set {eval_path} cannot be evaluated: 0 of the "
            f"episodes carry a prompt + counterfactual group + reserve/staleness "
            f"evidence ({bad}). The wall file that exists today "
            f"(wall_episodes.jsonl) holds only {{t_dec_ms, mint, family, split}}, so "
            f"the scored wall records still have to be produced by the dataset "
            f"builder before the gate can be applied. Refusing to fabricate prompts "
            f"or returns.")
    return eval_path, rows, bad


# ===========================================================================
# WALL-EVAL OBSERVABILITY (change 3). OBSERVATION ONLY - no sampling, no
# advantage, no loss is touched by anything below. It exists because two failure
# modes are invisible in the gate's scalar: a policy that has COLLAPSED onto one
# action (the gate can look healthy while the policy has stopped deciding), and a
# policy that ARGUES one action in its rationale and EMITS another.
# ===========================================================================

# The DECISION arm reduces to a FAMILY for the knowing-doing probe. SIZE is a
# separate slot and is deliberately excluded: a rationale cannot be asked to imply
# FULL vs SMALL, so comparing sizes would manufacture disagreement.
# A bare "BUY" (a size-less arm some legacy fixtures score) is deliberately NOT a
# key: parse_decision refuses a BUY without a usable SIZE, so no completion can
# ever emit it, and counting it in the entropy basis would deflate the ratio with an
# action the policy cannot take.
_ARM_FAMILY = {"BUY_SMALL": "BUY", "BUY_MID": "BUY", "BUY_FULL": "BUY",
               "WATCH": "WATCH", "SKIP": "SKIP"}

# Deterministic rationale -> family lexicon. FROZEN on purpose (pinned patterns),
# so the agreement number is comparable run to run. A rationale matching MORE THAN
# ONE family is NOT resolved by precedence - it is UNPARSEABLE, because picking a
# winner would manufacture agreement that the text does not actually state.
_RATIONALE_FAMILIES = (
    ("BUY", re.compile(
        r"\b(?:buy|buying|enter|entering|entered"
        r"|open(?:ing)?\s+(?:a\s+)?(?:long|position)|accumulat\w*"
        r"|add(?:ing)?\s+to|scale\s+in|take\s+(?:a\s+)?position)\b", re.I)),
    ("WATCH", re.compile(
        r"\b(?:watch|watching|wait|waiting|monitor|monitoring|observ\w*"
        r"|hold\s+off|stand\s+aside|defer\w*|no\s+(?:immediate\s+)?entry"
        r"|reassess)\b", re.I)),
    ("SKIP", re.compile(
        r"\b(?:skip|skipping|pass|passing|avoid|avoiding|no\s+trade"
        r"|do(?:es)?\s+not\s+(?:buy|enter)|don'?t\s+(?:buy|enter)"
        r"|no\s+position|stay\s+out)\b", re.I)),
)


def rationale_implied_family(completion: str):
    """The action family the completion's OWN rationale implies, or None.

    Only the text BEFORE the first DECISION marker is read: the DECISION field states
    the action outright, so including it would make the probe tautologically agree
    with itself. Zero or multiple family matches -> None (unparseable), never a guess.
    """
    if not completion:
        return None
    cut = len(completion)
    for marker in ("DECISION", "Decision", "decision"):
        i = completion.find(marker)
        if i != -1:
            cut = min(cut, i)
    rationale = completion[:cut]
    hits = [fam for fam, rx in _RATIONALE_FAMILIES if rx.search(rationale)]
    return hits[0] if len(hits) == 1 else None


def arm_histogram_entropy(arms: list, emittable: list) -> dict:
    """Per-arm action histogram + policy entropy over the arm distribution.

    `arms` is every evaluated action (None = the completion emitted no parseable
    DECISION). Entropy is taken over the EMITTABLE arm basis - the arms this eval set
    can actually score (the episodes' group keys) - so the maximum-entropy value is
    log(K) for K emittable arms: the ratio is 1.0 for a uniform policy and 0.0 for a
    fully collapsed one. Normalising against the OBSERVED support instead would
    flatter a collapsed policy (a policy emitting one arm would score ratio 1.0).
    Unparseable completions are reported as their own count and are EXCLUDED from the
    distribution: a failed parse is not an action choice.
    """
    n_total = len(arms)
    hist = {a: 0 for a in emittable}
    n_unparsed = 0
    for a in arms:
        if a is None or a not in hist:
            n_unparsed += 1
        else:
            hist[a] += 1
    n_parsed = sum(hist.values())
    if n_parsed:
        p = [c / n_parsed for c in hist.values() if c > 0]
        ent = float(-sum(pi * math.log(pi) for pi in p))
    else:
        ent = 0.0
    K = len(emittable)
    max_ent = math.log(K) if K > 1 else None
    return {"histogram": hist, "n_decisions": n_total, "n_parsed": n_parsed,
            "n_unparseable": n_unparsed,
            "unparseable_frac": (n_unparsed / n_total) if n_total else 0.0,
            "entropy_nats": ent, "n_emittable_arms": K,
            "max_entropy_nats": max_ent,
            "entropy_ratio": (ent / max_ent) if max_ent else None,
            "collapsed": bool(n_parsed and
                              len([c for c in hist.values() if c]) == 1)}


def evaluate_wall(model, tok, cfg, episodes, ref_dist, device, action_ids, actions,
                  completions=None):
    """Roll out on wall episodes; per-episode net return from the counterfactual
    group of the SAMPLED decision. Selection input for the gate ONLY - the training
    reward is never an input here.

    ALSO returns a third value: the OBSERVABILITY metrics (per-arm action histogram,
    arm-distribution entropy raw + ratio-to-max, and the knowing-doing consistency
    probe). They read the same rollout the gate already computes and are observation
    ONLY: sampling, advantages and the loss are untouched. They exist because a
    scalar gate cannot see a COLLAPSED policy (one action for every decision) or a
    policy that argues one action and emits another.
    """
    G = int(cfg["loop"]["group_size"])
    returns, per_ep = [], []
    all_arms, all_comps = [], []
    # the EMITTABLE arm basis for THIS eval set: the arms the episodes can score.
    # The entropy ratio is taken against log(K) over this basis, so a collapsed
    # policy reads 0.0 and a uniform one reads 1.0 - not against the observed
    # support, which would flatter a collapsed policy.
    emittable = sorted({a for ep in episodes for a in (ep.get("group") or {})
                        if a in _ARM_FAMILY})
    n_cmp = n_agree = n_unparsed = n_no_action = n_no_sig = 0
    for ep in episodes:
        msgs = json.loads(ep["prompt"])
        text = render_prompt(tok, msgs)
        enc = tok(text, return_tensors="pt", truncation=True, max_length=4096)
        batch = {"input_ids": enc["input_ids"], "attention_mask": enc["attention_mask"],
                 "meta": [{"prompt_sha256": ep["prompt_sha256"], "group": ep["group"],
                           "advantages": ep["advantages"], "prompt_text": text,
                           "mint": ep.get("mint"), "t_dec_ms": ep.get("t_dec_ms"),
                           "episode_id": ep.get("episode_id")}]}
        if completions is not None:
            roll = rollout(model, tok, batch, cfg, device,
                           completions={ep["prompt_sha256"]: completions[
                               ep["prompt_sha256"]]} if ep["prompt_sha256"] in
                               completions else None)
        else:
            roll = rollout(model, tok, batch, cfg, device)
        acts = roll[0]["actions"]
        comps = roll[0].get("completions") or []
        all_arms.extend(acts)
        all_comps.extend(comps)
        # knowing-doing: emitted action family vs the family the completion's OWN
        # rationale implies (text before DECISION). Both unparseable cases are
        # COUNTED, never silently folded into agreement.
        for comp in comps:
            emitted = parse_decision(comp)
            fam_e = _ARM_FAMILY.get(emitted) if emitted else None
            fam_i = rationale_implied_family(comp)
            if fam_e is None or fam_i is None:
                n_unparsed += 1
                n_no_action += 1 if fam_e is None else 0
                n_no_sig += 1 if fam_i is None else 0
            else:
                n_cmp += 1
                n_agree += 1 if fam_e == fam_i else 0
        reserves, stale = episode_evidence(ep)
        skip_v = float((ep.get("group") or {}).get("SKIP", 0.0) or 0.0)
        vals, n_fallback = [], 0
        for a in acts:
            if a is None:
                vals.append(skip_v)                    # no position taken -> the SKIP arm
                n_fallback += 1
            else:
                vals.append(float(ep["group"][a]))
        r = sum(vals) / max(len(vals), 1)
        returns.append(r)
        per_ep.append({"episode_id": ep.get("episode_id"), "mint": ep.get("mint"),
                       "t_dec_ms": ep.get("t_dec_ms"), "net_sol_return": r,
                       "n_samples": len(vals), "n_unparseable_fallback": n_fallback,
                       "reserves": reserves, "staleness_ms": stale,
                       "actions": acts})
    metrics = arm_histogram_entropy(all_arms, emittable)
    metrics["knowing_doing"] = {
        "n_completions": len(all_comps),
        "n_compared": n_cmp,
        "n_agree": n_agree,
        "agreement_rate": (n_agree / n_cmp) if n_cmp else None,
        "n_unparseable": n_unparsed,
        "n_unparseable_no_emitted_action": n_no_action,
        "n_unparseable_no_rationale_signal": n_no_sig,
        "note": ("emitted action family vs the family implied by the text BEFORE the "
                 "DECISION marker; a completion with no parseable action or no single "
                 "unambiguous rationale family is UNPARSEABLE and is never counted as "
                 "agreement"),
    }
    return returns, per_ep, metrics


def memorization_precheck() -> dict:
    """FAIL-CLOSED memorization gate for checkpoint SELECTION.

    A candidate is selectable only with a CLEAN verdict from a PREREGISTERED instrument:
    the method and thresholds are registered before the run (MEMORIZATION_BOUNDS.json),
    and the records (in-sample vs unseen decisions from the same policy) come from the
    evaluation step. Missing either one is a REFUSAL, not a pass - an instrument nothing
    feeds is not an instrument.
    """
    import subprocess
    if not os.path.isfile(MEM_PREREG):
        return {"ok": False, "reason": "no preregistered instrument at %s" % MEM_PREREG}
    if not os.path.isfile(MEM_RECORDS):
        return {"ok": False,
                "reason": ("no records at %s - the eval step must score matched in-sample and "
                           "unseen prompts through the same policy" % MEM_RECORDS)}
    # RANK-SAFE, ATOMIC PUBLISH. run_gate is called on EVERY rank, so every rank runs
    # this detector over the same records. Writing MEM_SCORE directly would have the
    # ranks race on one path (a truncated read -> "detector produced no report" ->
    # a spurious refusal). Each rank therefore scores into its OWN file and then
    # os.replace()s it onto MEM_SCORE: replace is atomic, all ranks compute the same
    # verdict from the same inputs, and the artifact of record is still MEM_SCORE.
    mine = "%s.p%d" % (MEM_SCORE, os.getpid())
    rc = subprocess.run([sys.executable, os.path.join(HERE, "memorization_detector.py"),
                         "--score", MEM_RECORDS, "--prereg", MEM_PREREG, "--out", mine],
                        capture_output=True, text=True)
    try:
        rep = json.load(open(mine, encoding="utf-8"))
        os.replace(mine, MEM_SCORE)
    except Exception:
        try:
            os.unlink(mine)                                      # noqa: B018
        except OSError:
            pass
        return {"ok": False, "reason": "detector produced no report (rc=%d)" % rc.returncode}
    rep["ok"] = bool(rc.returncode == 0 and rep.get("status") == "clean")
    if not rep["ok"]:
        rep.setdefault("reason", "verdict=%s" % rep.get("status"))
    return rep


def memorization_emit_enabled(cfg: dict, cli_flag: bool = False) -> bool:
    """Is the self-feeding memorization emitter switched ON for this run?

    `selection.emit_memorization_records` in the config, or --emit-mem-records on the
    command line. ABSENT = OFF, which is the unchanged, fail-closed behaviour: the
    gate then scores whatever file is already at MEM_RECORDS (or refuses when there
    is none). See the module docstring's CORRESPONDENCE GUARANTEE.
    """
    sel = (cfg or {}).get("selection") or {}
    return bool(cli_flag or sel.get("emit_memorization_records"))


def verify_records_provenance(path: str, candidate: str, step: int) -> dict:
    """READ BACK a records file and REFUSE unless every row names (candidate, step).

    This is the second half of the correspondence guarantee: the emitter always
    OVERWRITES the fixed path, so a stale file cannot survive an emission - but a
    PARTIAL write (a crash, a hand-edit, a second writer) could leave rows that belong
    to another policy. Such a file must never authorise a selection, so this is
    checked, loudly, on the file the gate is about to score.
    """
    bad = n = 0
    examples = []
    try:
        fh = open(path, encoding="utf-8")
    except OSError as e:                                         # noqa: BLE001
        raise SystemExit("REFUSING: memorization records unreadable at %s (%s) - a "
                         "gate cannot score a file it cannot read" % (path, e))
    with fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            n += 1
            try:
                p = (json.loads(line).get("provenance") or {})
            except Exception:                                    # noqa: BLE001
                bad += 1
                continue
            if p.get("ckpt") != candidate or int(p.get("step", -999)) != int(step):
                bad += 1
                if len(examples) < 3:
                    examples.append({"ckpt": p.get("ckpt"), "step": p.get("step")})
    if n == 0 or bad:
        raise SystemExit("REFUSING: %d/%d memorization records do not name the "
                         "candidate %s@%s - a verdict from another policy must never "
                         "authorise this checkpoint %s"
                         % (bad, n, candidate, step, examples))
    return {"rows": n, "ckpt": candidate, "step": int(step)}


def emit_selection_memorization_records(model, tok, cfg: dict, device: str,
                                        candidate: str, step: int = -1,
                                        completions=None, write: bool = True) -> dict:
    """Emit reports/MEMORIZATION_RECORDS.jsonl FROM THE CANDIDATE POLICY, right now.

    This is the self-feeding half of the veto. It scores matched in-sample and unseen
    prompts through the policy this process is HOLDING (the candidate about to be
    judged), writes the records the detector reads, and then PROVES the file it wrote
    names that candidate - the loop refuses to select otherwise.

    The policy is passed by OBJECT (model/tokenizer), never reloaded from disk: the
    candidate under selection may be a sharded in-memory state whose on-disk form is
    not the object being judged. Failures raise; the caller turns that into a refusal.

    `write=False` is the NON-MAIN-RANK half of a multi-rank run: the GENERATION and
    pricing still happen (rollout's decode loop carries FSDP2 collectives, so every
    rank must execute the same number of decode steps or the run hangs), but only the
    main rank writes the file. The caller must barrier after this returns.
    """
    import emit_memorization_records as emr
    sel = (cfg or {}).get("selection") or {}
    # ALWAYS the path the gate reads. An override here would let the emitter write a
    # file the detector never scores, which is exactly the stale-file failure this
    # switch exists to remove.
    out = MEM_RECORDS
    prov = {"ckpt": candidate, "step": int(step),
            "emitted_by": "grpo_loop.emit_selection_memorization_records",
            "correspondence": ("records scored from the candidate policy held in "
                               "memory at selection time")}
    st = emr.produce(ckpt=candidate, model=model, tokenizer=tok,
                     train_file=sel.get("mem_train_file") or
                     (cfg.get("data") or {}).get("train") or emr.DEFAULT_TRAIN,
                     validation_file=sel.get("mem_validation_file")
                     or emr.DEFAULT_VALIDATION,
                     out=out, limit=int(sel.get("mem_limit") or 0), device=device,
                     seed=int(sel.get("mem_seed") or 7),
                     report_path=sel.get("mem_report") or emr.DEFAULT_REPORT,
                     completions=completions, cpk_provenance=prov,
                     run_detector=False, write=write)
    if not write:
        return dict(st, provenance_verified=None, wrote=False)
    if st.get("written", 0) < 60:
        raise SystemExit("REFUSING: the memorization emitter wrote only %s rows"
                         % st.get("written"))
    # READ BACK, and refuse unless the file provably belongs to this candidate.
    st["provenance_verified"] = verify_records_provenance(out, candidate, step)
    return st


def run_gate(returns, cfg: dict) -> dict:
    from grpo_trainer import GRPOConfig, gate_lower_bound
    g = cfg["gate"]
    gcfg = GRPOConfig(gate_floor_net_sol=float(g["gate_floor_net_sol"]),
                      gate_min_episodes=int(g["gate_min_episodes"]),
                      gate_confidence=float(g["gate_confidence"]))
    out = gate_lower_bound(returns, gcfg)
    out["preregistered"] = {"floor_net_sol": gcfg.gate_floor_net_sol,
                            "min_episodes": gcfg.gate_min_episodes,
                            "confidence": gcfg.gate_confidence}
    out["selected_on"] = "wall_gate_only"
    out["training_reward_used_for_selection"] = False
    # SELECTION NEEDS BOTH: the wall lower bound AND a clean memorization verdict. The
    # wall alone cannot see a policy whose choices encode which rows it trained on.
    mem = memorization_precheck()
    out["memorization"] = mem
    if out.get("passes") and not mem.get("ok"):
        out["passes"] = False
        out["reason"] = "wall gate passed but memorization is not clean: %s" % mem.get("reason")
    return out


# --------------------------------------------------------------------------
# the training loop
# --------------------------------------------------------------------------
def load_test_completions(path: str):
    """TEST-ONLY seam. A tiny randomly-initialised model cannot emit a parseable
    'DECISION: X', so the smoke test supplies fixed completions per prompt_sha256.
    Refused unless --device cpu, and it cannot be reached from launch_rl.py's fixed
    command line, so production behaviour is untouched."""
    require(os.path.isfile(path), f"--test-completions file not found: {path}")
    comp = {}
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        r = json.loads(line)
        comp[r["prompt_sha256"]] = list(r["completions"])
    require(bool(comp), f"--test-completions file is empty: {path}")
    return comp, hashlib.sha256(open(path, "rb").read()).hexdigest()


def run_train(a) -> int:
    t_start = time.time()
    _install_stackdump()
    refmod = _ref_contract()
    optim_mod = refmod.fsdp2_bnb
    cfg = load_config(a.config)
    cfg_sha = hashlib.sha256(open(a.config, "rb").read()).hexdigest()
    loop, optim_cfg, ckpt_cfg, gate_cfg = (cfg["loop"], dict(cfg["optim"]),
                                           cfg["checkpoint"], cfg["gate"])
    device = resolve_device(getattr(a, "device", "auto"))
    out_root = a.out_root or os.path.join("/training/runs",
                                          str(cfg.get("run_id") or "rl"))
    max_steps = int(getattr(a, "max_steps", 0) or 0) or int(optim_cfg["max_steps"])
    test_comp, test_sha = None, None
    if getattr(a, "test_completions", ""):
        require(device == "cpu",
                "--test-completions is a TEST seam: it is refused on an accelerator "
                "because a real run must roll out the real policy")
        test_comp, test_sha = load_test_completions(a.test_completions)

    # ---- LOUD REFUSALS: all of them, before any forward pass -----------------
    init_from = check_init_from(a.init_from or cfg.get("init_from") or "")
    train_path = (cfg.get("data") or {}).get("train") or ""
    require(os.path.isfile(train_path), f"RL dataset not found: {train_path!r}")
    require(os.path.getsize(train_path) > 0, f"RL dataset is empty: {train_path}")
    ref_dist = check_ref_cache(cfg, a)               # raises on empty / order mismatch
    wall_ms = int((cfg.get("data") or {}).get("wall_ms") or 0)
    require(wall_ms > 0, "data.wall_ms is missing; the forward wall is not optional")
    leaks = wall_leaks(train_path, wall_ms)
    require(not leaks,
            f"FORWARD-WALL LEAK in the RL training file {train_path}: {leaks[:5]} "
            f"({len(leaks)} shown/capped). Nothing at or after the wall is ever "
            f"trained on.")
    eval_path, wall_eps, _bad = load_wall_eval(cfg)
    require(len(wall_eps) >= 1, "the wall eval set is empty")
    require(str(loop.get("kl_kind")) == "action_distribution",
            f"loop.kl_kind={loop.get('kl_kind')!r}; the action-distribution KL is the "
            f"pinned anchor (the cached reference belongs to the SFT completion, so a "
            f"token-level KL is not computable)")
    print(json.dumps({"stage": "refusals-passed", "init_from": init_from,
                      "train": train_path, "ref_cache": ref_dist.path,
                      "ref_rows": len(ref_dist.map), "wall_ms": wall_ms,
                      "wall_eval_episodes": len(wall_eps),
                      "wall_eval_path": eval_path, "device": device},
                     indent=1))

    # ---- model + tokenizer ---------------------------------------------------
    from transformers import AutoModelForCausalLM, AutoTokenizer
    tok = AutoTokenizer.from_pretrained(init_from, local_files_only=True)
    require(tok.pad_token_id is not None or tok.eos_token_id is not None,
            "tokenizer has neither a pad nor an eos token; left-padded rollout "
            "cannot be built")
    # The scoring forward tokenizes (prompt + completion) with padding, and the
    # per-row prompt boundary is only valid under RIGHT padding (the prompt keeps
    # its offset; left padding would shift it by the pad length). Pin it here
    # instead of inheriting whatever the tokenizer shipped with.
    tok.padding_side = "right"
    ds = RLDataset(train_path, tok)
    require(len(ds) > 0,
            f"the RL dataset has no usable record (every row is missing "
            f"group_usable): {train_path}")
    actions, action_ids = action_token_ids(tok)   # action_token_ids returns (names, ids)
    _size_labels, size_ids = size_token_ids(tok)  # SIZE basis for the arm joint KL
    require(len(action_ids) == len(actions) == 3,
            f"action token ids {action_ids} do not resolve for {actions}")

    model = AutoModelForCausalLM.from_pretrained(
        init_from, local_files_only=True, torch_dtype=torch.bfloat16,
        attn_implementation="sdpa")
    model.config.use_cache = False
    require(all(p.requires_grad for p in model.parameters()),
            "some parameter is frozen; this phase is FULL-parameter RL")
    decoder_cls = refmod.decoder_layer_class_name(model)
    require(decoder_cls is not None,
            "cannot derive the repeating decoder-layer class from the module tree, so "
            "FSDP auto-wrap has nothing to wrap. REFUSING to train.")
    plan_path, pin, cfgd = resolve_plan(cfg, decoder_cls)
    print(f"[plan] {plan_path}: auto-wrap {decoder_cls} "
          f"(fsdp_version={cfgd['fsdp_version']}, "
          f"reshard_after_forward={cfgd['fsdp_reshard_after_forward']}, "
          f"{cfgd['fsdp_state_dict_type']}, mixed_precision="
          f"{cfgd['mixed_precision']}, cpu_offload={pin.get('cpu_offload')})")

    acc, plugin = build_accelerator(cfg, cfgd, device)
    model, pg_source = prepare_fsdp2(acc, model, device)
    print(f"[fsdp2] process group: {pg_source}")

    # ---- optimizer: bound to the LOCAL SHARD after sharding ------------------
    optim_name = "adamw_bnb_8bit"
    refmod.assert_pinned_optimizer_shards_under_dtensor(
        "rl_before_optimizer_creation", optim_name)
    optimizer = build_optimizer(model, cfg)
    scheduler = build_scheduler(optimizer, cfg)
    shim = GuardShim(acc, model, optimizer, optim_name)
    bind_report = refmod.bind_fsdp2_local_shard_optimizer(
        shim, "rl_after_fsdp2_sharding")
    require(bool(bind_report.get("bound")),
            f"the pinned 8-bit optimizer was not bound to the local shard: "
            f"{bind_report}")
    refmod.assert_no_fp32_master_set(shim, "rl_after_fsdp2_sharding", model)

    # ---- resume / fresh ------------------------------------------------------
    cks = find_checkpoints(out_root)
    start_step, done_shas, resume_report = 0, set(), None
    gate_history = []
    if a.mode == "resume":
        require(bool(cks),
                f"--mode resume but no checkpoint-* exists under {out_root!r}. "
                f"Refusing: a resume must NEVER silently restart from scratch.")
        step_dir = cks[-1][1]
        state, resume_report = load_checkpoint(
            acc, plugin, model, optimizer, scheduler, step_dir,
            digest=bool(test_comp), optim_mod=optim_mod)
        start_step = int(state["step"])
        done_shas = set(state.get("done_prompt_sha256") or [])
        gate_history = list(state.get("wall_gate_history") or [])
        require(start_step > 0,
                f"the checkpoint {step_dir} records step={start_step}; refusing to "
                f"resume from a zero-step checkpoint as if it were progress")
        print(json.dumps({"stage": "resumed", "from": step_dir,
                          "resume_report": resume_report}, indent=1))
    elif cks:
        print(json.dumps({"stage": "fresh-run-with-existing-checkpoints",
                          "note": "mode=fresh starts from the initial weights; the "
                                  "existing checkpoints are kept but NOT loaded",
                          "ignored_checkpoints": [p for _s, p in cks]}, indent=1))

    # The distributed topology comes from the PROCESS GROUP, not from accelerate's
    # bookkeeping: a torchrun/srun launch with no visible CUDA device reaches the
    # driver with a live gloo group while accelerate reports a single process, and the
    # sampler must partition by the REAL world size or every rank trains the same
    # prompts.
    pg_ws, pg_rank = _world_size(acc), _world_rank(acc)
    ws = max(int(pg_ws), int(acc.num_processes))
    rank = pg_rank
    print(f"[topology] world_size={ws} rank={rank} "
          f"(process_group={pg_ws}, accelerate={acc.num_processes})")

    cur = curriculum_config(loop)
    curriculum_reports = []

    def _make_loader(curriculum_on: bool):
        """Build the loader. When the curriculum is ON the sampler must NOT shuffle:
        the schedule IS the order, and an independently-shuffled index order would
        silently discard it."""
        smp = None
        if ws > 1:
            smp = DistributedSampler(ds, num_replicas=ws, rank=rank,
                                     shuffle=not curriculum_on, seed=1234)
        ld = DataLoader(ds, batch_size=int(loop["batch_prompts"]), sampler=smp,
                        shuffle=False,
                        collate_fn=lambda b: collate(b, tok.pad_token_id
                                                     or tok.eos_token_id),
                        drop_last=False)
        return ld, smp

    loader, sampler = _make_loader(bool(cur["enabled"]))
    require(len(loader) > 0, "the RL dataloader has no batch")
    cur_plan = None
    if cur["enabled"]:
        # SCORE ONCE over the file order, then permute per epoch: re-scoring a
        # permuted list would reorder ties differently and drift the schedule.
        cur_plan = curriculum_plan(ds.rows, cur)
        ds.rows, rep = curriculum_order(cur_plan, epoch=0)
        curriculum_reports.append(rep)
        print(f"[curriculum] ON metric={rep['metric']} stages={rep['stages']} "
              f"n={rep['n_rows']} bands={rep['band_sizes']} "
              f"spread[min/med/max]={rep['spread_min']:.6g}/"
              f"{rep['spread_median']:.6g}/{rep['spread_max']:.6g} "
              f"order_sha256={rep['order_sha256'][:16]}")
    else:
        print("[curriculum] OFF (loop.curriculum absent or false): file order, "
              "legacy-uniform schedule")
    grad_accum = int(loop["grad_accum"])
    save_steps = int(ckpt_cfg["save_steps"])
    keep_last = int(ckpt_cfg["keep_last"])
    max_grad_norm = float(optim_cfg["max_grad_norm"])
    from grpo_loss import GRPOLossConfig
    cfg_loss = GRPOLossConfig(kl_coef=float(loop["kl_coef"]),
                              normalize_advantage=bool(loop["normalize_advantage"]))
    epochs = max(1, int(math.ceil(float(optim_cfg["epochs"]) or 1.0)))
    step, micro, opt_steps, n_skipped = start_step, 0, 0, 0
    last_stats, train_reward_hist = {}, []
    sharded_checked = False
    t0 = time.time()
    stop = False
    for epoch in range(epochs):
        if stop:
            break
        if cur["enabled"] and cur_plan is not None and epoch > 0:
            # advance the ladder: same membership, a different starting band (the
            # rotation wraps, so no row is ever dropped from an epoch).
            ds.rows, rep = curriculum_order(cur_plan, epoch=epoch)
            curriculum_reports.append(rep)
            loader, sampler = _make_loader(True)
            print(f"[curriculum] epoch {epoch}: start_band={rep['start_band']} "
                  f"order_sha256={rep['order_sha256'][:16]}")
        if sampler is not None:
            sampler.set_epoch(epoch)
        for batch in loader:
            if step >= max_steps:
                stop = True
                break
            shas = [m["prompt_sha256"] for m in batch["meta"]]
            if all(s in done_shas for s in shas):
                n_skipped += 1
                continue                                  # already-completed prompts
            loss, stats = train_step(model, tok, batch, cfg, ref_dist, device,
                                     action_ids, actions, cfg_loss, backward=True,
                                     completions=test_comp, grad_scale=1.0 / grad_accum,
                                     size_ids=size_ids)
            last_stats = stats
            if loss is None:                              # masked batch: no gradient
                n_skipped += 1
                continue
            micro += 1
            if not sharded_checked:
                # WHILE the accumulated gradient is resident, before it is consumed
                # (an FSDP grads-present check after zero_grad() has nothing to
                # inspect). This is the first moment a sharded gradient exists.
                refmod.assert_sharded_gradients(shim, "rl_after_first_backward")
                sharded_checked = True
            if micro < grad_accum:
                continue
            grad_norm = clip_grads(model, max_grad_norm)
            optimizer.step()
            scheduler.step()
            optimizer.zero_grad(set_to_none=True)
            micro, opt_steps, step = 0, opt_steps + 1, step + 1
            done_shas.update(shas)
            train_reward_hist.append({"step": step, "loss": stats.get("loss"),
                                      "n_active": stats.get("n_active")})
            if _is_main(acc) and (step % 10 == 0 or step - start_step <= 2):
                print(json.dumps({"step": step, "max_steps": max_steps,
                                  "loss": stats.get("loss"),
                                  "kl_action": stats.get("kl_action"),
                                  "n_active": stats.get("n_active"),
                                  "n_no_slot": stats.get("n_no_decision_slot"),
                                  "grad_norm": grad_norm,
                                  "sec": round(time.time() - t0, 1)}))
            if save_steps > 0 and step % save_steps == 0:
                d = save_checkpoint(acc, plugin, model, optimizer, scheduler, step,
                                    out_root, cfg, done_shas,
                                    {"config_sha256": cfg_sha, "init_from": init_from,
                                     "wall_gate_history": gate_history},
                                    digest=bool(test_comp), optim_mod=optim_mod)
                print(f"[checkpoint] {d}")
                rets, per_ep, wall_metrics = [], [], None
                try:
                    rets, per_ep, wall_metrics = evaluate_wall(
                        model, tok, cfg, wall_eps, ref_dist,
                        device, action_ids, actions, test_comp)
                    # SELF-FEEDING VETO (see the module docstring): with the switch ON,
                    # the records the gate is about to score are emitted FROM THIS
                    # CANDIDATE, right now, and the file is read back and refused unless
                    # it names it. OFF (the default) changes nothing at all.
                    if memorization_emit_enabled(cfg,
                                                 getattr(a, "emit_mem_records", False)):
                        # EVERY rank generates (rollout carries FSDP2 collectives);
                        # only main writes, then all ranks barrier before the gate
                        # reads the file. See emit_selection_memorization_records.
                        est = emit_selection_memorization_records(
                            model, tok, cfg, device, candidate=d, step=step,
                            completions=test_comp, write=_is_main(acc))
                        if _is_main(acc):
                            print(f"[memorization] emitted {est.get('written')} records "
                                  f"(in={est.get('n_in_sample')} "
                                  f"unseen={est.get('n_unseen')}) for {d}@step{step}; "
                                  f"provenance verified: "
                                  f"{json.dumps(est.get('provenance_verified'))}")
                        acc.wait_for_everyone()
                    gate = run_gate(rets, cfg)
                except SystemExit as exc:
                    gate = {"passes": False, "reason": f"wall eval refused: {exc}"}
                if wall_metrics is not None:
                    # observation only: these ride on the gate record purely so the
                    # run's logged eval output carries collapse + consistency. They
                    # never enter sampling, advantages or the loss.
                    gate["eval_metrics"] = wall_metrics
                gate_history.append({"step": step, "gate": gate,
                                     "eval_metrics": wall_metrics,
                                     "training_reward": stats.get("loss")})
                if _is_main(acc):
                    with open(os.path.join(out_root, "gate_history.jsonl"), "a",
                              encoding="utf-8") as fh:
                        fh.write(json.dumps(gate_history[-1]) + "\n")
                    print(f"[gate] step {step}: passes={gate.get('passes')} "
                          f"{json.dumps(gate.get('preregistered') or {})} "
                          f"lb={gate.get('lower_bound')} n={gate.get('n')}")
                    if wall_metrics is not None:
                        print(f"[eval] step {step}: "
                              f"{json.dumps(wall_metrics, sort_keys=True)}")
                if gate.get("passes"):
                    selection = {
                        "selected_checkpoint": ckpt_dir(out_root, step),
                        "step": step, "gate": gate, "selected_on": "wall_gate_only",
                        "never_selected_on": "training_reward",
                        "training_reward_diagnostic": stats.get("loss"),
                        "wall_eval_path": eval_path,
                        "wall_episodes": len(wall_eps),
                        "preregistered": gate.get("preregistered"),
                    }
                    if _is_main(acc):
                        tmp = os.path.join(out_root, "selection.json.tmp")
                        json.dump(selection, open(tmp, "w"), indent=1, sort_keys=True)
                        os.replace(tmp, os.path.join(out_root, "selection.json"))
                        print(f"[selection] {json.dumps(selection)}")
                prune_checkpoints(acc, out_root, keep_last)
    acc.wait_for_everyone()

    final = {"schema": "rl_train_report_v1", "run_id": cfg.get("run_id"),
             "mode": a.mode, "started_from_step": start_step, "steps_done": step,
             "optimizer_steps_this_process": opt_steps, "max_steps": max_steps,
             "skipped_batches": n_skipped, "device": device,
             "world_size": ws, "process_index": rank,
             "init_from": init_from, "config_sha256": cfg_sha,
             "ref_cache": ref_dist.path, "fsdp2_plan": plan_path,
             "decoder_layer_cls": decoder_cls, "process_group": pg_source,
             "local_shard_bind": {k: (v if isinstance(v, (int, float, str, bool, type(None)))
                                      else getattr(v, "__name__", type(v).__name__))
                                  for k, v in (bind_report or {}).items()},
             "resume_report": resume_report, "last_batch_stats": last_stats,
             "gate_history": gate_history,
             "curriculum": {"enabled": bool(cur["enabled"]),
                            "stages": cur["stages"],
                            "note": ("off: file order, legacy-uniform"
                                     if not cur["enabled"] else
                                     "on: ascending arm-spread, per-epoch band rotation"),
                            "schedule": curriculum_reports},
             "test_seam": ({"enabled": True, "file_sha256": test_sha,
                            "note": "TEST-ONLY fixed completions; not reachable from "
                                    "launch_rl.py's command line"}
                           if test_comp else {"enabled": False}),
             "elapsed_s": round(time.time() - t_start, 1)}
    if _is_main(acc):
        os.makedirs(out_root, exist_ok=True)
        p = os.path.join(out_root, "train_report.json")
        json.dump(final, open(p, "w"), indent=1, sort_keys=True, default=str)
        print(f"[report] {p}")
    print(json.dumps({"stage": "train-finished", "steps_done": step,
                      "optimizer_steps": opt_steps, "skipped": n_skipped,
                      "elapsed_s": final["elapsed_s"]}))
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--config", default=os.path.join(HERE, "configs", "rl_v1.json"))
    ap.add_argument("--init-from", default="")
    ap.add_argument("--out-root", default="")
    ap.add_argument("--ref-cache", default="")
    ap.add_argument("--mode", default="fresh", choices=["fresh", "resume"])
    # Driver-only switches. launch_rl.py passes NONE of these, so the production
    # command line it builds is unchanged; they exist so the smoke test can exercise
    # the real --train path on CPU.
    ap.add_argument("--device", default="auto", choices=["auto", "cpu", "cuda"],
                    help="compute device; 'auto' prefers cuda")
    ap.add_argument("--max-steps", type=int, default=0,
                    help="override optim.max_steps (smoke testing)")
    ap.add_argument("--test-completions", default="",
                    help="TEST-ONLY: fixed completions per prompt_sha256, "
                         "refused unless --device cpu")
    ap.add_argument("--check-wall-leaks", action="store_true",
                    help="run the forward-wall leak self-test and exit")
    ap.add_argument("--emit-mem-records", action="store_true",
                    help="SELF-FEEDING VETO: emit MEMORIZATION_RECORDS.jsonl from the "
                         "candidate checkpoint under selection, immediately before its "
                         "memorization verdict (equivalent to "
                         "selection.emit_memorization_records in the config)")
    m = ap.add_mutually_exclusive_group()
    m.add_argument("--selfcheck", action="store_true")
    m.add_argument("--plan", action="store_true")
    m.add_argument("--train", action="store_true")
    a = ap.parse_args(argv)

    if a.selfcheck:
        return _selfcheck()
    if a.check_wall_leaks:
        return test_wall_leaks(out=True)
    cfg = json.load(open(a.config, encoding="utf-8")) if os.path.isfile(a.config) else {}
    train_path = (cfg.get("data") or {}).get("train", "")
    ref_path = a.ref_cache or ((cfg.get("reference") or {}).get("action_dist_cache") or "")
    if a.plan:
        return _plan(a.config, train_path, ref_path)
    if a.train:
        if a.device == "cpu":
            # Make the process genuinely GPU-free. torch's default fully_shard mesh
            # is chosen from torch._C._get_accelerator(), which is 'cpu' only when no
            # CUDA device is visible; without this, sharding would place the
            # DTensor shards on cuda:0. Must run before the first CUDA call.
            os.environ["CUDA_VISIBLE_DEVICES"] = ""
        return run_train(a)
    ap.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())





