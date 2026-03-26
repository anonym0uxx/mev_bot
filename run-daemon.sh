#!/bin/bash
# Daemon supervisor — auto-restarts on crash, prevents duplicates
cd /data/.openclaw/workspace/projects/pump-quant

PIDFILE="logs/daemon.pid"
LOCKFILE="logs/supervisor.lock"

# Exclusive lock: only one supervisor allowed at a time
exec 200>"$LOCKFILE"
if ! flock -n 200; then
    echo "[$(date)] Another supervisor is already running. Exiting." >> logs/supervisor.log
    exit 1
fi

cleanup() {
    if [ -f "$PIDFILE" ]; then
        kill $(cat "$PIDFILE") 2>/dev/null
        rm -f "$PIDFILE"
    fi
    rm -f "$LOCKFILE"
    exit 0
}
trap cleanup EXIT SIGTERM SIGINT

# Kill any existing daemon instances
pkill -9 -f "node dist/daemon/index.js" 2>/dev/null
sleep 2

echo "[$(date)] Supervisor started (PID $$)" >> logs/supervisor.log

while true; do
    echo "[$(date)] Starting daemon..." >> logs/supervisor.log
    CONFIG_PATH=config/canary.json PAPER_MODE=false node dist/daemon/index.js >> logs/daemon.log 2>&1 &
    DAEMON_PID=$!
    echo $DAEMON_PID > "$PIDFILE"
    echo "[$(date)] Daemon PID: $DAEMON_PID" >> logs/supervisor.log
    wait $DAEMON_PID
    EXIT_CODE=$?
    echo "[$(date)] Daemon exited with code $EXIT_CODE. Restarting in 5s..." >> logs/supervisor.log
    rm -f "$PIDFILE"
    sleep 5
done
