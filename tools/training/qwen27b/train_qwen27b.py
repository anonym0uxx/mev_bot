#!/usr/bin/env python3
"""Full-parameter BF16 training of Qwen3.8-27B — CPT (Phase A) and SFT (Phase B).

Launch ONLY via accelerate + DeepSpeed ZeRO-3 (see LAUNCH.md). Do not run directly.

VALIDATION-GOVERNED schedule (operator directive 2026-08-31, supersedes fixed epochs):
  CPT : MAXIMUM WINDOW 1.75 epochs, peak LR 4e-6, cosine, warmup 4%, wd 0.10,
        seq 4096 EOS-packed. Eval+save at ~0.25-epoch marks
        (0.25/0.5/0.75/1.0/1.25/1.5/1.75).
  SFT : MAXIMUM WINDOW 1.25 epochs, peak LR 2e-6, cosine, warmup 3%, wd 0.01,
        seq<=12288 UNPACKED, dynamic padding to batch max, prompts masked -100.
        Eval+save at ~0.25-epoch marks (0.25/0.5/0.75/1.0/1.25).
  These are MAX WINDOWS, not targets. The SELECTED checkpoint is the one with the
  strongest internal-validation behavior — NOT automatically the last one:
    - load_best_model_at_end + metric eval_loss → final/ = best-val weights.
    - EarlyStopping(patience=2): if val worsens across 2 consecutive marks while
      training continues, training STOPS and the earlier/best checkpoint is kept
      (overfitting rule). Training loss alone NEVER governs continuation.
    - If val is still materially improving at the max boundary: STOP AND ASK the
      operator before extending. --extend_epochs refuses values beyond the window.
  Full eval history is written to <out_dir>/val_history.jsonl for selection review
  (val token loss + perplexity + train-loss context at each mark). Human review
  still covers: SFT schema/output validity, repetition/degeneration, grad/loss
  stability (TensorBoard grad-norm + loss curves).
  SFT initializes from the SELECTED CPT weights with a FRESH optimizer/scheduler
  (structural guarantee: separate process; --init_from loads model weights only).
  Frozen qwen_eval_v1.1 untouched until post-training.

INTERNAL VALIDATION: ~2.5% held out from training-eligible material,
GROUP-DISJOINT (mint / trajectory / repair-chain / source-doc), evaluated at
checkpoint saves only. This is NOT qwen_eval_v1.1.

Attention: PyTorch SDPA (cuDNN/flash backend as available on sm_120).
FA2 deliberately NOT required.
"""
import argparse, hashlib, json, math, os, random, resource

import torch
from torch.utils.data import Dataset
from transformers import (AutoModelForCausalLM, AutoTokenizer, Trainer,
                          TrainingArguments, set_seed,
                          EarlyStoppingCallback, TrainerCallback)

TOKENIZER_REPO = "unsloth/Qwen3.8-27B"
TOKENIZER_REV = "3ea932cee0a432ae86e9c7826cbe8aef52323a28"
CPT_SEQ_LEN = 4096      # max CPT record = 3,999 tok — one bin fits any record
SFT_SEQ_LEN = 12288     # max SFT record = 10,154 tok — headroom, zero truncation
SEED = 42
VAL_FRACTION = 0.025    # internal validation, group-disjoint
NUM_GPUS = 3
# Retention target share of SUPERVISED LOSS-BEARING LABEL TOKENS (directive:
# 3-5% initially). Raw token share is wrong: domain prompts are masked -100, so
# optimization share = share of labels != -100 after chat templating/masking.
RETENTION_TARGET = 0.04

# ── SUPERVISION GEOMETRY (task-level gradient budget, BINDING) ──
# Target OPTIMIZATION shares measured in post-mask LOSS-BEARING label tokens.
# Enforced by bounded per-task LOSS WEIGHTS (no repetition/padding — quality
# over scale). Weight = target_share / measured_share, clipped to
# [1/TASK_WEIGHT_CAP, TASK_WEIGHT_CAP], then renormalized so the weighted
# token count equals the raw token count (keeps the effective LR unchanged).
# CRITICAL: weights derive ONLY from task identity — NEVER from realized
# return, MFE, net SOL, robust_utility magnitude, or moonshot status.
TASK_TARGETS = {
    "cross":     0.70,   # cross-sectional Pump decisions/ranking (+ exec surface)
    "post":      0.10,   # Pump postmortem/contrast
    "rust":      0.10,   # Rust/Solana engineering
    "narrative": 0.05,   # narrative/meta strategy
    "hermes":    0.01,   # Hermes/tool behavior
    "retention": 0.04,   # general retention
}
TASK_WEIGHT_CAP = 4.0

_BUCKET_BY_PREFIX = (
    ("sft_cross", "cross"), ("sft_exec", "cross"),
    ("sft_post", "post"),
    ("sft_rust", "rust"),
    ("sft_narr", "narrative"),
    ("sft_hermes", "hermes"), ("sft_gen", "hermes"),
)


def task_bucket(rec):
    """Map a record to its supervision-geometry bucket. Retention records use
    the messages schema (no curriculum id) -> 'retention'."""
    rid = str(rec.get("id", ""))
    for pfx, bucket in _BUCKET_BY_PREFIX:
        if rid.startswith(pfx):
            return bucket
    return "retention"


