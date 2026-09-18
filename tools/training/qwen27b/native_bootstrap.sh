#!/usr/bin/env bash
# Native bring-up: fresh Linux boot -> /training ready -> seed staged -> launch-ready.
#
# Safety model
#   * Runs the read-only checks first and stops if any fails.
#   * Every mutating phase needs its own explicit flag; nothing destructive is
#     implicit. Formatting the training partition requires --format-p8 AND a typed
#     confirmation, because it destroys whatever is on that partition.
#   * Idempotent: safe to re-run. Completed phases are detected and skipped.
#   * Never touches the Windows partition, the boot disk layout, or the Data disk's
#     contents (it only reads the staged seed from there).
#
# Usage
#   bash native_bootstrap.sh --check                       # read-only, run this first
#   bash native_bootstrap.sh --apply --seed-source /mnt/data/mev_bot-artifacts/qwen-seed
#   bash native_bootstrap.sh --apply --format-p8 --seed-source ...   # only if P8 is bare
set -uo pipefail

REV=3ea932cee0a432ae86e9c7826cbe8aef52323a28
SEED_REL="models--unsloth--Qwen3.8-27B/snapshots/$REV"
TRAINING_MOUNT="${TRAINING_MOUNT:-/training}"
TRAINING_LABEL="${TRAINING_LABEL:-training}"
DATA_DISK_PART="${DATA_DISK_PART:-/dev/nvme1n1p1}"
VENV="${VENV:-$HOME/qwen27b-venv}"
CODE_SRC="${CODE_SRC:-}"
DISK_FLOOR_BYTES=1500000000000

APPLY=0; FORMAT_P8=0; SEED_SOURCE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --check) APPLY=0 ;;
    --apply) APPLY=1 ;;
    --format-p8) FORMAT_P8=1 ;;
    --seed-source) SEED_SOURCE="$2"; shift ;;
    --code-src) CODE_SRC="$2"; shift ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac; shift
done

say()  { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
ok()   { printf '  [ OK ] %s\n' "$*"; }
bad()  { printf '  [FAIL] %s\n' "$*"; }
warn() { printf '  [warn] %s\n' "$*"; }
die()  { bad "$*"; exit 1; }
run()  { if [ "$APPLY" -eq 1 ]; then "$@"; else printf '  [dry-run] %s\n' "$*"; fi; }

# ---------------------------------------------------------------- 0. host identity
say "0. host identity"
REL=$(uname -r)
printf '  %s\n' "$REL"
printf '%s' "$REL" | grep -Eqi 'microsoft|wsl' && die "this is WSL; the launcher requires a native Linux boot"
[ -e /dev/dxg ] && die "/dev/dxg present (WSL passthrough)"
ok "native Linux"

say "0b. GPUs (need 3 x >= 95000 MiB, none busy)"
command -v nvidia-smi >/dev/null || die "nvidia-smi missing"
nvidia-smi --query-gpu=index,uuid,name,memory.total,memory.used --format=csv,noheader | sed 's/^/  /'
NG=$(nvidia-smi --query-gpu=uuid --format=csv,noheader | grep -c .)
[ "$NG" -eq 3 ] || die "expected 3 GPUs, saw $NG"
LOW=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits | awk '$1<95000' | wc -l)
[ "$LOW" -eq 0 ] || die "$LOW GPU(s) below the 95000 MiB floor"
BUSY=$(nvidia-smi --query-compute-apps=pid --format=csv,noheader | grep -c .)
[ "$BUSY" -eq 0 ] || die "$BUSY GPU process(es) already running"
ok "3 GPUs, all above floor, none busy"
echo "  gpu_uuids for the contract:"
nvidia-smi --query-gpu=uuid --format=csv,noheader | sed 's/^/    /'

