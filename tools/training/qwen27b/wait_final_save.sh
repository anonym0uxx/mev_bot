#!/usr/bin/env bash
# Wait for the CPT final save to complete, then report the state. Runs inside WSL.
# Completion marker: train_qwen27b.py prints "DONE cpt ..." after save_model(final)
# + tok.save_pretrained(final). Polls up to ~45 min.
set -u
LOG=/training/runs/qwen27b_cpt_v1/train.log
FINAL=/training/runs/qwen27b_cpt_v1/final
DONE=0
for i in $(seq 1 45); do
  if grep -q "DONE cpt" "$LOG" 2>/dev/null; then
    DONE=1
    break
  fi
  sleep 60
done
echo "=== CPT FINAL SAVE STATE ==="
echo "done_marker=$DONE  (1 = 'DONE cpt' present)"
echo "procs=$(pgrep -c -f 'train_qwen27b[.]py --phase cpt' 2>/dev/null)"
echo "final_size=$(du -sh "$FINAL" 2>/dev/null | cut -f1)"
echo "fd_max=$(for p in $(pgrep -f train_qwen27b 2>/dev/null); do ls /proc/$p/fd 2>/dev/null | wc -l; done | sort -n | tail -1)"
echo "--- final/ contents ---"
ls -la "$FINAL" 2>/dev/null | head -40
echo "--- last log lines ---"
tr '\r' '\n' < "$LOG" | tail -5