def compute_task_weights(recs_with_loss):
    """(bucket, loss_tokens) pairs -> {bucket: weight}.
    Bounded ratio weights, renormalized to preserve total token mass."""
    per = {}
    for bucket, lt in recs_with_loss:
        per[bucket] = per.get(bucket, 0) + lt
    total = sum(per.values())
    w = {}
    for bucket, lt in per.items():
        share = lt / total
        target = TASK_TARGETS.get(bucket, share)
        w[bucket] = min(max(target / share, 1.0 / TASK_WEIGHT_CAP),
                        TASK_WEIGHT_CAP)
    # Renormalize: sum(w_b * tokens_b) == total  (unchanged effective LR)
    scale = total / sum(w[b] * per[b] for b in per)
    w = {b: w[b] * scale for b in w}
    eff = {b: round(w[b] * per[b] / total, 4) for b in per}
    return w, per, eff


def load_jsonl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]


def group_key(rec):
    """Group-disjoint validation key: mint / trajectory / repair chain / source doc.
    Falls back to content hash (its own group) when no grouping field exists."""
    prov = rec.get("provenance") or {}
    meta = rec.get("meta") or {}
    for src in (rec, prov, meta):
        for k in ("mint", "mint_address", "trajectory_id", "chain_id",
                  "repair_chain_id", "group_id", "source_doc", "source_url",
                  "doc_id", "source_id"):
            v = src.get(k)
            if v:
                return f"{k}:{v}"
    sids = rec.get("source_ids")
    if sids:
        return "sids:" + ",".join(sorted(str(s) for s in sids))
    basis = rec.get("content") or rec.get("output") or rec
    if not isinstance(basis, str):
        basis = json.dumps(basis, sort_keys=True, ensure_ascii=False, default=str)
    return "hash:" + hashlib.sha256(basis.encode()).hexdigest()[:16]


def split_train_val(recs):
    """Deterministic group-disjoint split: hash group key -> ~VAL_FRACTION to val."""
    train, val = [], []
    for r in recs:
        h = int(hashlib.sha256(group_key(r).encode()).hexdigest()[:8], 16) / 0xFFFFFFFF
        (val if h < VAL_FRACTION else train).append(r)
    return train, val


class PackedCPTDataset(Dataset):
    """Greedy sequential packing of CPT documents into CPT_SEQ_LEN blocks with EOS
    separators. 4096 bins: no forced fusion of many unrelated records per bin."""

    def __init__(self, docs, tok, shuffle=True):
        if shuffle:
            docs = list(docs)
            random.Random(SEED).shuffle(docs)
        eos = tok.eos_token_id
        buf, self.blocks = [], []
        for d in docs:
            ids = tok(d["content"], add_special_tokens=False)["input_ids"]
            buf.extend(ids + [eos])
            while len(buf) >= CPT_SEQ_LEN:
                self.blocks.append(buf[:CPT_SEQ_LEN])
                buf = buf[CPT_SEQ_LEN:]
        if len(buf) > CPT_SEQ_LEN // 8:
            self.blocks.append(buf)

    def __len__(self):
        return len(self.blocks)

    def __getitem__(self, i):
        ids = torch.tensor(self.blocks[i], dtype=torch.long)
        return {"input_ids": ids, "labels": ids.clone(),
                "attention_mask": torch.ones_like(ids)}


def build_example(r, tok):
    """One record -> (full_ids, labels) with prompt masking, or None if over-length.
    Canonical serialization MUST match certify_curriculum_v1.py:
    non-string input/output -> json.dumps(..., ensure_ascii=False)."""
    if "messages" in r:
        msgs = r["messages"]
    else:
        def _s(v):
            return v if isinstance(v, str) else json.dumps(v, ensure_ascii=False)
        user = _s(r["instruction"]) + (
            "\n\n" + _s(r["input"]) if r.get("input") else "")
        msgs = [{"role": "user", "content": user},
                {"role": "assistant", "content": _s(r["output"])}]
    # transformers >=5.x: apply_chat_template(tokenize=True) returns a BatchEncoding,
    # not a raw list[int] (4.x). Extract ["input_ids"] (flat list of token ids).
    prompt_ids = tok.apply_chat_template(msgs[:-1], add_generation_prompt=True,
                                         tokenize=True)["input_ids"]
    full_ids = tok.apply_chat_template(msgs, add_generation_prompt=False,
                                       tokenize=True)["input_ids"]
    if len(full_ids) > SFT_SEQ_LEN:
        return None  # cert max = 10,154 < 12,288 -> expect never
    labels = [-100] * len(prompt_ids) + full_ids[len(prompt_ids):]
    return full_ids, labels


def loss_tokens(labels):
    return sum(1 for x in labels if x != -100)


