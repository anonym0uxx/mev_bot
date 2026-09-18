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
import argparse, hashlib, json, math, os, random
try:
    import resource
except ImportError:  # Windows supports loader-only dryruns.
    resource = None

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
NUM_GPUS = 3
from release_data import (load_jsonl, task_bucket, build_example, loss_tokens,
                          derive_train_weights, weight_key, load_release, require,
                          resolve, sha256, tokenizer_fingerprint, verify_tokenizer, verify_training_request)


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
        if buf:
            self.blocks.append(buf)

    def __len__(self):
        return len(self.blocks)

    def __getitem__(self, i):
        ids = torch.tensor(self.blocks[i], dtype=torch.long)
        return {"input_ids": ids, "labels": ids.clone(),
                "attention_mask": torch.ones_like(ids)}


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
                w = float(task_weights[weight_key(r)])
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

    def prediction_step(self, model, inputs, prediction_loss_only, ignore_keys=None):
        # Gather per-example sufficient statistics while Accelerate still knows
        # the final-batch remainder; this removes distributed sampler duplicates.
        inputs=self._prepare_inputs(dict(inputs))
        labels=inputs.pop('labels')
        weights=inputs.pop('task_weight',None)
        require(weights is None or bool(weights.eq(1).all()), 'Validation weights must be unit weights')
        with torch.no_grad(), self.compute_loss_context_manager():
            outputs=model(**inputs)
            logits=outputs.logits if hasattr(outputs,'logits') else outputs['logits']
            per_token=self._chunked_ce(logits[:,:-1,:],labels[:,1:])
            totals=torch.stack((per_token.double().sum(1),labels[:,1:].ne(-100).double().sum(1)),dim=1)
        gathered=self.accelerator.gather_for_metrics(totals)
        self._eval_totals += gathered.sum(0).cpu()
        self._eval_examples += len(gathered)
        return totals[:,0].sum()/totals[:,1].sum().clamp(min=1),None,None

    def evaluation_loop(self, dataloader, description, prediction_loss_only=None,
                        ignore_keys=None, metric_key_prefix='eval'):
        self._eval_totals=torch.zeros(2,dtype=torch.float64)
        self._eval_examples=0
        output=super().evaluation_loop(dataloader,description,True,ignore_keys,metric_key_prefix)
        numerator,denominator=self._eval_totals.tolist()
        require(denominator>0 and math.isfinite(numerator), 'Invalid validation token totals')
        require(self._eval_examples==output.num_samples, 'Validation population mismatch')
        output.metrics[metric_key_prefix+'_loss']=numerator/denominator
        output.metrics[metric_key_prefix+'_loss_tokens']=int(denominator)
        output.metrics[metric_key_prefix+'_loss_sum']=numerator
        return output

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


