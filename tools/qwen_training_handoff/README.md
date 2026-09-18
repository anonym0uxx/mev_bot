# Qwen 27B Full-FT Training Handoff Package
## Self-Contained — No Hermes/LLM Required

**Created by:** Hermes Agent (GLM-5.2)  
**Date:** 2026-08-29  
**Status:** AWAITING EXPORT CERTIFICATION — training data files must be generated and certified before this package is usable

---

## ⚠️ PREREQUISITES — DO NOT SKIP

1. **qwen_eval_v1** must be frozen (immutable eval set exists)
2. **qwen_cpt_v1** must be exported (JSONL, ~34M tokens, ~15K records)
3. **qwen_sft_v1** must be exported (JSONL, ~15M tokens, ~3K records)
4. **All export files must have certified hashes** in a manifest
5. **Hermes + GLM/llama.cpp must be STOPPED** and VRAM verified free
6. **3× NVIDIA RTX PRO 6000** GPUs must show 0MB VRAM usage

**Handoff sequence:**
```
1. Hermes finishes + certifies all exports/config/scripts
2. User reviews this package
3. Stop Hermes
4. Stop GLM/llama.cpp (frees GPUs)
5. Run gpu_check.sh — verify all 3 GPUs show 0MB VRAM
6. Run launch.sh — starts REAL training
7. Training runs autonomously — no Hermes needed
```

---

## Package Contents

| File | Purpose |
|------|---------|
| `train.py` | Main training entrypoint — self-contained, no LLM calls |
| `accelerate_config.yaml` | Accelerate config for 3-GPU ZeRO-3 |
| `deepspeed_config.json` | DeepSpeed ZeRO-3 config (BF16, contiguous gradients) |
| `training_args.json` | All hyperparameters (epochs, batch, LR, checkpoint cadence) |
| `requirements.txt` | Exact package versions for reproducibility |
| `launch.sh` | One-command launch for real training run |
| `resume.sh` | One-command resume from latest checkpoint |
| `gpu_check.sh` | Verify all 3 GPUs are free before launch |
| `monitor.sh` | GPU/RAM monitoring during training |
| `RUNBOOK.md` | Failure/OOM/distributed recovery procedures |

---

## Quick Start (after prerequisites met)

```bash
# 1. Verify GPUs are free (MUST pass before launch)
bash gpu_check.sh

# 2. Launch training
bash launch.sh

# 3. Monitor (separate terminal)
bash monitor.sh
```

## Resume from Checkpoint

```bash
# Resume from latest checkpoint
bash resume.sh

# Or specify a specific checkpoint
bash resume.sh --checkpoint output/qwen_cpt/checkpoint-5000
```

---

## Training Configuration Summary

| Parameter | CPT Phase | SFT Phase |
|-----------|-----------|-----------|
| Model | Qwen2.5-27B (full-parameter) | Qwen2.5-27B (full-parameter) |
| Precision | BF16 | BF16 |
| GPUs | 3× RTX PRO 6000 (96GB) | 3× RTX PRO 6000 (96GB) |
| Strategy | DeepSpeed ZeRO-3 | DeepSpeed ZeRO-3 |
| Epochs | 3 | 5 |
| Per-device batch | 2 | 2 |
| Grad accumulation | 8 (effective batch 48) | 8 (effective batch 48) |
| Learning rate | 2e-5 (cosine, warmup 5%) | 1e-5 (cosine, warmup 5%) |
| Max seq len | 4096 | 4096 |
| Checkpoint interval | 500 steps | 500 steps |
| Early checkpoints | Step 10, 50, 100, 200 | Step 10, 50, 100, 200 |
| Data | qwen_cpt_v1.jsonl (~34M tokens) | qwen_sft_v1.jsonl (~15M tokens) |

---

## Export File Paths (TO BE FILLED AFTER CERTIFICATION)

| Export | Path | SHA-256 | Record Count | Token Count |
|--------|------|---------|--------------|-------------|
| qwen_eval_v1 | `[TBD]` | `[TBD]` | `[TBD]` | `[TBD]` |
| qwen_cpt_v1 | `[TBD]` | `[TBD]` | `[TBD]` | `[TBD]` |
| qwen_sft_v1 | `[TBD]` | `[TBD]` | `[TBD]` | `[TBD]` |

*These values will be filled by Hermes after export certification completes.*

---

## No-Smoke-Test Feasibility Validation

The real run's early steps ARE the feasibility validation:

1. **ZeRO-3 sharding verification**: Monitor per-GPU VRAM at step 1 — all 3 GPUs should have roughly equal VRAM (model sharded across GPUs)
2. **Loss finiteness**: Verify loss is finite at step 1 (not NaN/Inf)
3. **Gradient flow**: Verify gradients are non-zero at step 1
4. **Throughput**: Log tokens/sec after first 100 steps
5. **Early checkpoint**: Save checkpoint at step 10, 50, 100, 200 — verify they save/load correctly
6. **Resume test**: After step 100, kill and resume from checkpoint — verify loss continuity

See `RUNBOOK.md` Section "Feasibility Validation" for detailed procedures.

---

## Expected Outputs

```
output/
├── qwen_cpt/
│   ├── checkpoint-10/
│   ├── checkpoint-50/
│   ├── checkpoint-100/
│   ├── checkpoint-200/
│   ├── checkpoint-500/
│   ├── ...
│   ├── checkpoint-{final}/
│   ├── training_logs/
│   │   ├── training_loss.json   # per-step loss
│   │   ├── gpu_metrics.json     # per-step GPU VRAM/RAM
│   │   └── throughput.json      # tokens/sec
│   └── trainer_state.json
└── qwen_sft/
    └── (same structure)
```

---

## Failure Recovery

See `RUNBOOK.md` for:
- OOM on GPU → reduce batch size, enable CPU offload
- OOM on CPU RAM → reduce ZeRO-3 offload ratio
- NaN/Inf loss → reduce LR, check data for corruption
- Distributed timeout → increase timeout, check network
- Checkpoint corruption → resume from earlier checkpoint
- Node crash → resume from latest valid checkpoint

---

*This package is self-contained. Training runs deterministically without any Hermes or LLM calls. All data is pre-exported and hashed.*
