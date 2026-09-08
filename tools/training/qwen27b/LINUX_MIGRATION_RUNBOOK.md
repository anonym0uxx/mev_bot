# Qwen 27B Training → Native Linux Migration Runbook

**Status:** FINAL — hardware verified, partition feasible, memory math corrected, decisions locked.
**Scope:** Move Qwen3.8-27B CPT/SFT training from WSL2 (Windows) to *native* Linux on the **same physical machine** via a dual-boot partition. No external drive, no server change.
**Operator decisions (locked 2026-09-01):**
- mev_bot does NOT need to be live during training (dual-boot acceptable).
- GPUs are **PCIe, no NVLink** (operator-confirmed).
- **Secure Boot = DISABLE** (decided for autonomous operation — see §4; rationale in §15).
- Recurring cycle until a dedicated training server exists.

---

## 1. Why migrate (corrected root-cause analysis)

Every residual training failure traces to **WSL2's `/dev/dxg` GPU layer**, not to our training math or the optimizer. The specific mechanism:

Under dxg, GPU memory is **backed by host RAM** and subject to a **~57–59GB per-process commit ceiling**. Because a ZeRO-3 shard of the params + grads + activations **could not fit** under that ceiling, we were forced to NVMe-offload the *parameters* — which runs DeepSpeed's `PartitionedParamSwapper`, the exact component behind every "buffer already assigned" / "Not enough buffers for swapping" crash.

**Native Linux removes the dxg ceiling**, so the parameters finally fit in real 96GB VRAM and the param-swapper — the single biggest source of our instability — is deleted from the config entirely.

### The one thing native Linux does NOT fix (important correction)
A 27B model's FP32 Adam optimizer state is large and *must* stay offloaded regardless of platform:

| Component | Size (27B) | Fits in… |
|---|---|---|
| fp32 master weights | 108 GB | — |
| Adam `m` (exp_avg) | 108 GB | — |
| Adam `v` (exp_avg_sq) | 108 GB | — |
| **Optimizer total** | **324 GB** | **exceeds 256GB RAM; exceeds 96GB/GPU sharded → must stay NVMe-offloaded** |
| params (BF16) | 54 GB | GPU (18GB/rank) |
| grads (FP32, transient) | 108 GB | GPU (36GB/rank) |

**Conclusion:** we keep `offload_optimizer: nvme` (already running, stable) and *delete* `offload_param` (params → GPU). This is a **one-block surgical change** with **zero training-dynamics change** — same FP32 Adam (betas 0.9/0.95, eps 1e-8), same BF16, same batch, same LR schedule.

| | WSL2 (today) | Native Linux (target) |
|---|---|---|
| Params | NVMe → buggy `PartitionedParamSwapper` | **GPU** (fits in 96GB) |
| Optimizer | NVMe (stable) | NVMe (unchanged) |
| dxg commit ceiling | ~58GB/GPU | gone |
| VM bounce / wslservice wedge | yes | gone |
| ~812GB swap-file bloat | yes | gone (no param swap files) |

---

## 2. Verified hardware inventory (measured, not assumed)

- **GPUs:** 3× `NVIDIA RTX PRO 6000 Blackwell Workstation`, **96GB each**, compute capability **12.0 (sm_120)**, **PCIe (no NVLink)**
- **CPU/RAM:** AMD Zen5, **256GB RAM**
- **Storage:** 2× Crucial T710 4TB NVMe (GPT, UEFI firmware)
  - **Disk 0** (`nvme0n1`): ESP 0.2GB + MSR 16MB + **C: 3725GB (3.19TB free)** + Recovery 0.8GB
  - **Disk 1** (`nvme1n1`): **D: "Data" 3726GB (686GB free)** — holds `D:\wsl\training_ext4.vhdx` + swap VHDX
- **Training data footprint (measured):** ~**300MB** — `cpt/` 24M, `sft/` 44M, `retention/` 4.2M, `eval*` ~140M, `candidates/` 52M. (The 1.8TB VHDX was ~all offload/checkpoint churn, now purged.) **Nothing large migrates.**
- **Model:** `Qwen3.8-27B` (unsloth), full-parameter BF16.
- **Stack to reproduce:** torch `2.9.1+cu128`, deepspeed `0.18.2`, transformers `5.16.1`, accelerate `1.11.0`, CUDA 12.8.