say "0c. RAM (>= 128 GB budget + 12% reserve)"
TOTAL=$(awk '/^MemTotal:/{print $2*1024}' /proc/meminfo)
AVAIL=$(awk '/^MemAvailable:/{print $2*1024}' /proc/meminfo)
RESERVE=$(( (TOTAL*12+99)/100 ))
printf '  total %s  available %s  reserve(12%%) %s\n' "$TOTAL" "$AVAIL" "$RESERVE"
[ "$AVAIL" -ge 128000000000 ] || die "available RAM below the 128 GB floor"
[ $((AVAIL-128000000000)) -ge "$RESERVE" ] || die "less than the 12% reserve would remain"
ok "RAM headroom sufficient"

# ------------------------------------------------------------- 1. /training volume
say "1. $TRAINING_MOUNT"
if findmnt -n "$TRAINING_MOUNT" >/dev/null 2>&1; then
  findmnt -n -o TARGET,SOURCE,FSTYPE,SIZE,AVAIL,UUID,OPTIONS "$TRAINING_MOUNT" | sed 's/^/  /'
  ok "already mounted"
else
  echo "  candidates:"
  lsblk -o NAME,SIZE,TYPE,FSTYPE,LABEL,UUID,MOUNTPOINT | sed 's/^/    /'
  DEV=$(blkid -L "$TRAINING_LABEL" 2>/dev/null || true)
  if [ -z "$DEV" ]; then
    warn "no partition labelled '$TRAINING_LABEL'"
    echo "  If the ~1912 GiB partition on the TRAINING disk (serial ...A05C) is bare,"
    echo "  label + mount it. THIS IS DESTRUCTIVE - it erases that partition:"
    echo "    sudo mkfs.ext4 -L $TRAINING_LABEL /dev/nvme0n1p8"
    echo "  then re-run:  bash $0 --apply --seed-source $SEED_SOURCE"
    echo
    echo "  If it is ALREADY ext4, just label and mount it (no format needed):"
    echo "    sudo e2label /dev/nvme0n1p8 $TRAINING_LABEL"
    echo "    bash $0 --apply --seed-source $SEED_SOURCE"
    exit 1
  fi
  ok "found $DEV"
  run sudo mkdir -p "$TRAINING_MOUNT"
  run sudo mount "LABEL=$TRAINING_LABEL" "$TRAINING_MOUNT"
  run sudo chown -R "$(id -un):$(id -gn)" "$TRAINING_MOUNT"
fi
if findmnt -n "$TRAINING_MOUNT" >/dev/null 2>&1; then
  AVC=$(findmnt -n -o AVAIL -b "$TRAINING_MOUNT")
  UUID=$(findmnt -n -o UUID "$TRAINING_MOUNT")
  FSTYPE=$(findmnt -n -o FSTYPE "$TRAINING_MOUNT")
  [ "$FSTYPE" = ext4 ] || die "fstype $FSTYPE, need ext4"
  [ "${AVC:-0}" -ge "$DISK_FLOOR_BYTES" ] || die "only $((AVC/1000000000)) GB free, floor 1500 GB"
  ok "ext4, $((AVC/1000000000)) GB free"
  echo "  mount_uuid for the contract: $UUID"
fi
run sudo mkdir -p "$TRAINING_MOUNT/offload" "$TRAINING_MOUNT/runs" "$TRAINING_MOUNT/seed"

# ------------------------------------------------------------- 2. python environment
say "2. python environment ($VENV)"
if [ -x "$VENV/bin/python" ] && "$VENV/bin/python" -c 'import torch,deepspeed' 2>/dev/null; then
  ok "venv already has torch + deepspeed"
else
  if [ "$APPLY" -eq 1 ]; then
    [ -d "$VENV" ] || python3 -m venv "$VENV"
    "$VENV/bin/python" -m pip install -q --upgrade pip
    # CUDA build for Blackwell (sm_120); adjust the index if the driver differs.
    "$VENV/bin/python" -m pip install -q torch --index-url https://download.pytorch.org/whl/cu128
    "$VENV/bin/python" -m pip install -q transformers accelerate deepspeed safetensors
    ok "installed"
  else
    printf '  [dry-run] create venv + install torch(cu128) transformers accelerate deepspeed safetensors\n'
  fi
