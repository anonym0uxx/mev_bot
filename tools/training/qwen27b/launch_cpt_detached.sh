#!/usr/bin/env bash
# Detached CPT launcher — survives wsl.exe/wrapper death.
# Usage (from Windows): wsl -d Ubuntu -- bash /mnt/d/repos/mev_bot/tools/training/qwen27b/launch_cpt_detached.sh
set -euo pipefail
RUN_DIR=/training/runs/qwen27b_cpt_v1
LOG=$RUN_DIR/train.log
# HARD GUARD: /training must be the ext4 LABEL=training disk, not rootfs.
# (2026-08-31: post-BSOD relaunch wrote to rootfs because the mount was lost.)
findmnt -n -o SOURCE,FSTYPE /training | grep -q ext4 || { echo "FATAL: /training not mounted (ext4). Run: wsl --mount D:\\wsl\\training_ext4.vhdx --vhd --bare, then mount -L training /training"; exit 1; }
mkdir -p "$RUN_DIR"
# [.] so a bash -c cmdline carrying this pattern never self-matches.
if pgrep -f "train_qwen27b[.]py --phase cpt" >/dev/null; then
  echo "ALREADY RUNNING:"; pgrep -af "train_qwen27b[.]py --phase cpt"; exit 0
fi
# Fresh-start hygiene: remove stale checkpoint dirs from any interrupted save.
# DeepSpeed save_checkpoint refuses to overwrite an existing path, so a run that
# died mid-save (e.g. 2026-09-01 12:24 FileExistsError: offloaded_tensors/rank0X)
# leaves a blocker for the next launch. There is no resume-from-partial-save, so
# purging is always safe here.
if compgen -G "$RUN_DIR/checkpoint-*" >/dev/null; then
  echo "purging stale checkpoints:"; ls -d "$RUN_DIR"/checkpoint-*
  rm -rf "$RUN_DIR"/checkpoint-*
fi
# Disk/RAM headroom preflight (grounded: 27B NVMe offload steady-state ~300GB,
# one sharded ZeRO-3 ckpt ~380GB, save_total_limit=2 -> ~1.1TB transient + margin).
# Runs BEFORE any launch; refuses when the next checkpoint-save could exhaust the
# ext4 volume (the #19/#22 failure family, shifted from RAM to disk).
OFFLOAD_DIR=$(grep -oE '"nvme_path":[[:space:]]*"[^"]+"' \
  /mnt/d/repos/mev_bot/tools/training/qwen27b/ds_zero3_nvme.json \
  | head -1 | sed 's/.*"\([^"]*\)"$/\1/')
if [ -n "$OFFLOAD_DIR" ] && [ -d "$OFFLOAD_DIR" ]; then
  STALE=$(find "$OFFLOAD_DIR" -type f -name '*.swp' 2>/dev/null | wc -l)
  if [ "$STALE" -gt 0 ]; then
    echo "purging stale NVMe offload swap files ($STALE .swp) — safe: no active run"
    find "$OFFLOAD_DIR" -type f -name '*.swp' -delete 2>/dev/null || true
  fi
fi
FREE_G=$(df -BG --output=avail /training 2>/dev/null | tail -1 | tr -dc '0-9')
AVL_G=$(free -g | awk '/^Mem:/{print $7}')
echo "[preflight] /training free=${FREE_G}G guest-RAM-avail=${AVL_G}G"
if [ -n "$FREE_G" ] && [ "$FREE_G" -lt 1000 ]; then
  echo "FATAL: /training has ${FREE_G}G free (< 1000G). Refusing launch to prevent" \
       "disk exhaustion at the next checkpoint save. Free space or reduce" \
       "save_total_limit before relaunching."; exit 1
fi
source ~/qwen-train/bin/activate
# Durability: re-apply the DeepSpeed 0.18.2 NVMe-offload save copytree patch
# (dirs_exist_ok False->True) so a future pip reinstall can't silently
# reintroduce the FileExistsError that killed attempts #22 and #25.
bash /mnt/d/repos/mev_bot/tools/training/qwen27b/patch_deepspeed_save.sh
cd /mnt/d/repos/mev_bot/tools/training/qwen27b
# --- Stability env (single source of truth; 2026-08-31 hardening) ---
# NCCL cuMem off: WSL2 NCCL error 999 without it.
export NCCL_CUMEM_ENABLE=0
# Allocator: default caching allocator. Verdict after attempts #7-#10: the
# 'OOM with 35GB free' signature is NOT allocator fragmentation — dxg caps
# effective per-GPU commit at ~61GB (constant across native, GC-tuned, and
# cudaMallocAsync backends). Fix is lower peak usage (offload_param: cpu in
# ds config), not allocator tuning. expandable_segments/VMM unsupported on dxg.
unset PYTORCH_CUDA_ALLOC_CONF 2>/dev/null || true
# overlap_comm's recordStream keeps blocks un-freeable until stream sync;
# this avoids reserved-memory creep during backward with comm overlap.
export TORCH_NCCL_AVOID_RECORD_STREAMS=1
# 1 tqdm progress line / 30s instead of ~10/s — log growth control.
export TQDM_MININTERVAL=30
# NVMe param offload opens one swap file per param + aio events; the default
# soft nofile=1024 exhausted at first eval (attempt #16: swap_in AssertionError
# + OSError 24). Hard limit in this distro is 1048576.
ulimit -n 1048576
echo "=== launch $(date -Is) ===" >> "$LOG"
setsid accelerate launch --config_file accelerate_zero3.yaml train_qwen27b.py --phase cpt \
  >> "$LOG" 2>&1 < /dev/null &
LAUNCHER_PID=$!
disown
# ZeRO-3 init failures often surface after >5s — verify at 30s.
sleep 30
echo "launched: launcher_pid=$LAUNCHER_PID"
pgrep -af "train_qwen27b[.]py --phase cpt" || { echo "LAUNCH FAILED — tail:"; tail -20 "$LOG"; exit 1; }