**Partition feasibility: YES** — C: has 3.19TB free; carve ~2TB via `diskpart shrink`.

---

## 3. Target architecture

```
Boot A: Windows 10  → mev_bot / Hermes / cron / trading (unchanged)
Boot B: Ubuntu 24.04 → native CUDA training (NEW)

Disk 0 (nvme0n1): [EFI][C: ~1.7TB][ / 100G ][ /training ~1.85TB ][swap? 32G]
Disk 1 (nvme1n1): [D: 3.7TB]  (untouched — holds WSL data, rollback path)
```

---

## 4. Pre-flight (Windows, one-time, ~15 min)

Run **as Administrator**:

```powershell
# 1. Disable hibernation + Fast Startup (REQUIRED for dual-boot: Fast Startup
#    leaves NTFS in a hibernated state Linux can't safely write)
powercfg /h off

# 2. (Optional) move pagefile off C: to free maximum shrink headroom
#    System > Advanced > Performance > Virtual memory

# 3. Confirm shrink headroom
Get-Partition -DriveLetter C | Select Size
```

**BIOS — DISABLE Secure Boot** (decision, see §15): enter firmware setup (F2/Del at POST) → Security → Secure Boot → **Disabled**. *Requires physical/console access; do this while at the machine.*

**Also before first Linux boot:**
1. `git push` all uncommitted mev_bot work (repo must be clone-able from Linux).
2. Stop mev_bot daemon + cron/watchdog cleanly.
3. Record the current canonical config + git SHA from `ATTEMPTS.md`.

---

## 5. Step 1 — Shrink C: (Windows, ~10 min)

```powershell
diskpart
  list disk
  select disk 0            # the NVMe holding C:
  list volume
  select volume 3          # confirm it is C:
  shrink desired=2097152   # ~2TB (MiB)
  exit
```

> If `shrink` returns far less than 2TB (unmovable files), re-check §4 (hibernation/pagefile). If it still won't release ~2TB, **STOP and escalate** — do not force with third-party tools.

---

## 6. Step 2 — Install Ubuntu 24.04 LTS (dual-boot)

1. Download **Ubuntu 24.04.x LTS** desktop ISO → write USB with **Rufus** (GPT/UEFI).
2. Reboot → boot-menu (F8/Del) → select UEFI USB.
3. "Try/Install Ubuntu" → **"Something else"** (manual).
4. In the ~2TB unallocated space on `nvme0n1`, create:
   | Mount | Size | FS | Notes |
   |---|---|---|---|
   | `/` | 100GB | ext4 | OS + driver + venv + code |
   | `/training` | ~1.85TB | ext4 | training data + NVMe offload + checkpoints |
   | swap | 32GB | swap | optional with 256GB RAM (can use a 8GB swapfile post-install instead) |
5. **Boot loader:** existing ESP (`/dev/nvme0n1p1`) — adds GRUB next to Windows Boot Manager.
6. Reboot → GRUB menu → Ubuntu.

> Secure Boot is already OFF (§4), so the NVIDIA driver loads with no MOK enrollment.

---

## 7. Step 3 — NVIDIA driver + CUDA (Ubuntu, ~20 min)

Blackwell (`sm_120`) needs driver **R570+**; `nvidia-driver-580` is NOT in stock Ubuntu 24.04 repos (needs the PPA).

```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y gcc make build-essential
# Official graphics-drivers PPA (carries R580 for Blackwell workstation):
sudo add-apt-repository -y ppa:graphics-drivers/ppa
sudo apt update
sudo apt install -y nvidia-driver-580 nvidia-utils-580
sudo reboot

# Verify
nvidia-smi          # expect 3× RTX PRO 6000, 96GB, driver 580.x
nvidia-smi topo -m  # confirm 3 GPUs, PCIe (no NVLink) — topology for NCCL

# CUDA 12.8 toolkit (needed for DeepSpeed's nvcc JIT + future fused ops;
# not strictly required for CPU-Adam-only training, but recommended)
wget https://developer.download.nvidia.com/compute/cuda/12.8.1/local_installers/cuda-repo-ubuntu2404-12-8-local_12.8.1-1_amd64.deb
sudo dpkg -i cuda-repo-ubuntu2404-12-8-local_12.8.1-1_amd64.deb
sudo apt update && sudo apt install -y cuda-toolkit-12-8
```

---

## 8. Step 4 — Python training environment (Ubuntu)

