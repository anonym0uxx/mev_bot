#!/usr/bin/env bash
# Report current-attempt status for qwen27b CPT (runs inside WSL).
LOG=/training/runs/qwen27b_cpt_v1/train.log
L=$(tr '\r' '\n' < "$LOG" | grep -nE '=== (launch|resume)' | tail -1 | cut -d: -f1)
tr '\r' '\n' < "$LOG" | tail -n +"$L" > /tmp/cur.log
echo "procs=$(pgrep -c -f 'train_qwen27b[.]py --phase cpt')"
echo "errors=$(grep -cE 'Traceback|out of memory' /tmp/cur.log)"
# DETERMINISTIC step/eval signals (parsed from artifacts, NOT the tqdm bar —
# so the watchdog agent cannot hallucinate a step number):
CUR=$(grep -oE '[0-9]+/[0-9]+' /tmp/cur.log | tail -1)
echo "STEP=${CUR:-unknown}"
CK=$(ls -t /training/runs/qwen27b_cpt_v1/checkpoint-*/trainer_state.json 2>/dev/null | head -1)
if [ -n "$CK" ]; then
  echo "LAST_CKPT_STEP=$(grep -oE '\"global_step\": [0-9]+' "$CK" | head -1 | grep -oE '[0-9]+')"
  echo "BEST_VAL=$(grep -oE '\"best_metric\": [0-9.]+' "$CK" | head -1 | grep -oE '[0-9.]+')"
fi
# Train-vs-val DIVERGENCE + val trajectory (the overfit tell: gap opening / plateau).
python3 - <<'PY' 2>/dev/null
import json
try:
    lines = open("/training/runs/qwen27b_cpt_v1/val_history.jsonl").read().strip().splitlines()
    recs = [json.loads(l) for l in lines if l.strip()]
    seen = {}
    for d in recs:
        seen[d["step"]] = d          # dedupe restarts (step-0/23 can repeat)
    ordered = [seen[s] for s in sorted(seen)]
    print("VAL_TRAJ " + "  ".join("%s:%.3f" % (d["step"], d["val_loss"]) for d in ordered))
    last = ordered[-1]
    vl = last["val_loss"]; tl = last.get("train_loss_latest")
    if tl is not None:
        print("DIVERGENCE=train-val=%.4f (train %.4f vs val %.4f @ step %s)" % (tl - vl, tl, vl, last["step"]))
except Exception:
    pass
PY
# FD CANARY — READ-ONLY, reports only, NEVER acts. DeepSpeed's aio leaks one fd per
# completed swap op (deepspeed_py_io_handle.cpp:213 missing close()), so each rank's
# open-fd count climbs ~10k/step and that rank asserts at the 1048576 soft cap (~103
# steps of uptime). This line surfaces the margin for OPERATOR VISIBILITY ONLY; the
# revive-on-death is handled by the self-heal cron (cpt_selfheal.py), never here.
# No threshold, no action — just `ls` + count.
FD_MAX=$(for p in $(pgrep -f 'train_qwen27b[.]py --phase cpt' 2>/dev/null); do
  ls /proc/"$p"/fd 2>/dev/null | wc -l
done | sort -n | tail -1)
echo "FD_MAX=${FD_MAX:-0}/1048576"
nvidia-smi --query-gpu=memory.used,utilization.gpu --format=csv,noheader
grep -E 'eval_loss|loss.*:' /tmp/cur.log | tail -2
tail -3 /tmp/cur.log
