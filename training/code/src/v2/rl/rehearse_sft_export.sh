#!/usr/bin/env bash
# Rehearsal: prove the SFT best checkpoint can become a loadable RL init.
# Reads the LIVE checkpoint-1777 (already complete on disk) and exports a
# consolidated BF16 HF dir into a scratch location. Safe while training runs.
cd /training/v2/code/src/v2/rl || exit 1
OUT=/training/runs/sft-013/_handoff_rehearsal_bf16
echo "=== rehearsal start $(date -Is) ==="
/home/alon/qwen27b-venv/bin/python sft_checkpoint_handoff.py export --out "$OUT"
rc=$?
echo "=== export rc=$rc $(date -Is) ==="
if [ $rc -eq 0 ]; then
  ls -la "$OUT" | head -12
  du -sh "$OUT"
  echo "=== verifying the exported dir is classification-loadable ==="
  /home/alon/qwen27b-venv/bin/python -c "
import sys; sys.path.insert(0,'/training/v2/code/src/v2/rl')
from sft_checkpoint_handoff import classify_artifact
import json; print(json.dumps(classify_artifact('$OUT'), indent=1))
"
fi
echo "=== rehearsal end $(date -Is) ==="