```bash
sudo apt install -y python3.12 python3.12-venv python3-pip git curl
python3.12 -m venv ~/qwen-train
source ~/qwen-train/bin/activate
pip install --upgrade pip
pip install torch==2.9.1 --index-url https://download.pytorch.org/whl/cu128
pip install deepspeed==0.18.2 transformers==5.16.1 accelerate==1.11.0
pip install datasets tokenizers numpy tqdm tensorboard
```

> **Zero code change** — `train_qwen27b.py`, `accelerate_zero3.yaml`, and launch scripts port as-is (already CUDA 12.8 / Linux-idiomatic). Only the DeepSpeed JSON changes (§10).

---

## 9. Step 5 — Migrate data + code (~300MB)

```bash
git clone https://github.com/anonym0uxx/mev_bot.git ~/mev_bot
# (or, if GitHub is stale: mount D: read-only and copy:
#   sudo mkdir /mnt/d && sudo mount -t ntfs3 -o ro /dev/nvme1n1p1 /mnt/d
#   cp -r /mnt/d/repos/mev_bot ~/mev_bot)

sudo mkdir -p /training
sudo mount /dev/nvme0n1pX /training      # X = /training partition number
sudo mkdir -p /training/data /training/offload
cp -r ~/mev_bot/tools/data-pipeline/output/qwen_curriculum_v1 /training/data/
```

Verify:
```bash
df -h /training && du -sh /training/data
```

---

## 10. Step 6 — Verify memory, then the native DeepSpeed config

