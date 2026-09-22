#!/bin/bash
# SFT hourly watchdog - INDEPENDENT of the Hermes cron dispatcher.
# The run it watches comes from SFT_WATCHDOG_UNIT/RUN_ROOT (systemd unit env), and every
# message names THAT run: an alert naming the wrong run is worse than no alert.
#
# WHY: the Hermes cron path dispatches "restart-safe" workers via
# `systemd-run --user --scope`, which needs XDG_RUNTIME_DIR in the GATEWAY's
# environment. This host had no user systemd instance at all, and sd-bus has no
# fallback (proven: it fails with the var absent even though /run/user/1000/bus
# exists). Fixing the gateway needs a restart - so this monitor deliberately does
# not depend on it: a SYSTEM timer needs no user bus.
#
# Behaviour: quiet when HEALTHY (full audit trail in the log). On a fault it engages
# the agent in one-shot mode to ASSESS and FIX, then sends the agent's own report to
# Telegram. On COMPLETE it says so once.
set -u
cd /training/v2/code/src/v2/rl || exit 1
PY=/home/alon/qwen27b-venv/bin/python
UNIT_NM=${SFT_WATCHDOG_UNIT:-qwen-sft-006}
LOG=${SFT_WATCHDOG_LOG:-/training/v2/reports/sft_watchdog_hourly.log}
RUNBOOK=${SFT_WATCHDOG_RUNBOOK:-/training/v2/reports/SFT_WATCHDOG_RUNBOOK.md}
TOKEN=$(grep -m1 '^TELEGRAM_BOT_TOKEN=' /home/alon/.hermes/.env 2>/dev/null | cut -d= -f2-)
CHAT=${WATCHDOG_CHAT_ID:-5024153101}

tg() {
  [ -n "${TOKEN:-}" ] || { echo "no telegram token; message suppressed: $1" >> "$LOG"; return 0; }
  curl -s -m 25 -X POST "https://api.telegram.org/bot${TOKEN}/sendMessage" \
    --data-urlencode "chat_id=${CHAT}" --data-urlencode "text=$1" >/dev/null 2>&1 || true
}

OUT=$($PY sft_watchdog.py 2>&1)
printf '%s\n%s\n' "=== $(date '+%F %T %Z') ===" "$OUT" >> "$LOG"
STATUS=$(printf '%s' "$OUT" | $PY -c "import sys,json
try: print(json.load(sys.stdin).get('status','UNKNOWN'))
except Exception: print('UNKNOWN')" 2>/dev/null)
[ -z "$STATUS" ] && STATUS=UNKNOWN

case "$STATUS" in
  HEALTHY) exit 0 ;;
  COMPLETE)
    tg "$UNIT_NM COMPLETE: best-validation weights written. qwen-rl-chain will now build the ref cache, verify, and launch RL automatically."
    exit 0 ;;
esac

PROMPT="$(cat "$RUNBOOK")

WATCHDOG EVIDENCE (this tick):
${OUT}"
REPLY=$(timeout 900 env HOME=/home/alon HERMES_HOME=/home/alon/.hermes \
  /home/alon/.hermes/hermes-agent/venv/bin/python -m hermes_cli.main -z "$PROMPT" 2>>"$LOG" || true)
printf 'AGENT REPLY: %s\n' "$REPLY" >> "$LOG"
if [ -n "$REPLY" ]; then
  tg "$REPLY"
else
  tg "$UNIT_NM ${STATUS} detected, but the assessing agent returned no reply. Manual check needed. Evidence: ${LOG}"
fi
exit 0
