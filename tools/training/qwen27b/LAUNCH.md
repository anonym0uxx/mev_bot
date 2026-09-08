# Qwen3.8-27B Full-Parameter Training — Launch Reference (DO NOT RUN WITHOUT OPERATOR GO)

Host: DESKTOP-CP8N3IC — 3× RTX PRO 6000 Blackwell 96GB (sm_120), 256GB RAM, Zen5.
Environment: WSL2 Ubuntu 26.04, venv `~/qwen-train` (Python 3.12.13 via deadsnakes).

## Pinned stack (recorded in run manifest — do not upgrade mid-run)
- torch 2.9.1+cu128 (CUDA 12.8, sm_120 in arch list)
- transformers 4.57.1, accelerate 1.11.0, deepspeed 0.18.2
- numpy 2.2.6, python 3.12.13, libaio-dev 0.3.113-8build1
- Attention: PyTorch SDPA. FlashAttention2 NOT required, NOT installed.

## WSL memory (`C:\Users\Alon\.wslconfig`) — APPLIED + VERIFIED
```ini
[wsl2]
memory=220GB          # was 6GB (!)
processors=30         # was 4
swap=24GB
swapfile=D:\\wsl\\swap\\wsl-swap.vhdx
localhostForwarding=true
```
Verified inside WSL after `wsl --shutdown`: 216GiB usable RAM, 30 CPUs, 24GB swap active, all 3 GPUs visible.

## Offload filesystem — ext4 on T710 NVMe (NOT /mnt/d)
Dedicated 1.8TB expandable VHDX `D:\wsl\training_ext4.vhdx` (D: = Crucial T710 4TB),
attached bare into WSL. DeepSpeed NVMe offload + checkpoints live on Linux-native ext4;
/mnt/d is used only for cold reads (training data) and cold artifact copies.

⚠️ **Linux device letters (`/dev/sdX`) are NON-AUTHORITATIVE.** They re-enumerate after
`wsl --shutdown`/reboot (observed: the training disk moved sde→sdd; sde became Docker's
1TB disk). NEVER format or mount by a remembered letter. Identify by evidence, mount by
LABEL/UUID only.

One-time format — mkfs is agent-blocklisted (hardline), OPERATOR runs it:
```powershell
# Windows (elevated), attach the EXACT VHDX bare:
wsl --mount D:\wsl\training_ext4.vhdx --vhd --bare
```
```bash
# inside WSL as root — POSITIVELY identify the blank 1.8TB disk first:
#   lsblk -b -o NAME,SIZE,FSTYPE,LABEL,UUID   -> exactly one disk with
#   SIZE=1932735283200, empty FSTYPE/LABEL/UUID, no partition table, unmounted.
#   (Docker disks are 1099511627776 bytes; system/swap disks all carry FSTYPE.)
DEV=/dev/sdX   # <- substitute the verified device
sudo mkfs.ext4 -L training -m 0 "$DEV"   # -m 0: dedicated data volume, no root reserve
```
After mkfs, mount/dirs/ownership are label-based and idempotent (agent-runnable):
```bash
sudo bash tools/training/qwen27b/mount_training.sh
```
Re-attach after any `wsl --shutdown` (VHDX bare-mounts do not persist):
```powershell
wsl --mount D:\wsl\training_ext4.vhdx --vhd --bare   # Windows side, exact path
```
```bash
sudo bash tools/training/qwen27b/mount_training.sh    # mounts by LABEL=training
```
Filesystem UUID is recorded in `PREFLIGHT_RECORD_*.json` next to this file after first
format; `mount_training.sh` accepts `TRAINING_UUID=<uuid>` to pin by UUID instead of label.

## Pre-launch verification (required)
```bash
source ~/qwen-train/bin/activate
ds_report                                   # AIO must be [OKAY] against libaio
findmnt -n -o FSTYPE /training              # must print: ext4
free -g; nvidia-smi                         # 216G RAM; 3 idle GPUs
```