**First, measure the REAL number** (don't trust a rough 27B estimate):

```bash
source ~/qwen-train/bin/activate
python - <<'EOF'
from transformers import AutoConfig
c = AutoConfig.from_pretrained("unsloth/Qwen3.8-27B")
print("params:", round(sum(p for p in c.num_parameters)/1e9, 2), "B")
EOF
# Then DeepSpeed's exact estimator:
# deepspeed.runtime.zero.stage3.estimate_zero3_model_states_mem_needs_all_live(
#    model, num_gpus_per_node=3, num_nodes=1)
```

**Decision rule:** if the printed optimizer+grad+param footprint fits GPU and RAM, you may drop NVMe entirely; but for 27B + FP32 Adam it will not, so the safe target is **offload_optimizer = nvme, no offload_param**.

Write `ds_zero3_native.json` (surgical change from `ds_zero3_nvme.json` — **only `offload_param` and its swapper knobs are removed**):

```json
{
  "bf16": { "enabled": true },
  "zero_optimization": {
    "stage": 3,
    "offload_optimizer": {
      "device": "nvme",
      "nvme_path": "/training/offload",
      "pin_memory": false,
      "buffer_count": 4,
      "fast_init": false
    },
    "overlap_comm": false,
    "contiguous_gradients": false,
    "sub_group_size": 3e7,
    "reduce_bucket_size": 5e7,
    "stage3_gather_16bit_weights_on_model_save": true
  },
  "aio": {
    "block_size": 8388608,
    "queue_depth": 32,
    "single_submit": false,
    "overlap_events": true,
    "thread_count": 4
  },
  "gradient_clipping": 1.0,
  "train_batch_size": 24,
  "train_micro_batch_size_per_gpu": 1,
  "gradient_accumulation_steps": 8,
  "steps_per_print": 1
}
```

**What changed vs WSL2, and why:**
- **`offload_param` deleted** → params stay on GPU. This kills `PartitionedParamSwapper` — the source of every "buffer already assigned"/"not enough buffers" crash. With native 96GB (no dxg ceiling), params+grads+activations (~70–80GB/rank) fit.
- **swapper knobs deleted** (`stage3_max_live_parameters`, `stage3_max_reuse_distance`, `stage3_prefetch_bucket_size`, `stage3_param_persistence_threshold`) — these govern `offload_param` only; they're inert once param offload is gone.
- **`stage3_gather_16bit_weights_on_model_save: false → true`** — with params on GPU and 256GB RAM (no dxg ceiling), the full 54GB BF16 gather at save is cheap, and it produces a **directly-usable consolidated model** (no `zero_to_fp32.py` needed → simpler serve path §12).
- **batch geometry made explicit** (`train_batch_size` 24 / micro 1 / grad-accum 8) — removes the "GradientAccumulationPlugin has 1, DeepSpeed has 8" ambiguity we saw in the WSL log; 8 == 8, identical effective batch.
- **`overlap_comm`/`contiguous_gradients` left `false`** to match the proven-reduction behavior; both are safe to enable natively later for throughput (they were dxg holdovers), but stability-first for run #1.
- **`sub_group_size` kept `3e7`** (proven); can raise to `1e8`–`1e9` natively for fewer comm rounds once stable.

---

## 11. Step 7 — Launch + verify

```bash
source ~/qwen-train/bin/activate
cd ~/mev_bot/tools/training/qwen27b
# Point accelerate_zero3.yaml's deepspeed_config_file at ds_zero3_native.json,
# then launch exactly as today:
accelerate launch --config_file accelerate_zero3.yaml train_qwen27b.py --phase cpt 2>&1 | tee train.log
```

**Success gate (repeat of the WSL run):**
- baseline eval loss **6.3968** (deterministic)
- step 1: loss 6.706, grad_norm ~405, all 3 ranks finite
- GPU ~70–80GB/rank steady, **no param-swap warnings, no "buffer already assigned"**

---

## 12. Step 8 — Serve via Hermes on Windows

With `stage3_gather_16bit_weights_on_model_save: true`, each checkpoint already contains the **consolidated BF16 model** — no `zero_to_fp32.py` needed.

1. Locate the best checkpoint's consolidated weights (`pytorch_model.bin` / `model.safetensors` + `config.json`).
2. Copy that dir to a Windows-readable location (mount D: read-write, or an SMB/USB share).
3. Reboot → Windows → start mev_bot + cron/watchdog → point Hermes at the new weights dir.

(Optional: `zero_to_fp32.py <ckpt> <out>` yields FP32 master weights if you later want maximum precision at some inferential cost.)

---

## 13. Recurring cycle (until a dedicated training server exists)

```
Windows (live trading) ──stop bot──▶ reboot → Ubuntu (train N epochs)
      ▲                                              │
      └──reboot, start bot, load new weights ◀── copy best ckpt weights ──┘
```

Maintain `RUN.md` per cycle: git SHA, dataset manifest hash, model dir — same discipline as `ATTEMPTS.md`.

---

## 14. Risks & rollback

- **Shrink C: is the only irreversible step.** Back up critical C: content to D: first (cheap insurance with 3.19TB free). diskpart shrink is safe; re-extending later is possible.
- **GRUB/boot:** Ubuntu is additive; Windows is never deleted. If GRUB breaks, boot Windows via UEFI boot menu; `efibootmgr` fixes entries in 2 lines.
- **Rollback to WSL2:** the VHDX + bootstrap task stay intact on D: — boot Windows and resume WSL2 if native hits an unforeseen driver issue. Zero data loss.
- **NVIDIA driver:** Blackwell needs R570+; the PPA pin in §7 avoids the stale-default-driver trap.
- **PCIe comm (no NVLink):** 3-GPU all-reduce runs over PCIe. `nvidia-smi topo -m` (§7) confirms P2P; if P2P is unavailable NCCL falls back to host-staged (slower but correct). ZeRO-3 over PCIe for 3 GPUs is small-reduce, so throughput is acceptable; no special NCCL env is expected (drop the WSL `NCCL_CUMEM_ENABLE=0` / `TORCH_NCCL_AVOID_RECORD_STREAMS=1` dxg workarounds — they're harmless-but-not-needed natively).

---

## 15. Decisions locked (with rationale)

1. **Secure Boot = DISABLE.** Autonomous operation has no human at the console for the NVIDIA MOK password prompt; third-party DKMS modules load cleanly with SB off. Home-server threat model has no untrusted-physical-access exposure, so the SB security loss is negligible vs. the operational robustness gain.
2. **PCIe / no NVLink.** No NVLink bridge to plan; NCCL over PCIe; confirms ZeRO-3 (DP-sharded) over TP is correct strategy (no tensor-parallel NVLink assumption).
3. **`offload_optimizer: nvme` kept, `offload_param` dropped.** Grounded in the 324GB optimizer-size computation (§1); minimal, no-training-dynamics-change.

---

*Prepared 2026-09-01. Hardware measured live on DESKTOP-CP8N3IC. Model = Qwen3.8-27B (unsloth). Versions pinned to the current WSL stack for reproducibility. This revision corrects the earlier "no offload needed" assertion via the 324GB FP32-Adam optimizer computation.*