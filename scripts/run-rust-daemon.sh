#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

BINARY="./rust/target/release/pump-quant"
LOG="logs/rust-daemon.log"
PIDFILE="data/pump-quant.pid"
RESTART_DELAY=3
MAX_RESTARTS=10

mkdir -p logs data

# Load env
if [ -f "./rust/.env" ]; then
  set -a && source ./rust/.env && set +a
fi

# ── ENFORCE SINGLE DAEMON — kill all duplicates before starting ───────────────
bash "$SCRIPT_DIR/ensure-single-daemon.sh"
# ─────────────────────────────────────────────────────────────────────────────

restart_count=0

while true; do
  echo "[$(date -u)] Starting pump-quant Rust daemon (restart #$restart_count)" >> "$LOG"

  PAPER_MODE=true RUST_LOG=info RUST_BACKTRACE=1 \
    "$BINARY" >> "$LOG" 2>&1 &
  DAEMON_PID=$!
  echo $DAEMON_PID > "$PIDFILE"

  # Wait for process to finish
  wait $DAEMON_PID || true
  EXIT_CODE=$?

  rm -f "$PIDFILE"
  restart_count=$((restart_count + 1))

  if [ $restart_count -ge $MAX_RESTARTS ]; then
    echo "[$(date -u)] Too many restarts ($MAX_RESTARTS). Stopping supervisor." >> "$LOG"
    if [ -n "${TELEGRAM_BOT_TOKEN:-}" ] && [ -n "${TELEGRAM_CHAT_ID:-}" ]; then
      curl -s "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage" \
        -d "chat_id=${TELEGRAM_CHAT_ID}" \
        -d "text=❌ pump-quant Rust daemon crashed $MAX_RESTARTS times — supervisor stopped" > /dev/null
    fi
    exit 1
  fi

  echo "[$(date -u)] Daemon exited with code $EXIT_CODE. Restarting in ${RESTART_DELAY}s..." >> "$LOG"
  sleep "$RESTART_DELAY"
done
