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
        DPID=$(cat "$PIDFILE")
        echo "[$(date)] Graceful shutdown: sending SIGTERM to daemon $DPID" >> logs/supervisor.log
        kill -TERM $DPID 2>/dev/null
        # Wait up to 8s for clean shutdown (streams disconnect)
        for i in $(seq 1 8); do
            sleep 1
            kill -0 $DPID 2>/dev/null || break
        done
        # Force-kill only if still running after grace period
        kill -0 $DPID 2>/dev/null && kill -9 $DPID 2>/dev/null
        rm -f "$PIDFILE"
    fi
    rm -f "$LOCKFILE"
    exit 0
}
trap cleanup EXIT SIGTERM SIGINT

# Gracefully stop any existing daemon (SIGTERM first, not SIGKILL)
if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE")
    echo "[$(date)] Stopping existing daemon $OLD_PID gracefully..." >> logs/supervisor.log
    kill -TERM $OLD_PID 2>/dev/null
    for i in $(seq 1 8); do sleep 1; kill -0 $OLD_PID 2>/dev/null || break; done
    kill -0 $OLD_PID 2>/dev/null && kill -9 $OLD_PID 2>/dev/null
    rm -f "$PIDFILE"
fi
# Also catch any stray daemon not tracked by pidfile
pkill -TERM -f "node dist/daemon/index.js" 2>/dev/null
sleep 3  # Give streams time to close cleanly

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
