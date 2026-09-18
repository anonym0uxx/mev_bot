#!/usr/bin/env bash
# Watch attempt 3 through the step-5/6 window where attempt 2 died (NCCL allgather
# stall). Exit 0 if it clears step 6 (stall likely transient); exit 1 on death.
LOG=/training/runs/qwen27b_sft_v1/train.log
# count logged losses in the CURRENT (latest launch) segment only
SEG=$(grep -n '^=== launch' "$LOG" | tail -1 | cut -d: -f1)
for i in $(seq 1 90); do
  P=$(pgrep -c -f 'train_qwen27b[.]py --phase sft' 2>/dev/null || echo 0)
  SEGNOW=$(grep -n '^=== launch' "$LOG" | tail -1 | cut -d: -f1)
  if [ "$SEGNOW" != "$SEG" ]; then echo "NEW LAUNCH detected (attempt >3): self-heal fired again."; exit 2; fi
  L=$(tr '\r' '\n' < "$LOG" | sed -n "${SEG},$ p" | grep -c "'loss'")
  if [ "$P" -lt 3 ]; then echo "DEAD: procs=$P losses_in_attempt3=$L"; exit 1; fi
  if [ "$L" -ge 6 ]; then
    echo "CLEARED step-6 window: procs=$P losses=$L (stall likely transient)"
    tr '\r' '\n' < "$LOG" | sed -n "${SEG},$ p" | grep "'loss'" | tail -3; exit 0
  fi
  sleep 30
done
echo "TIMEOUT (45min): procs=$P losses=$L still alive, just slow"; exit 3