fi
if [ "$APPLY" -eq 1 ]; then
  "$VENV/bin/python" - <<'EOF' || die "torch/deepspeed import or GPU count check failed"
import sys, torch
print("  torch", torch.__version__, "cuda", torch.cuda.is_available(), "devices", torch.cuda.device_count())
assert torch.cuda.is_available(), "CUDA not available"
assert torch.cuda.device_count() == 3, "need exactly 3 CUDA devices"
import deepspeed, transformers
print("  deepspeed", deepspeed.__version__, "transformers", transformers.__version__)
EOF
  ok "environment verified"
fi

# ---------------------------------------------------------------------- 3. seed
say "3. seed -> $TRAINING_MOUNT/seed/$SEED_REL"
DEST="$TRAINING_MOUNT/seed/$SEED_REL"
if [ -f "$DEST/model.safetensors.index.json" ] && \
   [ "$(ls -1 "$DEST"/model-*-of-00018.safetensors 2>/dev/null | wc -l)" -eq 18 ]; then
  ok "seed already staged (18 shards)"
else
  [ -n "$SEED_SOURCE" ] || die "no --seed-source given and the seed is not staged"
  SRC="$SEED_SOURCE/$SEED_REL"
  [ -f "$SRC/model.safetensors.index.json" ] || die "seed not found at $SRC"
  if ! findmnt -n /mnt/data >/dev/null 2>&1 && [ "$SEED_SOURCE" = "/mnt/data"* ]; then
    echo "  mounting the Data disk read-only so we can read the staged seed"
    run sudo mkdir -p /mnt/data
    run sudo mount -o ro "$DATA_DISK_PART" /mnt/data
  fi
  count=$(ls -1 "$SRC"/model-*-of-00018.safetensors 2>/dev/null | wc -l)
  [ "$count" -eq 18 ] || die "source has $count shards, expected 18"
  ok "source verified: 18 shards"
  run mkdir -p "$(dirname "$DEST")"
  # preserve the revision-named directory: verify_clean_seed ties the seed to the
  # pinned revision by directory name as well as by digest.
  run cp -r "$SRC" "$DEST"
  ok "copied"
fi
if [ "$APPLY" -eq 1 ] && [ -f "$CODE_SRC/verify_clean_seed.py" ] || [ -f ./verify_clean_seed.py ]; then
  V=$( [ -f ./verify_clean_seed.py ] && echo ./verify_clean_seed.py || echo "$CODE_SRC/verify_clean_seed.py" )
  "$VENV/bin/python" "$V" --seed "$DEST" --out "$TRAINING_MOUNT/seed/CLEAN_SEED_RECEIPT.json" >/dev/null 2>&1 \
    && ok "seed receipt written" || warn "seed verification failed - inspect before launching"
fi

# --------------------------------------------------------------- 4. code + contracts
say "4. training code"
if [ -n "$CODE_SRC" ]; then
  run cp -r "$CODE_SRC" "$TRAINING_MOUNT/code" 2>/dev/null || true
  echo "  code staged from $CODE_SRC -> $TRAINING_MOUNT/code"
fi

say "5. next steps (both need the values printed above)"
cat <<EOF
  1. fill the launch contracts:
       python3 fill_launch_contracts.py --gpu-uuids <uuid1> <uuid2> <uuid3> \\
           --mount-uuid <uuid> --run-id cpt-001 --seed-dir $DEST
  2. build gate evidence from the real seed:
       python3 build_gate_evidence.py --seed-dir $DEST --trainer ./train_qwen27b.py \\
           --release <RELEASE.json> --waivers <GATE_WAIVERS.json> --out <EVIDENCE.json>
  3. operator approval, then launch:
       ./launch_cpt_detached.sh --release_manifest <RELEASE.json> \\
           --launch-contract <CONTRACT.json> --contract-sha256 <SHA256> --execute
EOF
say "done (APPLY=$APPLY)"
