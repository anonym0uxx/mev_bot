#!/usr/bin/env bash
# Idempotent patch for DeepSpeed 0.18.2 NVMe-offload checkpoint-save bug.
#
# Root cause: deepspeed/runtime/engine.py save_checkpoint copies the NVMe
# optimizer swap folder into the checkpoint via
#   copytree(offload_dir, offload_ckpt_dir, ..., dirs_exist_ok=False)
# but _create_zero_checkpoint_files() has ALREADY created the destination
# offloaded_tensors/rankXX dir, so copytree raises FileExistsError and kills
# the run at the first checkpoint save (attempts #22 and #25).
# The LOAD path (engine.py line ~3172) already uses dirs_exist_ok=True.
# Fix: make the save path idempotent to match. dirs_exist_ok=True means
# copytree merges into the existing dir instead of refusing to start.
set -euo pipefail
ENGINE=$(python -c "import deepspeed.runtime.engine as e; print(e.__file__)" 2>/dev/null || true)
if [ -z "$ENGINE" ]; then
  echo "patch_deepspeed_save: deepspeed not importable (skipping)"
  exit 0
fi
if grep -q "dirs_exist_ok=False" "$ENGINE"; then
  sed -i 's/dirs_exist_ok=False/dirs_exist_ok=True/g' "$ENGINE"
  echo "patch_deepspeed_save: PATCHED $ENGINE (dirs_exist_ok=False -> True)"
else
  echo "patch_deepspeed_save: already patched ($ENGINE)"
fi
