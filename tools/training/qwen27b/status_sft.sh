#!/usr/bin/env bash
# Report current-attempt status for qwen27b SFT (runs inside WSL).
LOG=/training/runs/qwen27b_sft_v1/train.log
L=$(tr '\r' '\n' < "$LOG" | grep -nE '=== (launch|resume)' | tail -1 | cut -d: -f1)
tr '\r' '\n' < "$LOG" | tail -n +"$L" > /tmp/cur.log
echo "procs=$(pgrep -c -f 'train_qwen27b[.]py --phase sft')"
echo "errors=$(grep -cE 'Traceback|out of memory' /tmp/cur.log)"
# DETERMINISTIC step/eval signals (parsed from artifacts, NOT the tqdm bar).
CUR=$(grep -oE '[0-9]+/[0-9]+' /tmp/cur.log | tail -1)
echo "STEP=${CUR:-unknown}"
CK=$(ls -t /training/runs/qwen27b_sft_v1/checkpoint-*/trainer_state.json 2>/dev/null | head -1)
if [ -n "$CK" ]; then
  echo "LAST_CKPT_STEP=$(grep -oE '\"global_step\": [0-9]+' "$CK" | head -1 | grep -oE '[0-9]+')"
  echo "BEST_VAL=$(grep -oE '\"best_metric\": [0-9.]+' "$CK" | head -1 | grep -oE '[0-9.]+')"
fi
# Train-vs-val DIVERGENCE + val trajectory (the overfit tell).
python3 - <<'PY' 2>/dev/null
import json
try:
    lines = open("/training/runs/qwen27b_sft_v1/val_history.jsonl").read().strip().splitlines()
    recs = [json.loads(l) for l in lines if l.strip()]
    seen = {}
    for d in recs:
        seen[d["step"]] = d
    ordered = [seen[s] for s in sorted(seen)]
    print("VAL_TRAJ " + "  ".join("%s:%.3f" % (d["step"], d["val_loss"]) for d in ordered))
    last = ordered[-1]
    vl = last["val_loss"]; tl = last.get("train_loss_latest")
    if tl is not None:
        print("DIVERGENCE=train-val=%.4f (train %.4f vs val %.4f @ step %s)" % (tl - vl, tl, vl, last["step"]))
except Exception:
    pass
PY
# FD CANARY — READ-ONLY, reports only, NEVER acts (fd-leak bug: one fd per completed
# swap op). Operator visibility only; revive-on-death is the self-heal cron's job.
FD_MAX=$(for p in $(pgrep -f 'train_qwen27b[.]py --phase sft' 2>/dev/null); do
  ls /proc/"$p"/fd 2>/dev/null | wc -l
done | sort -n | tail -1)
echo "FD_MAX=${FD_MAX:-0}/1048576"
# GPU high-water mark (informational canary for the 2026-09-03 step-2 OOM class).
nvidia-smi --query-gpu=memory.used,memory.total --format=csv,noheader,nounits 2>/dev/null \
  | awk -F', ' 'BEGIN{m=0;t=0}{if($1>m)m=$1;t=$2}END{printf "GPU_PEAK_NOW=%d/%d MiB (%.0f%%)\n", m, t, (t>0?100*m/t:0)}'
nvidia-smi --query-gpu=memory.used,utilization.gpu --format=csv,noheader
grep -E 'eval_loss|loss.*:' /tmp/cur.log | tail -2
tail -3 /tmp/cur.log
