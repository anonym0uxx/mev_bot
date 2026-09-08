#!/usr/bin/env python3
"""Dry-run: EXACT optimizer steps / warmup / checkpoint marks from the REAL
datasets after the 2.5% group-disjoint holdout. No GPU, no model, no training.
Uses the same code paths as train_qwen27b.py (imported, not duplicated)."""
import math, sys, json
sys.path.insert(0, "/mnt/d/repos/mev_bot/tools/training/qwen27b")
from train_qwen27b import (load_jsonl, split_train_val, PackedCPTDataset,
                           SFTDataset, select_retention, build_example,
                           loss_tokens, task_bucket, compute_task_weights,
                           TOKENIZER_REPO, TOKENIZER_REV)
from transformers import AutoTokenizer

DATA = "/mnt/d/repos/mev_bot/tools/data-pipeline/output/qwen_curriculum_v1"
tok = AutoTokenizer.from_pretrained(TOKENIZER_REPO, revision=TOKENIZER_REV)
GB = 24  # 1 per-dev x 8 accum x 3 gpus

def plan(name, ds_len, max_window, warmup):
    spe = math.ceil(ds_len / GB)
    total = math.ceil(spe * max_window)
    mark = max(1, math.floor(spe / 4))
    marks = list(range(mark, total + 1, mark))
    if total not in marks:
        marks.append(total)   # BoundaryEvalCallback terminal eval+save
    return {"phase": name, "train_items": ds_len, "steps_per_epoch": spe,
            "max_window_ep": max_window, "total_steps_max": total,
            "warmup_steps": math.ceil(total * warmup), "mark_every": mark,
            "marks_steps": marks,
            "marks_epochs": [round(s / spe, 3) for s in marks]}

out = {}
docs = load_jsonl(f"{DATA}/cpt/qwen_cpt_v1.jsonl")
tr, va = split_train_val(docs)
cpt_tr = PackedCPTDataset(tr, tok)
cpt_va = PackedCPTDataset(va, tok, shuffle=False)
out["cpt"] = plan("cpt", len(cpt_tr), 1.75, 0.04)
out["cpt"]["docs_train"], out["cpt"]["docs_val"] = len(tr), len(va)
out["cpt"]["val_blocks"] = len(cpt_va)

domain = load_jsonl(f"{DATA}/sft/qwen_sft_v2.jsonl")
retention_full = load_jsonl(f"{DATA}/retention/qwen_retention_v1.jsonl")
retention, rstats = select_retention(domain, retention_full, tok)
recs = domain + retention
tr, va = split_train_val(recs)
pairs = []
for r in tr:
    ex = build_example(r, tok)
    if ex:
        pairs.append((task_bucket(r), loss_tokens(ex[1])))
task_w, per_bucket, eff_shares = compute_task_weights(pairs)
sft_tr = SFTDataset(tr, tok, task_weights=task_w)
sft_va = SFTDataset(va, tok, shuffle=False)
out["sft"] = plan("sft", len(sft_tr), 1.25, 0.03)
out["sft"]["recs_train"], out["sft"]["recs_val"] = len(tr), len(va)
out["sft"]["val_examples"] = len(sft_va)
out["sft"]["retention_selection"] = {k: (int(v) if not isinstance(v, float) else v)
                                     for k, v in rstats.items()}
out["sft"]["supervision_geometry"] = {
    "loss_tokens_by_bucket": per_bucket,
    "task_weights": {b: round(w, 4) for b, w in task_w.items()},
    "effective_gradient_shares": eff_shares,
}

print(json.dumps(out, indent=1))
with open("/mnt/d/repos/mev_bot/tools/training/qwen27b/EXACT_STEP_PLAN.json", "w") as f:
    json.dump(out, f, indent=1)
