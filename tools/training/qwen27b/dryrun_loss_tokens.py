#!/usr/bin/env python3
"""Dry-run for FINAL PRE-LAUNCH DELTA: exact LOSS-BEARING LABEL TOKEN composition
after chat templating/prompt masking, retention selection to ~4% of supervised
loss tokens, revised SFT step plan from the resulting dataloader, and
verification of token-normalized gradient accumulation (num_items_in_batch /
average_tokens_across_devices) against the pinned transformers version.
No GPU, no model weights, no training."""
import json, math, sys
from collections import defaultdict

sys.path.insert(0, "/mnt/d/repos/mev_bot/tools/training/qwen27b")
from train_qwen27b import (build_example, loss_tokens, select_retention,
                           split_train_val, load_jsonl, SFTDataset,
                           TOKENIZER_REPO, TOKENIZER_REV, RETENTION_TARGET,
                           task_bucket, compute_task_weights, TASK_TARGETS)
from transformers import AutoTokenizer
import transformers

ROOT = "/mnt/d/repos/mev_bot/tools/data-pipeline/output/qwen_curriculum_v1"
tok = AutoTokenizer.from_pretrained(TOKENIZER_REPO, revision=TOKENIZER_REV)

domain = load_jsonl(f"{ROOT}/sft/qwen_sft_v2.jsonl")
retention_full = load_jsonl(f"{ROOT}/retention/qwen_retention_v1.jsonl")

# --- loss-bearing label tokens per source / task ---
per_task = defaultdict(lambda: [0, 0])   # prefix -> [records, loss_tokens]
dom_loss = dom_total = 0
for r in domain:
    ids, labels = build_example(r, tok)
    lt = loss_tokens(labels)
    dom_loss += lt; dom_total += len(ids)
    pfx = str(r.get("id", "?")).rsplit("_", 1)[0]
    per_task[pfx][0] += 1; per_task[pfx][1] += lt

ret_loss_full = ret_total_full = 0
for r in retention_full:
    ids, labels = build_example(r, tok)
    ret_loss_full += loss_tokens(labels); ret_total_full += len(ids)

print("== LOSS-BEARING LABEL TOKENS (post templating/masking) ==")
print(f"specialized SFT : {len(domain)} rec  loss_tok={dom_loss:,}  "
      f"raw_tok={dom_total:,}  mask_ratio={1-dom_loss/dom_total:.3f}")
print(f"retention FULL  : {len(retention_full)} rec  loss_tok={ret_loss_full:,}  "
      f"raw_tok={ret_total_full:,}")
print(f"FULL-shard share of loss tokens would be: "
      f"{ret_loss_full/(dom_loss+ret_loss_full):.4f}")
print("-- per SFT task (id prefix) --")
for pfx in sorted(per_task, key=lambda p: -per_task[p][1]):
    n, lt = per_task[pfx]
    print(f"  {pfx:18s} {n:5d} rec  loss_tok={lt:>9,}  ({lt/dom_loss:.1%} of domain)")

# --- deterministic retention selection ---
kept, stats = select_retention(domain, retention_full, tok)
print(f"\n== RETENTION SELECTION (target={RETENTION_TARGET}) ==")
print(json.dumps(stats, indent=2))

# --- revised SFT step plan from the REAL post-holdout dataloader ---
recs = domain + kept
tr, va = split_train_val(recs)
pairs = [(task_bucket(r), loss_tokens(build_example(r, tok)[1]))
         for r in tr if build_example(r, tok)]
task_w, per_bucket, eff_shares = compute_task_weights(pairs)
print("\n== SUPERVISION GEOMETRY (train split) ==")
tot_b = sum(per_bucket.values())
for b in sorted(per_bucket, key=lambda x: -per_bucket[x]):
    print(f"  {b:10s} raw_loss_tok={per_bucket[b]:>9,} ({per_bucket[b]/tot_b:6.2%})"
          f"  weight={task_w[b]:.4f}  effective_share={eff_shares[b]:.2%}"
          f"  target={TASK_TARGETS.get(b, 0):.0%}")
chk = sum(task_w[b] * per_bucket[b] for b in per_bucket)
print(f"  token-mass preservation: weighted={chk:,.0f} raw={tot_b:,} "
      f"(ratio {chk/tot_b:.6f} — must be 1.0)")
train_ds = SFTDataset(tr, tok, task_weights=task_w)
val_ds = SFTDataset(va, tok, shuffle=False)
gb = 24
spe = math.ceil(len(train_ds) / gb)
total = math.ceil(spe * 1.25)
mark = max(1, math.floor(spe / 4))
marks = list(range(mark, total + 1, mark))
if total not in marks:
    marks.append(total)
print(f"\n== REVISED SFT STEP PLAN ==")
print(f"train={len(train_ds)} val={len(val_ds)} steps/epoch={spe} "
      f"total(max,1.25ep)={total} warmup={math.ceil(total*0.03)} mark={mark}")
print(f"marks(steps)={marks}")
print(f"marks(epochs)={[round(s/spe,3) for s in marks]}")

# --- verify token-normalized accumulation in THIS transformers version ---
print(f"\n== TOKEN-NORMALIZATION VERIFICATION (transformers "
      f"{transformers.__version__}) ==")
import inspect
from transformers import TrainingArguments, Trainer
sig = inspect.signature(TrainingArguments.__init__).parameters
print(f"average_tokens_across_devices param exists: "
      f"{'average_tokens_across_devices' in sig}")
ta = TrainingArguments(output_dir="/tmp/_x", average_tokens_across_devices=True)
print(f"accepted+set: {ta.average_tokens_across_devices}")
src = inspect.getsource(Trainer.training_step)
print(f"Trainer.training_step handles num_items_in_batch: "
      f"{'num_items_in_batch' in src}")
src2 = inspect.getsource(Trainer._get_num_items_in_batch)
print(f"_get_num_items_in_batch counts valid labels (ne(-100)): "
      f"{'ne(-100)' in src2}, gathers across devices when "
      f"average_tokens_across_devices: {'gather' in src2}")
from transformers.models.qwen3 import modeling_qwen3 as q3
fp = set(inspect.signature(q3.Qwen3ForCausalLM.forward).parameters)
print(f"Qwen3ForCausalLM.forward loss-kwargs passthrough: "
      f"{'loss_kwargs' in fp or 'kwargs' in fp or 'num_items_in_batch' in fp} "
      f"({sorted(p for p in fp if 'kw' in p or 'num_items' in p)})")
