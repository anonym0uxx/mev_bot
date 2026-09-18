# Qwen 27B Full-FT Training Runbook
## Failure / OOM / Distributed Recovery Procedures

This runbook covers all known failure modes for the ZeRO-3 full-parameter
training run. No Hermes/LLM is needed to execute these procedures.

---

## 1. Pre-Launch Checklist

- [ ] Hermes stopped (`hermes stop` or daemon closed)
- [ ] GLM/llama.cpp stopped (kill llama-server process)
- [ ] `./gpu_check.sh` passes — all 3 GPUs show <1GB VRAM used
- [ ] Training data exists: `qwen_cpt_v1.jsonl` (CPT) or `qwen_sft_v1.jsonl` (SFT)
- [ ] Python environment has `requirements.txt` installed
- [ ] DeepSpeed config matches GPU count (3)

---

## 2. CUDA Out-of-Memory (OOM)

**Symptom:** `torch.cuda.OutOfMemoryError` during training or model loading.

**Immediate action:**
1. The training script will attempt to exit cleanly. Check the log for the error.
2. If the process hangs, kill it:
   ```bash
   pkill -f "train.py"
   pkill -f "accelerate"
   ```
3. Verify GPUs are freed:
   ```bash
   nvidia-smi
   ```

**Recovery — reduce memory:**
Edit `training_args.json`:
- Reduce `per_device_train_batch_size`: 2 → 1
- Increase `gradient_accumulation_steps`: 16 → 32 (keeps effective batch size)
- If still OOM: enable CPU offloading in `deepspeed_config.json`:
  ```json
  "zero_optimization": {
    "stage": 3,
    "offload_optimizer": { "device": "cpu", "pin_memory": true },
    "offload_param": { "device": "cpu", "pin_memory": true }
  }
  ```
- If STILL OOM: reduce `max_seq_length`: 4096 → 2048

**Resume from checkpoint after fix:**
```bash
./resume.sh cpt
```

---

## 3. ZeRO-3 Sharding Failure

**Symptom:** Model loads on single GPU, OOM on one GPU while others idle.
Log shows: "RuntimeError: not enough memory for sharding" or similar.

**Cause:** DeepSpeed not properly initialized, or Accelerate config mismatch.

**Fix:**
1. Verify `accelerate_config.yaml` has `num_processes: 3` and `gpu_ids: 0,1,2`
2. Verify `deepspeed_config.json` has `"stage": 3`
3. Verify launch command uses `accelerate launch` (not `python train.py` directly)
4. Set environment variable:
   ```bash
   export DEEPSPEED_CONFIG=deepspeed_config.json
   ```
5. Relaunch:
   ```bash
   ./launch.sh cpt
   ```

---

## 4. Checkpoint Corruption / Resume Failure

**Symptom:** `resume.sh` fails with "checkpoint not found" or "state_dict mismatch".

**Fix:**
1. Check available checkpoints:
   ```bash
   ls -la ./output/qwen_cpt/checkpoint-*/
   ```
2. A checkpoint directory must contain `pytorch_model.bin` (or sharded) + `optimizer.pt` + `scheduler.pt` + `trainer_state.json`
3. If the latest checkpoint is corrupt (incomplete write during crash):
   ```bash
   # Remove the corrupt checkpoint
   rm -rf ./output/qwen_cpt/checkpoint-XXX
   # Resume from the previous one
   ./resume.sh cpt ./output/qwen_cpt/checkpoint-YYY
   ```
4. If ALL checkpoints are corrupt, restart from scratch:
   ```bash
   ./launch.sh cpt
   ```

---

## 5. Distributed Communication Error

**Symptom:** "Connection timed out", "port 29501 already in use", "NCCL error".

**Fix:**
1. Kill stale processes:
   ```bash
   pkill -f "train.py"
   pkill -f "accelerate"
   pkill -f "deepspeed"
   ```
2. Free the port:
   ```bash
   # Find process using port 29501
   lsof -i :29501 || true
   ```
3. Change the port in `launch.sh`:
   ```bash
   --main_process_port 29502
   ```
4. Relaunch

---

## 6. Training Loss Not Decreasing (NaN/Inf)

**Symptom:** Loss = NaN or Inf after a few steps.

**Fix:**
1. Stop training immediately (Ctrl-C or `pkill -f train.py`)
2. Check for numerical issues:
   - Reduce learning rate: 5e-5 → 1e-5 or 2e-5
   - Verify `bf16: true` in config (not fp16 — Qwen 27B needs BF16)
   - Check gradient clipping is active: `max_grad_norm: 1.0`
