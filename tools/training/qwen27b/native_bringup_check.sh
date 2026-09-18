#!/usr/bin/env bash
# Read-only native host diagnostic for the Qwen27B DeepSpeed launch.
#
# CHECKS ONLY. It changes nothing: no mount, no format, no chown, no install, no
# launch. It prints the exact follow-up command for each unmet requirement.
# Re-run it any time; it is safe and idempotent.
#
#   bash native_bringup_check.sh
#
# Exit 0 = every precondition the launcher enforces is already satisfied.
# Exit 1 = at least one requirement is unmet; each is reported with its fix.
set -uo pipefail

FAIL=0
ok()   { printf '  [ OK ] %s\n' "$*"; }
bad()  { printf '  [FAIL] %s\n' "$*"; FAIL=1; }
warn() { printf '  [warn] %s\n' "$*"; }
hdr()  { printf '\n=== %s ===\n' "$*"; }

TRAINING_MOUNT="${TRAINING_MOUNT:-/training}"
DISK_SERIAL_EXPECTED="25375338A05C"
TRAINING_DISK_FORBIDDEN="253753246786"
RAM_FLOOR_BYTES=128000000000
DISK_FLOOR_BYTES=1500000000000
GPU_MEM_FLOOR_MIB=95000

# ---------------------------------------------------------------- host identity
hdr "host identity"
SYS=$(uname -s)
REL=$(uname -r)
echo "  system=$SYS release=$REL"
if [ "$SYS" != "Linux" ]; then
  bad "not Linux (got $SYS)"
elif printf '%s' "$REL" | grep -Eqi 'microsoft|wsl'; then
  bad "this is WSL ('$REL'); the launcher requires a native Linux boot"
else
  ok "native Linux"
fi
if [ -e /dev/dxg ]; then bad "/dev/dxg present -> WSL GPU passthrough"; else ok "no /dev/dxg"; fi

# ------------------------------------------------------------------------- GPUs
hdr "GPUs (need exactly 3, each >= ${GPU_MEM_FLOOR_MIB} MiB)"
if command -v nvidia-smi >/dev/null 2>&1; then
  nvidia-smi --query-gpu=index,uuid,name,memory.total,memory.used \
             --format=csv,noheader 2>/dev/null | sed 's/^/  /'
  NG=$(nvidia-smi --query-gpu=uuid --format=csv,noheader 2>/dev/null | grep -c .)
  [ "$NG" -eq 3 ] && ok "3 GPUs visible" || bad "expected 3 GPUs, saw $NG"
  LOW=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null \
        | awk -v f="$GPU_MEM_FLOOR_MIB" '$1 < f' | wc -l)
  [ "$LOW" -eq 0 ] && ok "every GPU meets the memory floor" || bad "$LOW GPU(s) below floor"
  BUSY=$(nvidia-smi --query-compute-apps=pid --format=csv,noheader 2>/dev/null | grep -c .)
  [ "$BUSY" -eq 0 ] && ok "no GPU compute processes" || bad "$BUSY GPU process(es) running"
  echo "  -> paste these into LAUNCH_CONTRACT_*.json as gpu_uuids:"
  nvidia-smi --query-gpu=uuid --format=csv,noheader 2>/dev/null | sed 's/^/       "/;s/$/",/'
else
  bad "nvidia-smi not found -> install the NVIDIA driver"
fi

# -------------------------------------------------------------------------- RAM
hdr "RAM (need >= ${RAM_FLOOR_BYTES} plus a 12% reserve of MemTotal)"
if [ -r /proc/meminfo ]; then
  TOTAL=$(awk '/^MemTotal:/{print $2*1024}' /proc/meminfo)
  AVAIL=$(awk '/^MemAvailable:/{print $2*1024}' /proc/meminfo)
  RESERVE=$(( (TOTAL * 12 + 99) / 100 ))
  echo "  total=$TOTAL available=$AVAIL reserve(12%)=$RESERVE"
  if [ "$AVAIL" -lt "$RAM_FLOOR_BYTES" ]; then
    bad "available RAM below the ${RAM_FLOOR_BYTES} floor"
  elif [ $((AVAIL - RAM_FLOOR_BYTES)) -lt "$RESERVE" ]; then
    bad "less than the 12% reserve would remain after the training budget"
  else
    ok "RAM headroom covers the budget plus the 12% reserve"
  fi
fi

# ------------------------------------------------------------- /training volume
hdr "$TRAINING_MOUNT (need writable ext4, FSROOT /, >= 1.5 TB free, no nested mounts)"
if findmnt -n "$TRAINING_MOUNT" >/dev/null 2>&1; then
  findmnt -n -o TARGET,SOURCE,FSTYPE,SIZE,AVAIL,UUID,OPTIONS "$TRAINING_MOUNT" | sed 's/^/  /'
  FSTYPE=$(findmnt -n -o FSTYPE "$TRAINING_MOUNT")
  UUID=$(findmnt -n -o UUID "$TRAINING_MOUNT")
  AVC=$(findmnt -n -o AVAIL -b "$TRAINING_MOUNT")
  OPTS=$(findmnt -n -o OPTIONS "$TRAINING_MOUNT")
  FSROOT=$(findmnt -n -o FSROOT "$TRAINING_MOUNT")
  [ "$FSTYPE" = "ext4" ] && ok "ext4" || bad "fstype is $FSTYPE, need ext4"
  [ "$FSROOT" = "/" ] && ok "fsroot is /" || bad "fsroot is $FSROOT, need /"
  printf '%s' "$OPTS" | tr ',' '\n' | grep -qx rw && ok "mounted rw" || bad "not mounted rw"
  printf '%s' "$OPTS" | tr ',' '\n' | grep -qx ro && bad "mounted ro" || true
  if [ -n "$UUID" ]; then
    ok "uuid=$UUID"
    echo "  -> paste into LAUNCH_CONTRACT_*.json as mount_uuid: \"$UUID\""
  else
    bad "no filesystem UUID"
  fi
  [ "${AVC:-0}" -ge "$DISK_FLOOR_BYTES" ] && ok ">= 1.5 TB free" \
    || bad "only $((AVC/1000000000)) GB free, floor is 1500 GB"
  NESTED=$(findmnt -n -o TARGET --submounts "$TRAINING_MOUNT" 2>/dev/null | tail -n +2 | grep -c .)
  [ "$NESTED" -eq 0 ] && ok "no nested mounts beneath it" || bad "$NESTED nested mount(s) beneath it"
