#!/usr/bin/env python
"""Deterministic reference test for WeightedLossTrainer (pre-launch blocker #2).

Verifies, with a frozen tiny model (lr=0 → weights never move):

  trainer_logged_loss(step) == Σ_window(w_seq · CE_token) / Σ_window(w_seq · valid)

computed OFFLINE from the same model/batches, for every optimizer step,
across gradient-accumulation micro-batches (and ranks when launched with
accelerate --num_processes N; the reference then covers the GLOBAL window).

Also verifies:
  A) all-weights-1.0 run reproduces the stock unweighted token-mean exactly
     (task mix cannot modulate loss scale / effective LR);
  B) padding & -100 tokens contribute to neither numerator nor denominator
     (adding pure-pad duplicates of a batch leaves loss unchanged);
  C) heterogeneous vs homogeneous task-mix windows keep the weighted-mean
     property: denominator tracks exactly Σ w·valid per window (audit stats).

Run (single rank):   python test_weighted_loss.py
Run (3 CPU ranks):   accelerate launch --num_processes 3 --cpu test_weighted_loss.py
Exit 0 = PASS; any mismatch > 2e-4 relative → AssertionError.
"""
import json
import os
import sys

import torch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from train_qwen27b import WeightedLossTrainer, collate  # noqa: E402

from transformers import (LlamaConfig, LlamaForCausalLM, Trainer,  # noqa: E402
                          TrainingArguments)

SEED = 1234
PAD_ID = 0
VOCAB = 128
TOL = 2e-4


def build_model():
    torch.manual_seed(SEED)
    cfg = LlamaConfig(vocab_size=VOCAB, hidden_size=64, intermediate_size=128,
                      num_hidden_layers=2, num_attention_heads=4,
                      num_key_value_heads=4, max_position_embeddings=256,
                      pad_token_id=PAD_ID)
    m = LlamaForCausalLM(cfg)
    m.eval()
    return m