3. If loss is finite but flat:
   - Increase learning rate slightly
   - Check that data is properly tokenized (inspect first 5 examples)
4. Resume from last good checkpoint

---

## 7. Training Too Slow

**Symptom:** <500 tokens/sec throughput on 3×96GB GPUs for 27B model.

**Expected throughput:** 1500-3000 tokens/sec with ZeRO-3 + BF16 + gradient checkpointing.

**Fix:**
1. Enable Flash Attention:
   ```bash
   export USE_FLASH_ATTENTION=1
   ```
2. Disable gradient checkpointing (if VRAM allows):
   Set `gradient_checkpointing: false` in `training_args.json`
3. Increase batch size (if VRAM allows):
   `per_device_train_batch_size: 4`
4. Check GPU utilization with `./monitor.sh` — all 3 GPUs should be >80%

---

## 8. Disk Space Exhaustion

**Symptom:** "No space left on device" when saving checkpoints.

**Fix:**
1. Check disk space:
   ```bash
   df -h .
   ```
2. Reduce `save_total_limit` in `training_args.json`: 5 → 3
3. Delete old checkpoints manually:
   ```bash
   ls ./output/qwen_cpt/checkpoint-*/ | head -5
   # Keep only the 2 most recent
   rm -rf ./output/qwen_cpt/checkpoint-{old ones}
   ```
4. Each checkpoint for 27B BF16 = ~54GB (27B × 2 bytes/param). With optimizer
   state (ZeRO-3), checkpoint can be ~108GB each. Plan disk accordingly.

---

## 9. Training Process Killed by OS (OOM Killer)

**Symptom:** Process disappears without CUDA error. `dmesg` shows OOM killer.

**Fix:**
1. Check system RAM:
   ```bash
   free -h
   ```
2. ZeRO-3 offloads optimizer state to CPU — ensure system has sufficient RAM
   (27B model: ~108GB optimizer state on CPU)
3. Reduce `dataloader_num_workers`: 4 → 2
4. Enable ZeRO-3 CPU offloading (see section 2)
5. Or reduce `max_seq_length`: 4096 → 2048

---

## 10. Expected Output Locations

| Phase | Checkpoints | Final Model | Logs |
|-------|-------------|-------------|------|
| CPT | `./output/qwen_cpt/checkpoint-{step}/` | `./output/qwen_cpt/final/` | `./logs/train_cpt_*.log` |
| SFT | `./output/qwen_sft/checkpoint-{step}/` | `./output/qwen_sft/final/` | `./logs/train_sft_*.log` |

**Checkpoint contents:**
- `pytorch_model.bin` (or sharded `pytorch_model-0000X-of-0000Y.bin`)
- `optimizer.pt`
- `scheduler.pt`
- `trainer_state.json` (loss, lr, step history)
- `config.json`
- `tokenizer/` files

---

## 11. Post-Training Verification

After training completes:
1. Verify final model loads:
   ```bash
   python -c "from transformers import AutoModelForCausalLM; m = AutoModelForCausalLM.from_pretrained('./output/qwen_cpt/final', torch_dtype='bfloat16'); print(f'OK: {sum(p.numel() for p in m.parameters())/1e9:.2f}B params')"
   ```
2. Run eval using `qwen_eval_v1.jsonl` (separate eval script required)
3. Shadow testing before live promotion (per design doc)

---

## 12. Quick Reference Commands

```bash
# Launch CPT (Phase 1)
./launch.sh cpt

# Launch SFT (Phase 2, after CPT completes)
./launch.sh sft

# Resume from latest checkpoint
./resume.sh cpt

# Monitor GPUs
./monitor.sh

# Check GPUs free
./gpu_check.sh

# Kill training
pkill -f "train.py"
pkill -f "accelerate"

# Dry run (verify data + model load, no training)
accelerate launch --config_file accelerate_config.yaml train.py --phase cpt --dry_run
```

---

## 13. Early-Step Feasibility Validation Checklist

During the first ~50-100 steps of the real run, verify:

- [ ] ZeRO-3 is sharding: all 3 GPUs show similar VRAM usage (not one GPU at 96GB, others at 0)
- [ ] Per-GPU VRAM is stable (not monotonically increasing = memory leak)
- [ ] System RAM is stable
- [ ] Loss is finite (not NaN/Inf) and generally decreasing
- [ ] Gradients are non-zero (check log for gradient norms)
- [ ] Throughput is reasonable (>500 tokens/sec)
- [ ] Early checkpoints save successfully (steps 20, 50, 100)
- [ ] Checkpoint resume works: stop, resume, confirm loss matches

If any check fails, consult the relevant section above.