## Schedule — VALIDATION-GOVERNED (operator directive 2026-08-31)
MAXIMUM WINDOWS, not targets: **CPT ≤1.75 ep @ 4e-6** (cosine, warmup 4%, wd 0.10),
**SFT ≤1.25 ep @ 2e-6** (cosine, warmup 3%, wd 0.01, fresh optimizer from selected
CPT weights). Eval + checkpoint at ~0.25-epoch marks PLUS a guaranteed terminal
eval+save exactly at the max-window boundary step (BoundaryEvalCallback), so
boundary governance always sees the true boundary state. `eval_on_start=True`
verifies the validation/loss pipeline before any weights move.

**SFT retention loss weighting (FINAL PRE-LAUNCH DELTA).** Domain prompts are
masked −100, so optimization share = share of LOSS-BEARING LABEL TOKENS, not raw
tokens or record count. `select_retention()` deterministically subsamples the
retention shard (sha256-of-record ordering, seedless) to **4.000%** of supervised
loss tokens — with the v2 cross shard: **139 records / 25,268 loss tokens**
against 605,884 domain loss tokens. The full shard stays preserved/modular on
disk; selection happens at load time and is reproducible. Loss normalization is
token-correct across variable-length grad
accumulation and devices: `average_tokens_across_devices=True` + Trainer's
`num_items_in_batch` (verified in transformers 4.57.1: sums `labels.ne(-100)`
over all micro-batches, gathers across ranks, divides the summed CE loss). The
trainer ASSERTS `model_accepts_loss_kwargs` at startup and refuses to train if
the token-normalized path is inactive.

**SFT supervision geometry (v2 cross shard + task-level gradient budget).**
The v1 cross-sectional shard was rebuilt (`qwen_sft_v2.jsonl`, exporter
`sft_cross_exporter_v2.py`): the v1 exporter had an interface bug (read `mint`/
flat causal keys from panel candidates that carry `mint_id`/nested
`causal_state`) producing sha256('')[:12] mint ids, EMPTY causal inputs,
all-WATCH/0.0-utility outputs, and MFE/MAE outcome values leaking into
rationale text. v2 fixes the interface, plus: counterfactual net-return columns
read from the compact `_bp` schema (`_cf_net_return`, the `_pct` columns don't
exist in slinky_gold_v3_compact — this bug zeroed ALL utilities); columnar
INPUT serialization (22 curated causal fields, 25–50 candidates fits seq
12,288 with zero truncation); two target modes — `live_action` (852 rec, 81%,
compact production output: regime summary, BUY≤3 or NO_BUY, WATCH triggers,
size band, CAUSAL-only evidence, risk/invalidation, aggregated SKIP reasons;
REAL output loss tokens p50 263 / p90 404 / p99 425 / max 434) and
`full_rank_aux` (198 rec, 19%, columnar rank/action/score over all candidates,
no prose). BUY targets are deliberately rare/decisive (108/852 panels BUY,
always a single top pick) via an outcome-evidence gate (feasibility ≥0.8,
survived_60s, no collapse, MAE > −2000bp, MFE > +3000bp) — outcomes define
TARGET correctness only; `assert_no_outcome_leakage` certifies zero outcome
vocabulary in model-visible text (0 hits / 1050 records; 0 eval-mint overlap;
0 dup ids/content; 0 overlength).
Task-level gradient budget: bounded per-task loss weights (cap 4.0×,
renormalized to preserve total token mass → unchanged effective LR), computed
from measured post-mask loss-token shares, NEVER from outcomes/economics
(`WeightedLossTrainer`; eval stays unweighted; no repetition/oversampling —
weights only). Measured → effective gradient shares: cross 85.4%→70.2% (target
70), post 5.9%→10.0%, rust 2.9%→10.0%, narrative 1.8%→5.0%, hermes
0.17%→0.67% (weight capped at 4× — accepted deviation from 1%; do NOT pad
data to close it), retention 3.9%→4.0%.

EXACT plan (post-holdout, post-retention-selection, real dataloader; see
EXACT_STEP_PLAN.json): CPT 92 steps/ep, 161 max, warmup 7, marks every 23 steps
(0.25→1.75 ep; 161 = 7×23 exactly). SFT 2,258 train items / 43 val, 95 steps/ep,
119 max, warmup 4, marks 23/46/69/92/115 + terminal boundary eval at **119**
(1.253 ep).
Selection: `load_best_model_at_end` on internal val loss
(`metric_for_best_model=eval_loss`, `greater_is_better=False`, eval/save both on
the same `steps` grid) — `final/` is
the BEST-validation checkpoint, NOT automatically the boundary. EarlyStopping
(patience=2 marks) enforces the overfitting rule: val worsening across 2 consecutive
marks stops the run and keeps the earlier best. Training loss NEVER governs
continuation. If val is still materially improving AT the boundary with no
degeneration: STOP AND ASK — `--extend_epochs` refuses values beyond 1.75/1.25.
Beyond val loss/ppl (val_history.jsonl) also review: train-vs-val divergence,
SFT schema/output validity, repetition/degeneration, grad/loss stability (TensorBoard).