def select_retention(domain, retention, tok, target=RETENTION_TARGET):
    """Deterministically subsample the retention shard so retention contributes
    ~target of SUPERVISED LOSS-BEARING LABEL TOKENS (post-templating/masking).
    Full shard stays preserved on disk/modular — this only governs what is FED.
    Order: sha256 of the record's canonical JSON (stable, data-derived, seedless).
    Returns (kept_records, stats)."""
    dom_loss = 0
    for r in domain:
        ex = build_example(r, tok)
        if ex:
            dom_loss += loss_tokens(ex[1])
    per = []
    for r in retention:
        ex = build_example(r, tok)
        if not ex:
            continue
        key = hashlib.sha256(json.dumps(
            r, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
        per.append((key, r, loss_tokens(ex[1])))
    per.sort(key=lambda t: t[0])
    budget = dom_loss * target / (1.0 - target)   # R/(D+R) = target
    keep, acc = [], 0
    for key, r, lt in per:
        if acc >= budget:
            break
        keep.append(r)
        acc += lt
    stats = {"domain_loss_tokens": dom_loss,
             "retention_full_records": len(retention),
             "retention_kept_records": len(keep),
             "retention_kept_loss_tokens": acc,
             "retention_share_of_loss_tokens": acc / (dom_loss + acc)}
    return keep, stats


class SFTDataset(Dataset):
    """Chat-template SFT with prompt-token masking. UNPACKED; dynamic padding is
    done by the collator (batch max), never to a fixed 12K. Handles curriculum
    (instruction/input/output) and retention (messages) schemas.
    task_weights: optional {bucket: weight} — attaches a per-example loss weight
    (supervision geometry). None (eval / CPT) -> weight 1.0 for every example."""

    def __init__(self, recs, tok, shuffle=True, task_weights=None):
        if shuffle:
            recs = list(recs)
            random.Random(SEED).shuffle(recs)
        self.examples, skipped = [], 0
        for r in recs:
            ex = build_example(r, tok)
            if ex is None:
                skipped += 1
                continue
            w = 1.0
            if task_weights is not None:
                w = float(task_weights.get(task_bucket(r), 1.0))
            self.examples.append((ex[0], ex[1], w))
        if skipped:
            print(f"[WARN] skipped {skipped} over-length records (expected 0)")

    def __len__(self):
        return len(self.examples)

    def __getitem__(self, i):
        ids, labels, w = self.examples[i]
        return {"input_ids": torch.tensor(ids), "labels": torch.tensor(labels),
                "attention_mask": torch.ones(len(ids), dtype=torch.long),
                "task_weight": torch.tensor(w, dtype=torch.float32)}


def collate(batch, pad_id):
    maxlen = max(len(b["input_ids"]) for b in batch)   # dynamic: batch max only
    out = {"input_ids": [], "labels": [], "attention_mask": []}
    weights = [b.get("task_weight") for b in batch]
    for b in batch:
        pad = maxlen - len(b["input_ids"])
        out["input_ids"].append(torch.cat([b["input_ids"],
                                torch.full((pad,), pad_id, dtype=torch.long)]))
        out["labels"].append(torch.cat([b["labels"],
                             torch.full((pad,), -100, dtype=torch.long)]))
        out["attention_mask"].append(torch.cat([b["attention_mask"],
                                     torch.zeros(pad, dtype=torch.long)]))
    res = {k: torch.stack(v) for k, v in out.items()}
    if all(w is not None for w in weights):
        res["task_weight"] = torch.stack(weights)
    return res


class WeightedLossTrainer(Trainer):
    """TRUE weighted-token-mean CE with per-sequence task weights.

        loss = Σ(w_task · CE_token) / Σ(w_task per valid label token)

    Numerator and denominator cover the SAME global gradient-accumulation
    window and ranks: `_get_num_items_in_batch` is overridden to return the
    WEIGHTED valid-label-token count Σ_seq(w_seq · valid_seq) summed over all
    grad-accum micro-batches and gathered across devices — exactly mirroring
    the stock unweighted count path on transformers 4.57.1 (which this class
    otherwise inherits: training_step scales by world_size under
    average_tokens_across_devices and skips grad-accum division when
    num_items_in_batch is set; that logic is reused unchanged, only the
    count semantics change). Padding and -100 tokens contribute to NEITHER
    side (shift convention labels[:,1:] on both). With all weights == 1.0
    this reduces EXACTLY to the stock token-normalized objective, so a
    uniform batch of weight-w examples sees the same loss scale as
    unweighted — task mix cannot modulate the effective LR.
    Weights never depend on outcomes/economics. Eval datasets are built
    without task_weights (w=1.0) → eval_loss is the plain unweighted mean.
    Falls back to the stock path when batches carry no task_weight (CPT)."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._denom_stats = []      # (weighted_denom, raw_valid) per window
        self._startup_denom_ok = False   # first-GA-window denominator assert
        self._startup_numer_ok = False   # first-forward numerator assert

    # Tokens per CE chunk. 1024 x V(248,320) x 4B = ~1.0 GiB per fp32 tensor;
    # ~3 live per chunk -> ~3 GiB transient instead of ~25 GiB unchunked at 9k tok.
    CE_CHUNK = 1024

    @staticmethod
    def _ce_chunk_fn(lg, lb):
        # lg: [B, C, V] bf16 (view of the model logits); lb: [B, C] int64.
        return torch.nn.functional.cross_entropy(
            lg.float().transpose(1, 2), lb, ignore_index=-100, reduction="none")

    def _chunked_ce(self, shift_logits, shift_labels):
        """Per-token CE [B, T-1] in fp32, computed CE_CHUNK positions at a time.
        Under grad, each chunk runs through torch.utils.checkpoint so the fp32
        upcast + log_softmax intermediates are NOT retained for backward (they
        are recomputed per chunk in backward). Same math as one big
        cross_entropy(reduction='none'); only the peak-memory shape changes."""
        T = shift_logits.shape[1]
        if T <= self.CE_CHUNK and not torch.is_grad_enabled():
            return self._ce_chunk_fn(shift_logits, shift_labels)
        from torch.utils.checkpoint import checkpoint
        parts = []
        for s in range(0, T, self.CE_CHUNK):
            lg = shift_logits[:, s:s + self.CE_CHUNK, :]
            lb = shift_labels[:, s:s + self.CE_CHUNK]
            if torch.is_grad_enabled() and lg.requires_grad:
                parts.append(checkpoint(self._ce_chunk_fn, lg, lb,
                                        use_reentrant=False))
            else:
                parts.append(self._ce_chunk_fn(lg, lb))
        return torch.cat(parts, dim=1)

    def _get_num_items_in_batch(self, batch_samples, device):
        if not (len(batch_samples) > 0 and "labels" in batch_samples[0]
                and "task_weight" in batch_samples[0]):
            return super()._get_num_items_in_batch(batch_samples, device)
        try:
            w_parts, r_parts = [], []
            for batch in batch_samples:
                valid = batch["labels"][:, 1:].ne(-100).sum(dim=1)
                w = batch["task_weight"].to(torch.float64)
                w_parts.append((w * valid.to(torch.float64)).sum())
                r_parts.append(valid.sum())
            weighted = torch.stack(w_parts).sum()
            raw = torch.stack(r_parts).sum()
        except (TypeError, AttributeError):
            return super()._get_num_items_in_batch(batch_samples, device)
        weighted = weighted.to(device)
        raw = raw.to(device)
        if self.args.average_tokens_across_devices and self.args.world_size >= 1:
            weighted = self.accelerator.gather(weighted).sum()
            raw = self.accelerator.gather(raw).sum()
        # STARTUP ASSERTION (real run, first grad-accum window): weighted
        # valid-token denominator must be finite and nonzero, raw valid
        # tokens nonzero (i.e. not everything -100/pad).
        if not self._startup_denom_ok:
            wf, rf = float(weighted), float(raw)
            assert math.isfinite(wf) and wf > 0.0, (
                f"STARTUP ASSERT FAILED: weighted denominator {wf} not "
                f"finite/nonzero in first GA window. REFUSING to continue.")
            assert rf > 0.0, (
                "STARTUP ASSERT FAILED: zero valid (non--100) label tokens "
                "in first GA window. REFUSING to continue.")
            print(f"[startup-assert] first GA window: weighted_denom={wf:.2f} "
                  f"raw_valid_tokens={int(rf)} ratio={wf / rf:.4f} "
                  f"(finite/nonzero OK, gathered across "
                  f"{self.accelerator.num_processes} ranks)")
            self._startup_denom_ok = True
        # sampled denominator/scale audit: weighted vs raw global counts
        if len(self._denom_stats) < 4096:
            self._denom_stats.append((float(weighted), float(raw.float())))
        return weighted

    def compute_loss(self, model, inputs, return_outputs=False,
                     num_items_in_batch=None):
        if "task_weight" not in inputs:
            return super().compute_loss(model, inputs, return_outputs,
                                        num_items_in_batch=num_items_in_batch)
        weights = inputs.pop("task_weight")
        labels = inputs.pop("labels")
        outputs = model(**inputs)
        # shift: predict token t+1 from t  (same convention as denominator)
        shift_labels = labels[:, 1:]
        # MEMORY-BOUNDED CE (SFT OOM root cause, 2026-09-03 step-2 crash):
        # `outputs.logits.float()` + one unchunked cross_entropy materializes
        # ~4 full-vocab (V=248,320) fp32 tensors -> ~28-30 GiB at the p95-max
        # SFT lengths (8.6k-9.1k tok) vs ~13 GiB at CPT's fixed 4096, which
        # pushed a rank past this host's ~57-59GB WSL2/dxg per-process commit
        # cap ("OOM with 59GB free"). expandable_segments is NOT available on
        # WSL2 dxg (CUDA driver error), so the fix is structural: upcast+CE in
        # CE_CHUNK-token chunks under activation checkpointing, so only ONE
        # chunk's fp32 intermediates are live at a time. Per-token CE is
        # position-independent -> numerically the same sum (fp32 order only).
        per_tok = self._chunked_ce(outputs.logits[:, :-1, :], shift_labels)  # [B, T-1]
        per_seq = per_tok.sum(dim=1)                       # CE sum per sequence
        weighted = (per_seq * weights.to(per_seq.dtype)).sum()
        # STARTUP ASSERTION (real run, first forward): numerator finite;
        # -100/pad positions contribute EXACTLY zero to the CE sum.
        if not self._startup_numer_ok and model.training:
            ignored = shift_labels.eq(-100)
            leak = float(per_tok.masked_select(ignored).abs().sum()) \
                if bool(ignored.any()) else 0.0
            assert leak == 0.0, (
                f"STARTUP ASSERT FAILED: -100/pad positions contributed "
                f"{leak} to the CE numerator. REFUSING to continue.")
            assert bool(torch.isfinite(weighted)), (
                "STARTUP ASSERT FAILED: weighted CE numerator is not finite "
                "in first forward. REFUSING to continue.")
            print(f"[startup-assert] first forward: weighted CE numerator "
                  f"finite={bool(torch.isfinite(weighted))}, "
                  f"-100/pad contribution=0.0 OK, "
                  f"batch task_weights={[round(float(x), 4) for x in weights]}")
            self._startup_numer_ok = True
        if num_items_in_batch is not None:
            denom = num_items_in_batch.to(weighted.device, weighted.dtype) \
                if torch.is_tensor(num_items_in_batch) \
                else torch.tensor(float(num_items_in_batch),
                                  device=weighted.device)
            loss = weighted / denom
            # GLOBAL-DENOMINATOR / DDP-AVERAGING COMPENSATION — mirrors the
            # scaling stock compute_loss applies at its tail (which this
            # override replaces): with a global all-rank denominator, DDP
            # gradient averaging divides by world_size, so scale back up.
            # Same condition as stock: average_tokens_across_devices AND a
            # provided num_items_in_batch. No-op at world_size == 1.
            if self.args.average_tokens_across_devices:
                loss *= (self.accelerator.num_processes
                         if self.args.n_gpu <= 1 else self.args.n_gpu)
        else:
            # eval / single-batch path: WEIGHTED local denominator (same
            # formula, window = this batch). Eval datasets carry w=1.0, so
            # eval_loss is the plain unweighted token mean.
            valid = shift_labels.ne(-100).sum(dim=1)
            wdenom = (weights.to(per_seq.dtype) * valid.to(per_seq.dtype)) \
                .sum().clamp(min=1e-9)
            loss = weighted / wdenom
        return (loss, outputs) if return_outputs else loss

    def denom_stats_summary(self):
        if not self._denom_stats:
            return {}
        import statistics as st
        ratios = [w / r for w, r in self._denom_stats if r > 0]
        return {"windows_sampled": len(ratios),
                "weighted_over_raw_ratio": {
                    "min": round(min(ratios), 4),
                    "p50": round(st.median(ratios), 4),
                    "max": round(max(ratios), 4)}}


class CudaCacheFlushCallback(TrainerCallback):
    """WSL2/dxg stability: return cached blocks to the driver at every
    eval<->train transition so large fresh allocations (18GB contiguous
    grad buffer at first backward) don't fail on a heap fragmented by
    eval-shaped blocks. Root cause of the 2026-08-31 15:03 OOM: eval ran
    18/18 fine, then first backward OOMed on 228MB with 34.7GB 'free'."""
    def on_evaluate(self, args, state, control, **kw):
        import gc; gc.collect(); torch.cuda.empty_cache()

    def on_train_begin(self, args, state, control, **kw):
        import gc; gc.collect(); torch.cuda.empty_cache()


class FirstWindowLossFiniteCallback(TrainerCallback):
    """Real-run startup assertion for BOTH phases: the FIRST logged training
    loss (first grad-accum window, after cross-rank token normalization)
    must be finite and nonzero. Catches NaN/inf/zero-denominator failures
    at step 1 instead of silently poisoning the run."""
    def __init__(self):
        self._checked = False

    def on_log(self, args, state, control, logs=None, **kw):
        if self._checked or not logs or "loss" not in logs:
            return
        loss = logs["loss"]
        assert math.isfinite(loss) and loss > 0.0, (
            f"STARTUP ASSERT FAILED: first logged training loss {loss} is "
            f"not finite/nonzero. REFUSING to continue.")
        print(f"[startup-assert] first GA-window loss={loss:.6f} "
              f"finite/nonzero OK (step {state.global_step})")
        self._checked = True


class BoundaryEvalCallback(TrainerCallback):
    """Guarantee an explicit terminal evaluate()+save exactly AT the max-window
    boundary step (e.g. SFT 209, not divisible by the 41-step marks), so boundary
    governance sees the actual 1.25-epoch state before best-model selection."""

    def __init__(self, boundary_step):
        self.boundary_step = boundary_step

    def on_step_end(self, args, state, control, **kw):
        if state.global_step == self.boundary_step:
            control.should_evaluate = True
            control.should_save = True
        return control


class ValHistoryCallback(TrainerCallback):
    """Append every internal-validation result to <out_dir>/val_history.jsonl:
    step, epoch, val loss, val perplexity, latest train loss — the record used
    for checkpoint SELECTION (never frozen qwen_eval_v1.1)."""

    def __init__(self, out_dir):
        self.path = os.path.join(out_dir, "val_history.jsonl")
        self.last_train_loss = None

    def on_log(self, args, state, control, logs=None, **kw):
        if logs and "loss" in logs:
            self.last_train_loss = logs["loss"]

    def on_evaluate(self, args, state, control, metrics=None, **kw):
        if not metrics or "eval_loss" not in metrics or not state.is_world_process_zero:
            return
        rec = {"step": state.global_step, "epoch": round(state.epoch or 0, 4),
               "val_loss": metrics["eval_loss"],
               "val_ppl": math.exp(min(metrics["eval_loss"], 50)),
               "train_loss_latest": self.last_train_loss,
               "best_step_so_far": state.best_global_step
               if hasattr(state, "best_global_step") else None,
               "best_val_loss_so_far": state.best_metric}
        with open(self.path, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec) + "\n")
        print(f"[val] step {rec['step']} epoch {rec['epoch']}: "
              f"loss {rec['val_loss']:.4f} ppl {rec['val_ppl']:.3f} "
              f"(best so far: {state.best_metric})")


def main():
    # Belt-and-suspenders: raise RLIMIT_NOFILE to the hard cap IN-PROCESS before
    # DeepSpeed's NVMe aio opens any swap file. The shell `ulimit -n 1048576`
    # DOES reach the ranks (verified soft=1048576), but DeepSpeed's aio leaks one
    # fd per completed op (deepspeed_py_io_handle.cpp:213 is missing its close()),
    # exhausting even 1048576 in ~103 steps (attempt #19 crash, error 24). This
    # setrlimit is a no-op today but guarantees the full cap survives any future
    # launch-mechanism change. The REAL fix is re-adding close() — deferred to
    # the native-Linux migration (where offload_param is dropped entirely).
    _soft, _hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, (_hard, _hard))
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", choices=["cpt", "sft"], required=True)
    ap.add_argument("--data_root",
                    default="/mnt/d/repos/mev_bot/tools/data-pipeline/output/qwen_curriculum_v1")
    ap.add_argument("--out_root", default="/training/runs")
    ap.add_argument("--init_from", default=None,
                    help="SFT: path to Phase-A consolidated BF16 final weights. "
                         "Loads MODEL WEIGHTS ONLY — optimizer/scheduler start fresh.")
    ap.add_argument("--extend_epochs", type=float, default=None,
                    help="Override training window WITHIN the hard max only "
                         "(CPT 1.75 / SFT 1.25). Values beyond the max are REFUSED — "
                         "extending past the window requires a new operator directive.")
    ap.add_argument("--consolidate", action="store_true",
                    help="Load the final DeepSpeed checkpoint via the trainer's "
                         "correctly-configured engine and emit a consolidated BF16 "
                         "model (save_16bit_model). No training. Requires --phase cpt "
                         "and --consolidate_out.")
    ap.add_argument("--consolidate_out", default=None,
                    help="Output dir for --consolidate.")
    args = ap.parse_args()
    set_seed(SEED)

    tok = AutoTokenizer.from_pretrained(TOKENIZER_REPO, revision=TOKENIZER_REV)
    model_src = args.init_from or TOKENIZER_REPO
    model = AutoModelForCausalLM.from_pretrained(
        model_src, revision=None if args.init_from else TOKENIZER_REV,
        torch_dtype=torch.bfloat16, attn_implementation="sdpa")
    model.config.use_cache = False
    # gradient checkpointing enabled via TrainingArguments with
    # use_reentrant=False (non-reentrant: lower memory churn, plays
    # correctly with ZeRO-3 hooks; reentrant fallback is deprecated).

    if args.phase == "cpt":
        docs = load_jsonl(f"{args.data_root}/cpt/qwen_cpt_v1.jsonl")
        tr, va = split_train_val(docs)
        train_ds = PackedCPTDataset(tr, tok)
        val_ds = PackedCPTDataset(va, tok, shuffle=False)
        max_window = 1.75            # MAXIMUM WINDOW, not a target
        lr, warmup, wd = 4e-6, 0.04, 0.1
    else:
        domain = load_jsonl(f"{args.data_root}/sft/qwen_sft_v2.jsonl")
        retention_full = load_jsonl(
            f"{args.data_root}/retention/qwen_retention_v1.jsonl")
        # Retention weighting by LOSS-BEARING LABEL TOKENS (not raw tokens, not
        # record count): deterministic subsample to ~RETENTION_TARGET of the
        # supervised optimization signal. Full shard preserved on disk.
        retention, rstats = select_retention(domain, retention_full, tok)
        print(f"[retention] {json.dumps(rstats)}")
        recs = domain + retention
        tr, va = split_train_val(recs)
        # SUPERVISION GEOMETRY: bounded task-level loss weights from measured
        # post-mask loss-token shares of the REAL train split (never outcomes).
        pairs = []
        for r in tr:
            ex = build_example(r, tok)
            if ex:
                pairs.append((task_bucket(r), loss_tokens(ex[1])))
        task_w, per_bucket, eff_shares = compute_task_weights(pairs)
        print(f"[geometry] loss_tokens_by_bucket={per_bucket}")
        print(f"[geometry] task_weights={ {b: round(w, 4) for b, w in task_w.items()} }")
        print(f"[geometry] effective_gradient_shares={eff_shares}")
        train_ds = SFTDataset(tr, tok, task_weights=task_w)
        # Eval stays UNWEIGHTED: eval_loss remains a pure token-normalized CE
        # (checkpoint selection must not chase the reweighted objective).
        val_ds = SFTDataset(va, tok, shuffle=False)
        max_window = 1.25            # MAXIMUM WINDOW, not a target
        lr, warmup, wd = 2e-6, 0.03, 0.01

    epochs = args.extend_epochs or max_window
    assert epochs <= max_window, (
        f"REFUSED: {args.phase} window is capped at {max_window} epochs. "
        f"Extending beyond it requires a new operator directive (STOP AND ASK).")

    per_dev, accum = 1, 8
    global_batch = per_dev * accum * NUM_GPUS          # 24
    # EXACT step math from the REAL post-holdout dataset (not token approximations):
    steps_per_epoch = math.ceil(len(train_ds) / global_batch)
    total_steps = math.ceil(steps_per_epoch * epochs)
    # floor, not round: guarantees the final ~quarter-epoch mark (incl. the window
    # boundary eval) lands INSIDE total_steps (round(167/4)=42 -> 210 > 209 would
    # silently drop the 1.25-epoch SFT eval).
    mark = max(1, math.floor(steps_per_epoch / 4))     # eval+ckpt every ~0.25 epoch
    marks = [s for s in range(mark, total_steps + 1, mark)]
    if total_steps not in marks:
        marks.append(total_steps)   # BoundaryEvalCallback forces this terminal mark
    print(f"[plan] phase={args.phase} train={len(train_ds)} val={len(val_ds)} "
          f"steps/epoch={steps_per_epoch} max_window={epochs}ep "
          f"total_steps(max)={total_steps} warmup_steps={math.ceil(total_steps*warmup)} "
          f"eval+ckpt every {mark} steps -> marks(epochs): "
          f"{[round(s/steps_per_epoch, 3) for s in marks]}")

    out_dir = f"{args.out_root}/qwen27b_{args.phase}_v1"
    os.makedirs(out_dir, exist_ok=True)
    targs = TrainingArguments(
        output_dir=out_dir,
        num_train_epochs=epochs,
        per_device_train_batch_size=per_dev,
        per_device_eval_batch_size=1,
        gradient_accumulation_steps=accum,
        learning_rate=lr,
        lr_scheduler_type="cosine",
        # transformers 5.x removed warmup_ratio; warmup_steps = ceil(total*ratio)
        # matches 4.x get_warmup_steps() exactly — identical schedule.
        warmup_steps=math.ceil(total_steps * warmup),
        weight_decay=wd,
        adam_beta1=0.9, adam_beta2=0.95, adam_epsilon=1e-8,
        max_grad_norm=1.0,
        bf16=True,
        logging_steps=1,
        eval_on_start=(args.phase == "cpt"),  # SFT: skip to save dxg RAM for backward
        eval_strategy="steps",
        eval_steps=mark,
        save_strategy="steps",
        save_steps=mark,
        # 2 = best-val ckpt (rotation-protected by load_best_model_at_end) + most
        # recent resumable. limit=3 risks transient volume exhaustion: ~380GB x3
        # retained + ~380GB being written + ~324GB live NVMe optimizer state
        # > 1.8TB. Rotated-out marks survive in val_history.jsonl; consolidate a
        # specific valuable mark to BF16 (~54GB) separately when needed.
        save_total_limit=2,
        load_best_model_at_end=True,           # SELECTED ckpt = best val, not last
        metric_for_best_model="eval_loss",
        greater_is_better=False,
        # Token-normalized loss across variable-length sequences AND devices:
        # gradient accumulation must divide by total valid (non--100) prediction
        # tokens in the GLOBAL batch, not average per-sequence/per-device.
        average_tokens_across_devices=True,
        report_to=["tensorboard"],
        # transformers 5.x removed logging_dir; TB logs land under output_dir/runs.
        gradient_checkpointing=True,
        gradient_checkpointing_kwargs={"use_reentrant": False},
        dataloader_num_workers=2,
        # dxg error -75 killed the VM once from pinned-pool exhaustion;
        # batch tensors are tiny (4096 int64) — pinning buys nothing here.
        dataloader_pin_memory=False,
        seed=SEED,
    )
    trainer_cls = WeightedLossTrainer if args.phase == "sft" else Trainer
    trainer = trainer_cls(model=model, args=targs,
                      train_dataset=train_ds, eval_dataset=val_ds,
                      data_collator=lambda b: collate(b, tok.pad_token_id),
                      callbacks=[
                          # Overfitting rule: val worsening across 2 consecutive
                          # 0.25-epoch marks while train loss falls -> STOP, keep
                          # best earlier checkpoint. Train loss never governs.
                          EarlyStoppingCallback(early_stopping_patience=2),
                          ValHistoryCallback(out_dir),
                          # Explicit terminal evaluate()+save AT the boundary step
                          # (SFT 209 is not on the 41-step grid) so governance and
                          # best-model selection see the true window-boundary state.
                          BoundaryEvalCallback(total_steps),
                          FirstWindowLossFiniteCallback(),
                          CudaCacheFlushCallback(),
                      ])
    # VERIFY token-normalized accumulation with THIS transformers version
    # (4.57.1 verified at source): Trainer._get_num_items_in_batch computes
    # sum(labels.ne(-100)) over ALL grad-accum micro-batches, gathers it across
    # devices when average_tokens_across_devices=True, and passes it to the model
    # as num_items_in_batch so the summed CE loss is divided by GLOBAL valid
    # prediction tokens — not equal-weight per sequence/device. That path is
    # gated on trainer.model_accepts_loss_kwargs, so assert the REAL gate:
    assert trainer.model_accepts_loss_kwargs, (
        "Token-normalized loss NOT active: trainer.model_accepts_loss_kwargs is "
        "False (model.forward lacks **kwargs/loss_kwargs passthrough), so "
        "num_items_in_batch would not be computed and gradient accumulation "
        "would equal-weight sequences. REFUSING to train.")
    assert targs.average_tokens_across_devices, (
        "average_tokens_across_devices must be True: without the cross-device "
        "gather, each rank normalizes by its LOCAL token count only.")
    print(f"[loss-norm] verified: model_accepts_loss_kwargs="
          f"{trainer.model_accepts_loss_kwargs}, "
          f"average_tokens_across_devices={targs.average_tokens_across_devices} "
          f"-> loss normalized by global valid (non--100) label tokens")
    # STARTUP ASSERTIONS (real run, before first optimizer step) — operator
    # GO-closeout requirements. Not a disposable smoke test.
    ws = trainer.args.world_size
    assert ws == NUM_GPUS, (
        f"STARTUP ASSERT FAILED: world_size={ws}, expected {NUM_GPUS}. "
        f"Launch with all {NUM_GPUS} GPUs (accelerate/torchrun nproc). "
        f"REFUSING to train.")
    hf_ds_cfg = getattr(trainer.accelerator.state, "deepspeed_plugin", None)
    ds_stage = None
    if hf_ds_cfg is not None:
        try:
            ds_stage = hf_ds_cfg.zero_stage
        except AttributeError:
            ds_stage = (hf_ds_cfg.deepspeed_config or {}) \
                .get("zero_optimization", {}).get("stage")
    assert ds_stage == 3, (
        f"STARTUP ASSERT FAILED: DeepSpeed ZeRO stage={ds_stage}, expected 3 "
        f"(deepspeed_plugin={'present' if hf_ds_cfg else 'MISSING'}). "
        f"REFUSING to train.")
    if args.phase == "sft":
        expected_buckets = {"cross", "post", "rust", "narrative", "hermes",
                            "retention"}
        assert isinstance(trainer, WeightedLossTrainer), (
            "STARTUP ASSERT FAILED: SFT phase but trainer is not "
            "WeightedLossTrainer. REFUSING to train.")
        loaded = set(task_w.keys())
        assert loaded == expected_buckets, (
            f"STARTUP ASSERT FAILED: task-weight mapping {sorted(loaded)} != "
            f"expected {sorted(expected_buckets)}. REFUSING to train.")
        assert all(0.0 < w <= 4.0 * 1.05 for w in task_w.values()), (
            f"STARTUP ASSERT FAILED: task weights outside (0, 4x-cap "
            f"(+token-mass renorm tolerance)]: {task_w}. REFUSING to train.")
    print(f"[startup-assert] world_size={ws} OK; ZeRO stage={ds_stage} OK; "
          f"average_tokens_across_devices={targs.average_tokens_across_devices} OK; "
          f"task_weight_mapping="
          f"{'n/a (CPT)' if args.phase == 'cpt' else 'loaded+validated'} — "
          f"numerator/denominator finiteness asserted in-run at the first "
          f"GA window ([startup-assert] lines from WeightedLossTrainer).")
    ckpt = None
    if os.path.isdir(out_dir):
        cks = [d for d in os.listdir(out_dir) if d.startswith("checkpoint-")]
        if cks:
            ckpt = os.path.join(out_dir, max(cks, key=lambda x: int(x.split("-")[1])))
            print(f"[resume] {ckpt}")
    if args.consolidate:
        # offload_param: nvme checkpoints cannot be read by bare
        # deepspeed.initialize() (it skips _configure_checkpointing / NVMe-offload
        # setup, so load_checkpoint routes into the broken get_fp32_state_dict /
        # _get_zero_param_shapes paths). The trainer's accelerate+DeepSpeed engine
        # IS correctly configured (this is the exact path the self-heal resume used),
        # so load here and gather the BF16 weights with save_16bit_model.
        assert args.consolidate_out and args.phase == "cpt", \
            "--consolidate requires --phase cpt and --consolidate_out"
        # DeepSpeed init is deferred to train() in this transformers version.
        # _prepare_for_training runs the FULL deepspeed_init -> accelerator.prepare
        # -> self.deepspeed sequence the training run used, so load_checkpoint can
        # read the offload_param:nvme checkpoint (a bare deepspeed.initialize()
        # cannot — it skips the NVMe-offload wiring and routes into broken paths).
        trainer._prepare_for_training(total_steps, None, None)
        engine = getattr(trainer, "deepspeed", None)
        if engine is not None and hasattr(engine, "engine"):
            engine = engine.engine  # unwrap accelerate DeepSpeedEngineWrapper if present
        print(f"[consolidate] engine={type(engine).__name__} "
              f"has_load_checkpoint={hasattr(engine, 'load_checkpoint')}", flush=True)
        assert engine is not None and hasattr(engine, "load_checkpoint"), \
            "Deepspeed engine not available on trainer"
        engine.load_checkpoint(f"{out_dir}/final")
        # gather was deliberately disabled during training (avoid 54GB gather OOM
        # on busy GPUs); enable it now for offline consolidation (GPUs are empty).
        engine._config.zero_config.gather_16bit_weights_on_model_save = True
        engine.save_16bit_model(args.consolidate_out)
        tok.save_pretrained(args.consolidate_out)
        print("CONSOLIDATED", args.consolidate_out, flush=True)
        return
    trainer.train(resume_from_checkpoint=ckpt)
    st = trainer.state
    stopped_early = (st.global_step < total_steps)
    ds = getattr(trainer, "denom_stats_summary", lambda: {})()
    if ds:
        print(f"[denom-audit] {json.dumps(ds)}")
        with open(f"{out_dir}/denom_audit.json", "w") as f:
            json.dump(ds, f, indent=1)
    print(f"[select] best checkpoint: step {getattr(st, 'best_model_checkpoint', None)} "
          f"val_loss {st.best_metric}")
    if stopped_early:
        print("[select] early-stopped on validation worsening (overfitting rule); "
              "best earlier checkpoint retained as final/.")
    else:
        print(f"[BOUNDARY] reached the {epochs}-epoch MAXIMUM WINDOW. If "
              f"val_history.jsonl shows validation still materially improving at the "
              f"boundary with no degeneration, STOP AND ASK the operator before any "
              f"extension — do NOT extend automatically.")
    # final/ = BEST-validation weights (load_best_model_at_end), consolidated BF16
    trainer.save_model(f"{out_dir}/final")
    tok.save_pretrained(f"{out_dir}/final")
    print("DONE", args.phase, out_dir)


if __name__ == "__main__":
    main()
