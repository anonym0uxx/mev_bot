#!/bin/bash
# RL chain hourly tick - escalation wrapper around rl_chain_trigger.py.
#
# Exit-code contract with the trigger:
#   0 -> the chain COMPLETED (ref cache built, joint-KL gate passed, verified, RL launched)
#   3 -> benign "still waiting" (builds incomplete, or SFT final weights not written yet)
#         SILENT BY DESIGN: this is the normal state for days, and paging on it would
#         train the operator to ignore the alarm.
#   2 -> genuine RL-side fault: notify + engage the assessing agent (see the runbook).
#
# CHAIN_NOTIFY_DRY=1 suppresses the Telegram send and the agent one-shot, for testing.
set -u
cd /training/v2/code/src/v2/rl || exit 1
PY=/home/alon/qwen27b-venv/bin/python
LOG=${RL_CHAIN_WRAPPER_LOG:-/training/v2/reports/rl_chain_hourly.log}
RUNBOOK=${RL_CHAIN_RUNBOOK:-/training/v2/reports/RL_CHAIN_RUNBOOK.md}
EVIDENCE=${RL_CHAIN_JKGATE:-/training/v2/reports/RL_JOINT_KL_GATE.json}
POLL=${RL_CHAIN_POLL_S:-60}
MAXWAIT=${RL_CHAIN_MAX_WAIT_S:-3300}
TRIGGER=${RL_CHAIN_TRIGGER:-rl_chain_trigger.py}
DRY=${CHAIN_NOTIFY_DRY:-0}
TOKEN=$(grep -m1 '^TELEGRAM_BOT_TOKEN=' /home/alon/.hermes/.env 2>/dev/null | cut -d= -f2-)
CHAT=${WATCHDOG_CHAT_ID:-5024153101}

tg() {
  if [ "$DRY" = "1" ]; then echo "[dry-run] telegram: $1" >> "$LOG"; return 0; fi
  [ -n "${TOKEN:-}" ] || { echo "no telegram token; suppressed: $1" >> "$LOG"; return 0; }
  curl -s -m 25 -X POST "https://api.telegram.org/bot${TOKEN}/sendMessage" \
    --data-urlencode "chat_id=${CHAT}" --data-urlencode "text=$1" >/dev/null 2>&1 || true
}

OUT=$($PY "$TRIGGER" --poll-s "$POLL" --max-wait-s "$MAXWAIT" 2>&1)
RC=$?
printf '=== %s rc=%s ===\n%s\n' "$(date '+%F %T %Z')" "$RC" "$OUT" >> "$LOG"

case "$RC" in
  0)
    tg "RL CHAIN COMPLETE: ref cache built, joint-KL gate PASSED on a real batch, readiness verified, RL launched (qwen-rl-001)."
    exit 0 ;;
  3)
    exit 0 ;;                      # benign waiting; silent
esac

# ---- rc=2: a genuine fault. Capture evidence, notify, and have the agent assess. ----
PROMPT="$(cat "$RUNBOOK")

CHAIN OUTPUT (this tick, rc=$RC):
${OUT}

JOINT-KL GATE EVIDENCE:
$(tail -c 2000 "$EVIDENCE" 2>/dev/null || echo 'no gate evidence file')"

if [ "$DRY" = "1" ]; then
  echo "[dry-run] would notify + engage agent; prompt bytes: ${#PROMPT}" >> "$LOG"
  exit 0
fi

REPLY=$(timeout 900 env HOME=/home/alon HERMES_HOME=/home/alon/.hermes \
  /home/alon/.hermes/hermes-agent/venv/bin/python -m hermes_cli.main -z "$PROMPT" 2>>"$LOG" || true)
printf 'AGENT REPLY: %s\n' "$REPLY" >> "$LOG"
if [ -n "$REPLY" ]; then
  tg "RL CHAIN FAULT (rc=$RC). Assessing agent: $REPLY"
else
  tg "RL chain fault (rc=$RC) and the assessing agent returned no reply - manual check needed. Evidence: $LOG and $EVIDENCE"
fi
exit 0