def step_plan(phase, count, epochs=None):
    window = (1.75 if phase=='cpt' else 1.25) if epochs is None else epochs
    steps = math.ceil(count / (NUM_GPUS * 8))
    total = math.ceil(steps * window)
    mark = max(1, steps // 4)
    marks = list(range(mark,total+1,mark))
    if total not in marks: marks.append(total)
    return {'world_size':NUM_GPUS,'per_device_batch_size':1,'gradient_accumulation_steps':8,
            'steps_per_epoch':steps,'total_steps_max':total,'max_window_ep':window,
            'warmup_steps':math.ceil(total*(.04 if phase=='cpt' else .03)),
            'mark_every':mark,'marks_steps':marks,
            'marks_epochs':[round(s/steps,6) for s in marks]}


def prepare_datasets(release, phase, tok):
    verify_tokenizer(tok,release['manifest']['tokenizer'])
    parts=release['phases'][phase]
    if phase=='sft':
        weights,geometry=derive_train_weights(parts['train'],tok,release['manifest']['phases'][phase]['task_targets'])
        train=SFTDataset(parts['train'],tok,task_weights=weights)
        val=SFTDataset(parts['validation'],tok,shuffle=False)
    else:
        train=PackedCPTDataset(parts['train'],tok)
        val=PackedCPTDataset(parts['validation'],tok,shuffle=False)
        geometry={'method':'unweighted_factual_cpt_tokens; no task/class/outcome reweighting',
                  'tail_policy':'preserve_every_token','task_targets_applied':False}
    require(len(train)>0 and len(val)>0,'Empty resulting dataset')
    verify_tokenizer(tok,release['manifest']['tokenizer'])
    return train,val,geometry


def dataset_report(release,phase,train,val,geometry):
    def census(ds,partition):
        if phase=='sft':
            raw=sum(len(ids) for ids,labels,w in ds.examples)
            loss=sum(loss_tokens(labels) for ids,labels,w in ds.examples)
            weighted=sum(loss_tokens(labels)*w for ids,labels,w in ds.examples)
        else:
            raw=sum(map(len,ds.blocks));loss=sum(max(0,len(b)-1) for b in ds.blocks);weighted=loss
        return {'records':len(release['phases'][phase][partition]),'dataset_items':len(ds),
                'raw_tokens':raw,'shifted_loss_tokens':loss,'weighted_loss_token_mass':weighted,
                'tail_tokens':len(ds.blocks[-1]) if phase=='cpt' else None}
    return {'schema':'qwen27b_loader_dryrun_v2','release_id':release['manifest']['release_id'],
            'manifest_sha256':release['sha256'],'phase':phase,'training_authorized':False,
            'model_loaded':False,'training_launched':False,
            'tokenizer_effective_sha256':release['manifest']['tokenizer']['effective_sha256'],
            'train':census(train,'train'),'validation':census(val,'validation'),
            'geometry':geometry,'step_plan':step_plan(phase,len(train)),
            'validation_weights':'unit weights; no fitting or resplitting'}


def write_runtime_evidence(path, payload):
    """Best-effort runtime receipt. Never raises: collecting evidence must not be
    able to fail a training run."""
    try:
        tmp = str(path) + ".tmp"
        with open(tmp, "w") as fh:
            json.dump(payload, fh, indent=1, sort_keys=True)
        os.replace(tmp, path)
        print(f"[runtime-evidence] wrote {path}")
        return True
    except Exception as exc:                      # noqa: BLE001 - deliberate
        print(f"[runtime-evidence] NOT written ({type(exc).__name__}: {exc}); "
              "training is unaffected")
        return False


def collect_runtime_evidence(trainer, release, args, stopped_early, total_steps):
    """Assemble the receipt from state the trainer already tracks."""
    st = trainer.state
    eval_mass, eval_loss = None, None
    for entry in getattr(st, "log_history", []) or []:
        if "eval_loss_tokens" in entry:
            eval_mass = int(entry["eval_loss_tokens"])
            eval_loss = entry.get("eval_loss")
    try:
        zero_stage = trainer.accelerator.state.deepspeed_plugin.zero_stage
    except Exception:                             # noqa: BLE001
        zero_stage = None
    return {
        "schema": "qwen27b_runtime_evidence_v1",
        "phase": args.phase,
        "release_id": release.get("release_id"),
        "release_manifest": args.release_manifest,
        "world_size": int(getattr(trainer.args, "world_size", 0) or 0),
        "global_rank": int(getattr(trainer.args, "process_index", 0) or 0),
        "deepspeed_zero_stage": zero_stage,
        "native_deepspeed": zero_stage == 3,
        "steps_completed": int(st.global_step),
        "total_steps_planned": int(total_steps),
        "stopped_early": bool(stopped_early),
        "validation_loss_token_mass": eval_mass,
        "validation_loss": eval_loss,
        "denom_audit": (getattr(trainer, "denom_stats_summary", lambda: {})()),
        "note": ("Emitted by the training run itself. Reconcile validation_loss_token_mass "
                 "against the loader dry-run reference to obtain "
                 "weighted_loss_relative_error for the distributed_loss_runtime gate."),
    }


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", choices=["cpt", "sft"], required=True)
    ap.add_argument("--release_manifest", required=True)
    ap.add_argument("--dry_run", action="store_true")
    ap.add_argument("--report")
    ap.add_argument("--launch_contract")
    ap.add_argument("--contract_sha256")
    ap.add_argument("--resume_from_checkpoint")
    ap.add_argument("--preflight_only", action="store_true")
    ap.add_argument("--out_root", default="/training/runs")
    ap.add_argument("--init_from", default=None,
                    help="SFT: path to Phase-A consolidated BF16 final weights. "
                         "Loads MODEL WEIGHTS ONLY — optimizer/scheduler start fresh.")
    ap.add_argument("--extend_epochs", type=float, default=None,
                    help="Override training window WITHIN the hard max only "
                         "(CPT 1.75 / SFT 1.25). Values beyond the max are REFUSED — "
                         "extending past the window requires a new operator directive.")
    ap.add_argument("--runtime_evidence_out", default=None,
                    help="Write a runtime execution receipt here (world size, ZeRO stage, "
                         "steps and the gathered validation supervised-token mass) so the "
                         "distributed_loss_runtime gate can be verified from a REAL run "
                         "instead of a fixture. Best effort: never raises, never alters "
                         "training.")
    ap.add_argument("--consolidate", action="store_true",
                    help="Load the final DeepSpeed checkpoint via the trainer's "
                         "correctly-configured engine and emit a consolidated BF16 "
                         "model (save_16bit_model). No training. Requires --phase cpt "
                         "and --consolidate_out.")
    ap.add_argument("--consolidate_out", default=None,
                    help="Output dir for --consolidate.")
    args = ap.parse_args(argv)
    release = load_release(args.release_manifest, real_training=not args.dry_run)
    require(args.phase in release['phases'], 'Phase absent from release')
    request=None
    if not args.dry_run or args.init_from or args.resume_from_checkpoint or args.preflight_only:
        request=verify_training_request(release,args.phase,args.init_from,args.out_root,
            args.resume_from_checkpoint,args.launch_contract,args.contract_sha256)
    if args.preflight_only:
        print(json.dumps(dict(request,model_loaded=False,training_launched=False)))
        return request
    tok = AutoTokenizer.from_pretrained(release['tokenizer_path'], local_files_only=True)
    train_ds, val_ds, geometry = prepare_datasets(release, args.phase, tok)
    max_window = 1.75 if args.phase == 'cpt' else 1.25
    lr, warmup, wd = (4e-6, .04, .1) if args.phase == 'cpt' else (2e-6, .03, .01)
    if args.dry_run:
        report = dataset_report(release, args.phase, train_ds, val_ds, geometry)
        if args.report:
            from pathlib import Path
            Path(args.report).write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')
        print(json.dumps(report, indent=2))
        return report
    require(not args.consolidate, 'Legacy checkpoint consolidation disabled for clean release')
    # Use the same pre-model authority exercised by launch and resume preflight.
    out_dir = request['out_dir']
    model_src = request['model_src']
    if resource is not None:
        _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        resource.setrlimit(resource.RLIMIT_NOFILE, (hard,hard))
    set_seed(SEED)
    model = AutoModelForCausalLM.from_pretrained(
        model_src, local_files_only=True,
        torch_dtype=torch.bfloat16, attn_implementation='sdpa')
    model.config.use_cache = False
    require(all(p.requires_grad for p in model.parameters()), 'Full-parameter training required')

    epochs = max_window if args.extend_epochs is None else args.extend_epochs
    assert math.isfinite(epochs) and 0 < epochs <= max_window, (
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
        remove_unused_columns=False,
    )
    trainer_cls = WeightedLossTrainer  # both phases use global validation token mean
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
    trainer.train(resume_from_checkpoint=request['resume_from_checkpoint'])
    st = trainer.state
    stopped_early = (st.global_step < total_steps)
    ds = getattr(trainer, "denom_stats_summary", lambda: {})()
    if ds:
        print(f"[denom-audit] {json.dumps(ds)}")
        with open(f"{out_dir}/denom_audit.json", "w") as f:
            json.dump(ds, f, indent=1)
    if getattr(args, "runtime_evidence_out", None) and \
            int(getattr(trainer.args, "process_index", 0) or 0) == 0:
        write_runtime_evidence(
            args.runtime_evidence_out,
            collect_runtime_evidence(trainer, release, args, stopped_early, total_steps))
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