else
  bad "$TRAINING_MOUNT is not mounted"
  echo "  candidates (pick the ~1912 GiB partition on the training disk):"
  lsblk -o NAME,SIZE,TYPE,FSTYPE,LABEL,UUID,MOUNTPOINT 2>/dev/null | sed 's/^/    /'
  echo "  fix:  sudo e2label /dev/nvme0n1p8 training"
  echo "        sudo mount LABEL=training $TRAINING_MOUNT && sudo chown -R $(id -un):$(id -gn) $TRAINING_MOUNT"
fi

# -------------------------------------------------------------- training disk
hdr "training disk serial (must be $DISK_SERIAL_EXPECTED; $TRAINING_DISK_FORBIDDEN is the Data disk)"
for D in /sys/block/nvme*n1/device/serial; do
  [ -r "$D" ] || continue
  S=$(cat "$D" 2>/dev/null)
  N=$(basename "$(dirname "$(dirname "$D")")")
  printf '  %s serial=%s\n' "$N" "$S"
  case "$S" in
    *"$DISK_SERIAL_EXPECTED") ok "$N is the operator-pinned training disk" ;;
    *"$TRAINING_DISK_FORBIDDEN") warn "$N is the Data disk - never a training target" ;;
  esac
done

# -------------------------------------------------------------------- python env
hdr "native Python environment (torch + deepspeed)"
PY="${PY:-python3}"
if command -v "$PY" >/dev/null 2>&1; then
  echo "  $PY -> $($PY -V 2>&1)"
  "$PY" - <<'EOF' 2>/dev/null || bad "import check failed (torch/deepspeed missing in this interpreter)"
import sys
try:
    import torch
    print("  torch", torch.__version__, "cuda", torch.cuda.is_available(),
          "devices", torch.cuda.device_count())
    assert torch.cuda.is_available() and torch.cuda.device_count() == 3, "need 3 CUDA devices"
except Exception as exc:
    print("  [FAIL] torch:", exc); sys.exit(1)
try:
    import deepspeed
    print("  deepspeed", deepspeed.__version__)
except Exception as exc:
    print("  [FAIL] deepspeed:", exc); sys.exit(1)
try:
    import transformers, accelerate
    print("  transformers", transformers.__version__, "accelerate", accelerate.__version__)
except Exception as exc:
    print("  [FAIL] transformers/accelerate:", exc); sys.exit(1)
EOF
else
  bad "$PY not found"
fi

# ------------------------------------------------------------------------ seed
hdr "clean seed on the native filesystem"
REV=3ea932cee0a432ae86e9c7826cbe8aef52323a28
SEED=/training/seed/models--unsloth--Qwen3.8-27B/snapshots/$REV
for CAND in "$SEED"; do
  if [ -d "$CAND" ]; then
    N=$(ls -1 "$CAND"/model-*-of-*.safetensors 2>/dev/null | wc -l)
    SZ=$(du -sb "$CAND" 2>/dev/null | cut -f1)
    IDX=$([ -f "$CAND/model.safetensors.index.json" ] && echo yes || echo NO)
    echo "  $CAND shards=$N index=$IDX bytes=$SZ"
    [ "$N" -eq 18 ] && [ "$IDX" = yes ] && ok "seed looks complete" || bad "seed incomplete"
  else
    warn "$CAND absent"
  fi
done
SRC=/mnt/data/mev_bot-artifacts/qwen-seed/models--unsloth--Qwen3.8-27B/snapshots/$REV
echo "  transfer source (Data disk, already sha256-verified against the receipt):"
echo "    $SRC"
echo "  fix:  sudo mkdir -p /mnt/data && sudo mount -o ro /dev/nvme1n1p1 /mnt/data"
echo "        mkdir -p /training/seed/models--unsloth--Qwen3.8-27B/snapshots"
echo "        cp -r \"$SRC\" /training/seed/models--unsloth--Qwen3.8-27B/snapshots/"
echo "        python3 verify_clean_seed.py --seed $SEED --out /training/seed/CLEAN_SEED_RECEIPT.json"
echo "  IMPORTANT: the directory basename MUST stay $REV -- verify_clean_seed.py ties the"
echo "  seed to the pinned revision by directory name as well as by digest, so copying into"
echo "  a differently-named folder fails verification even when the bytes are perfect."

# ---------------------------------------------------------------------- verdict
hdr "verdict"
if [ "$FAIL" -eq 0 ]; then
  echo "  all launcher preconditions satisfied - proceed to contract fill + gate evidence"
else
  echo "  unmet requirements above. Nothing was changed by this script."
fi
exit "$FAIL"
