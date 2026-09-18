#!/usr/bin/env bash
# Wait until SFT passes step 2 (where attempt 1 OOM'd) or dies. Bounded ~70 min.
LOG=/training/runs/qwen27b_sft_v1/train.log
for i in $(seq 1 140); do
  P=$(pgrep -c -f 'train_qwen27b[.]py --phase sft')
  L=$(tr '\r' '\n' < "$LOG" | grep -oE "'loss': '[0-9.]+'" | wc -l)
  PK=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | sort -n | tail -1)
  if [ "$P" -lt 3 ]; then
    echo "DEAD procs=$P after $L logged steps. tail:"; tr '\r' '\n' < "$LOG" | grep -E 'Error|error|Traceback' | tail -5; exit 1
  fi
  if [ "$L" -ge 2 ]; then
    echo "PASSED step-2 gate: procs=$P logged_steps=$L gpu_peak_now=${PK}MiB"
    tr '\r' '\n' < "$LOG" | grep -E "'loss'" | tail -2; exit 0
  fi
  sleep 30
done
echo "TIMEOUT: procs=$P logged_steps=$L gpu_peak=${PK}MiB (still alive, just slow)"; exit 2