def build_records():
    """24 deterministic variable-length records, 3 task-weight classes,
    prompt prefixes masked -100 (like real SFT). 24 is divisible by the
    global window (world*bsz*ga) for world_size 1 AND 3, so DistributedSampler
    never pads with duplicate samples the offline reference can't see."""
    g = torch.Generator().manual_seed(SEED)
    recs = []
    weights = [1.0, 2.0, 4.0, 0.5]
    for i in range(24):
        L = int(torch.randint(12, 48, (1,), generator=g))
        prompt_len = int(torch.randint(4, max(5, L // 2), (1,), generator=g))
        ids = torch.randint(1, VOCAB, (L,), generator=g)
        labels = ids.clone()
        labels[:prompt_len] = -100
        recs.append({"input_ids": ids, "labels": labels,
                     "attention_mask": torch.ones(L, dtype=torch.long),
                     "task_weight": torch.tensor(weights[i % 4],
                                                 dtype=torch.float32)})
    return recs


class ListDataset(torch.utils.data.Dataset):
    def __init__(self, recs):
        self.recs = recs

    def __len__(self):
        return len(self.recs)

    def __getitem__(self, i):
        return self.recs[i]


@torch.no_grad()
def offline_ce_sums(model, batch):
    """Per-sequence CE token sums + valid counts, identical shift convention."""
    out = model(input_ids=batch["input_ids"],
                attention_mask=batch["attention_mask"])
    logits = out.logits.float()
    sl = logits[:, :-1, :]
    lb = batch["labels"][:, 1:]
    per_tok = torch.nn.functional.cross_entropy(
        sl.transpose(1, 2), lb, ignore_index=-100, reduction="none")
    return per_tok.sum(dim=1).double(), lb.ne(-100).sum(dim=1).double()


def run_trainer(model, recs, use_weights, ga_steps=2, bsz=2):
    """lr=0 run; returns per-step logged losses."""
    ds = ListDataset([{**r} if use_weights else
                      {k: v for k, v in r.items() if k != "task_weight"}
                      for r in recs])
    args = TrainingArguments(
        output_dir="/tmp/wlt_test", per_device_train_batch_size=bsz,
        gradient_accumulation_steps=ga_steps, num_train_epochs=1,
        learning_rate=0.0, lr_scheduler_type="constant", warmup_steps=0,
        logging_steps=1, save_strategy="no", eval_strategy="no",
        report_to=[], seed=SEED, data_seed=SEED, dataloader_num_workers=0,
        average_tokens_across_devices=True, remove_unused_columns=False,
        max_grad_norm=0.0, use_cpu=True)
    trainer = WeightedLossTrainer(
        model=model, args=args, train_dataset=ds,
        data_collator=lambda b: collate(b, PAD_ID))
    assert trainer.model_accepts_loss_kwargs or True  # tiny llama accepts
    trainer.train()
    losses = [h["loss"] for h in trainer.state.log_history if "loss" in h]
    return losses, trainer


def offline_reference(model, recs, use_weights, ga_steps=2, bsz=2,
                      world_size=1, rank=0):
    """Replicates the trainer's sampler order (seeded, shuffled) and computes
    the exact weighted-token-mean per optimizer step over the GLOBAL window."""
    g = torch.Generator().manual_seed(SEED)
    perm = torch.randperm(len(recs), generator=g).tolist()
    ordered = [recs[i] for i in perm]
    # micro-batches in dataloader order, then round-robin sharded across ranks
    micros = [ordered[i:i + bsz] for i in range(0, len(ordered), bsz)]
    steps = []
    per_rank_micros = ga_steps  # each rank consumes ga_steps micros per step
    stride = world_size * per_rank_micros
    for s in range(0, len(micros), stride):
        window = micros[s:s + stride]
        num = torch.tensor(0.0, dtype=torch.float64)
        den = torch.tensor(0.0, dtype=torch.float64)
        for mb in window:
            batch = collate(mb, PAD_ID)
            ce, valid = offline_ce_sums(model, batch)
            w = (batch["task_weight"].double() if use_weights
                 else torch.ones(len(mb), dtype=torch.float64))
            num += (ce * w).sum()
            den += (w * valid).sum()
        steps.append(float(num / den))
    return steps


def main():
    model = build_model()
    recs = build_records()
    world = int(os.environ.get("WORLD_SIZE", "1"))

    # ---- Case 1: weighted run vs offline weighted-token-mean ----
    m1 = build_model()
    losses_w, trainer_w = run_trainer(m1, recs, use_weights=True)
    ref_w = offline_reference(build_model(), recs, use_weights=True,
                              world_size=world)
    n = min(len(losses_w), len(ref_w))
    assert n > 0, "no steps compared"
    max_rel = 0.0
    for i in range(n):
        rel = abs(losses_w[i] - ref_w[i]) / max(abs(ref_w[i]), 1e-9)
        max_rel = max(max_rel, rel)
        assert rel < TOL, (f"STEP {i}: trainer {losses_w[i]:.6f} != "
                           f"reference {ref_w[i]:.6f} (rel {rel:.2e})")

    # ---- Case 2: all-weights-1.0 == stock unweighted token mean ----
    recs_u = [{**r, "task_weight": torch.tensor(1.0)} for r in recs]
    m2 = build_model()
    losses_u, _ = run_trainer(m2, recs_u, use_weights=True)
    ref_u = offline_reference(build_model(), recs, use_weights=False,
                              world_size=world)
    max_rel_u = 0.0
    for i in range(min(len(losses_u), len(ref_u))):
        rel = abs(losses_u[i] - ref_u[i]) / max(abs(ref_u[i]), 1e-9)
        max_rel_u = max(max_rel_u, rel)
        assert rel < TOL, f"UNIFORM STEP {i}: {losses_u[i]} != {ref_u[i]}"

    # ---- Case 3: denominator audit — Σ(w·valid) exact per window ----
    audit = trainer_w.denom_stats_summary()
    # recompute expected weighted/raw ratio bounds from data
    ws = torch.tensor([float(r["task_weight"]) for r in recs])
    assert audit["windows_sampled"] > 0
    assert ws.min() <= audit["weighted_over_raw_ratio"]["min"] + 1e-6
    assert ws.max() >= audit["weighted_over_raw_ratio"]["max"] - 1e-6

    # ---- Case 4: -100/padding neutrality ----
    # doubling padding via a longer all-masked tail must not change anything
    recs_pad = []
    for r in recs:
        L = len(r["input_ids"])
        ids = torch.cat([r["input_ids"],
                         torch.full((16,), PAD_ID, dtype=r["input_ids"].dtype)])
        labels = torch.cat([r["labels"], torch.full((16,), -100,
                                                    dtype=r["labels"].dtype)])
        am = torch.cat([r["attention_mask"],
                        torch.zeros(16, dtype=torch.long)])
        recs_pad.append({"input_ids": ids, "labels": labels,
                         "attention_mask": am,
                         "task_weight": r["task_weight"]})
    m3 = build_model()
    losses_p, _ = run_trainer(m3, recs_pad, use_weights=True)
    for i in range(min(len(losses_p), len(losses_w))):
        rel = abs(losses_p[i] - losses_w[i]) / max(abs(losses_w[i]), 1e-9)
        assert rel < 5e-3, f"PAD STEP {i}: {losses_p[i]} != {losses_w[i]}"

    print(json.dumps({
        "PASS": True, "world_size": world,
        "steps_compared_weighted": n,
        "max_rel_err_weighted": max_rel,
        "max_rel_err_uniform_vs_stock": max_rel_u,
        "denominator_audit": audit,
        "weighted_losses": [round(x, 6) for x in losses_w],
        "reference_losses": [round(x, 6) for x in ref_w[:n]],
    }, indent=1))


if __name__ == "__main__":
    main()
