#!/usr/bin/env bash
# V2 rebuild with the value-leg floor. Clean labels, then assemble + verify.
set -euo pipefail
cd /training/v2
PY=/home/alon/qwen27b-venv/bin/python
S=code/src/v2
LOG=reports/rebuild_chain.log
: > "$LOG"
run() { echo "=== $* ===" >> "$LOG"; "$@" >> "$LOG" 2>&1; }

run $PY $S/build_states_v2.py --root canonical/renormalized_v7 \
    --split-manifest reports/SPLIT_MANIFEST_V7.json --out canonical/ledger_v7
run $PY $S/labels.py --states canonical/ledger_v7/states.jsonl --out canonical/ledger_v7
run $PY $S/serialize_families.py --states canonical/ledger_v7/states.jsonl \
    --labels canonical/ledger_v7/labels.jsonl --out candidate_v7max \
    --max-per-mint 100000 --families decision,utility --utility-every 8
run $PY $S/build_replay_v2.py --out candidate_v7mgmt --splits train,val,test --lam 2.0
run $PY $S/build_value_families.py --out candidate_v7edge --splits train,val,test
run $PY $S/economic_exam.py --split test
run $PY $S/merge_final_v2.py
run $PY $S/loss_budget.py --candidate candidate_sft_final
run $PY $S/candidate_guard.py --candidate candidate_sft_final
run $PY $S/package_release.py --candidate candidate_sft_final
run $PY $S/preflight_sft.py --candidate candidate_sft_final
echo "=== CHAIN COMPLETE ===" >> "$LOG"