## Phase A — CPT (window ≤1.75 ep @ 4e-6, seq 4096 EOS-packed)
```bash
cd /mnt/d/repos/mev_bot/tools/training/qwen27b && \
accelerate launch --config_file accelerate_zero3.yaml train_qwen27b.py --phase cpt
```

## Phase B — SFT+retention (window ≤1.25 ep @ 2e-6, seq<=12288 unpacked, dynamic pad)
Starts from the SELECTED (best-validation) CPT weights; optimizer/scheduler are FRESH
(weights-only load).
```bash
cd /mnt/d/repos/mev_bot/tools/training/qwen27b && \
accelerate launch --config_file accelerate_zero3.yaml train_qwen27b.py --phase sft \
  --init_from /training/runs/qwen27b_cpt_v1/cpt_final_bf16
```

## Boundary rule (NEVER extend automatically)
The 1.75/1.25 windows are hard caps baked into the trainer. If validation is still
materially improving at a boundary, the run prints a [BOUNDARY] notice — STOP AND ASK
the operator; extension past the window requires a new directive. Frozen
qwen_eval_v1.1 untouched until post-training evaluation.

## Checkpoints
Saved + validated at every ~0.25-epoch mark. Full ZeRO-3 resumable checkpoints are
~380GB each (fp32 master + Adam m,v + bf16 shards); `save_total_limit=2` keeps the
best-val checkpoint (rotation-protected) + most recent resumable. limit=3 was
rejected: 3×380GB retained + ~380GB peak while the next checkpoint is written +
~324GB live NVMe optimizer state exceeds the 1.8TB volume; limit=2 peaks at
~1.14TB written + 324GB offload ≈ 1.46TB with headroom. To preserve a rotating
mark for later comparison, consolidate it to BF16 (~54GB) before it rotates out.
**Consolidate via the trainer's `--consolidate` mode, NOT `zero_to_fp32.py`** (which
fails on `offload_param: nvme` — empty `fp32_flat_groups`):
```bash
cd /mnt/d/repos/mev_bot/tools/training/qwen27b && \
accelerate launch --config_file accelerate_zero3.yaml train_qwen27b.py --phase cpt \
  --consolidate --consolidate_out /training/runs/qwen27b_cpt_v1/cpt_final_bf16
```
`final/` is the SHARDED ZeRO-3 checkpoint (301GB), NOT consolidated — the
consolidated ~54GB BF16 weights land in `cpt_final_bf16/` (verified via
`from_pretrained`, param count 26,895,998,464). See
`references/checkpoint-consolidation-offload-param.md` (skill) for the full
gotcha chain. Do NOT assume the final checkpoint is best — all epoch-mark
checkpoints are preserved (full or bf16-consolidated) for selection.

## Fallback ladder (decide BEFORE a run starts — never mid-run)
1. PRIMARY: `ds_zero3_nvme.json` — AdamW FP32 moments, NVMe offload to /training/offload.
2. If NVMe throughput unacceptable: `ds_zero3_bf16opt_cpu_FALLBACK.json` — BF16 optimizer
   states + CPU offload (~162GB host RAM, fits 216GB). Point the Accelerate YAML at it.
3. Only if both infeasible, with operator sign-off: BF16 LoRA (r=128, alpha=256, all linear).

## Monitoring
```bash
tensorboard --logdir /training/runs --port 6006   # train + eval loss, LR, grad-norm
watch -n 5 nvidia-smi
```
Watch: train vs val loss divergence, val perplexity, SFT schema validity of samples,
repetition/degeneration. Resume after interruption: re-run the same launch command
(auto-detects latest checkpoint; DS restores optimizer+scheduler+RNG).
