#!/bin/bash
# Daemon supervisor — auto-restarts on crash, prevents duplicates
cd /data/.openclaw/workspace/projects/pump-quant
PIDFILE="logs/daemon.pid"

cleanup() {
    if [ -f "$PIDFILE" ]; then
        kill $(cat "$PIDFILE") 2>/dev/null
        rm -f "$PIDFILE"
    fi
    exit 0
}
trap cleanup EXIT SIGTERM SIGINT

# Kill any existing daemon
pkill -f "node dist/daemon/index.js" 2>/dev/null
sleep 1

while true; do
    echo "[$(date)] Starting daemon..." >> logs/supervisor.log
    CONFIG_PATH=config/canary.json PAPER_MODE=false node dist/daemon/index.js >> logs/daemon.log 2>&1 &
    DAEMON_PID=$!
    echo $DAEMON_PID > "$PIDFILE"
    wait $DAEMON_PID
    EXIT_CODE=$?
    echo "[$(date)] Daemon exited with code $EXIT_CODE. Restarting in 3s..." >> logs/supervisor.log
    rm -f "$PIDFILE"
    sleep 3
done
