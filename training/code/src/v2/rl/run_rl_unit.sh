#!/usr/bin/env bash
# RL phase unit. Runs in its OWN systemd cgroup (launched from an agent shell the
# trainer would inherit hermes-gateway.service's cgroup, and a trainer OOM kills
# the gateway - this is a known hazard, not a theory).
#
# Resume, never restart: launch_rl.py detects existing checkpoints and passes
# --mode resume. The loop skips completed prompts by prompt_sha256.
set -u
cd /training/v2/code/src/v2/rl
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True
export OMP_NUM_THREADS=8

STAGE=/training/runs/rl-001
mkdir -p "$STAGE"

# 1. gate: refuse to start anything if a prerequisite is wrong
/home/alon/qwen27b-venv/bin/python rl_plan.py --json "$STAGE/rl_plan.json" \
  > "$STAGE/rl_plan.log" 2>&1
if [ $? -ne 0 ]; then
  echo "rl_plan BLOCKED - see $STAGE/rl_plan.log" >&2
  exit 2
fi

# 2. launch (writes launch_receipt.json + run_identity via launch_rl.py)
/home/alon/qwen27b-venv/bin/python launch_rl.py \
  --config configs/rl_v1.json --execute > "$STAGE/launch.log" 2>&1
ec=$?
if [ $ec -ne 0 ]; then echo "launcher exited $ec; not holding unit" >&2; exit $ec; fi

# 3. hold the unit until the trainer is gone, so systemd sees the real lifetime
sleep 20
while pgrep -f 'grpo_loop\.py' >/dev/null 2>&1; do sleep 60; done
echo "rl trainer gone; unit exiting"
exit 0
