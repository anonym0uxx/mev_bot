#!/bin/bash
# Daemon supervisor — strict singleton, auto-restarts on crash
cd /data/.openclaw/workspace/projects/pump-quant

PIDFILE="logs/daemon.pid"
LOCKFILE="logs/supervisor.lock"
LOGFILE="logs/daemon.log"

mkdir -p logs

# --- Singleton enforcement via lockfile ---
# Any existing supervisor holds this lock. If we can't get it, exit immediately.
exec 200>"$LOCKFILE"
if ! flock -n 200; then
    echo "[$(date)] Another supervisor already running — exiting." >> logs/supervisor.log
    exit 1
fi

# Kill any stray daemon processes not tracked by our pidfile (from previous crashed supervisors)
STRAY=$(pgrep -f "node dist/daemon/index.js" 2>/dev/null)
if [ -n "$STRAY" ]; then
    echo "[$(date)] Killing stray daemon PIDs: $STRAY" >> logs/supervisor.log
    kill -9 $STRAY 2>/dev/null
    sleep 1
fi
rm -f "$PIDFILE"

cleanup() {
    echo "[$(date)] Supervisor shutting down..." >> logs/supervisor.log
    if [ -f "$PIDFILE" ]; then
        DPID=$(cat "$PIDFILE")
        kill -TERM $DPID 2>/dev/null
        for i in $(seq 1 8); do
            sleep 1
            kill -0 $DPID 2>/dev/null || break
        done
        kill -9 $DPID 2>/dev/null
        rm -f "$PIDFILE"
    fi
    flock -u 200
    rm -f "$LOCKFILE"
    exit 0
}
trap cleanup EXIT SIGTERM SIGINT

echo "[$(date)] Supervisor started (PID $$)" >> logs/supervisor.log

while true; do
    # Guard: ensure no other daemon is running before starting a new one
    EXISTING=$(pgrep -f "node dist/daemon/index.js" 2>/dev/null)
    if [ -n "$EXISTING" ]; then
        echo "[$(date)] WARNING: stray daemon $EXISTING found before start — killing" >> logs/supervisor.log
        kill -9 $EXISTING 2>/dev/null
        sleep 1
    fi

    echo "[$(date)] Starting daemon..." >> logs/supervisor.log
    CONFIG_PATH=config/canary.json PAPER_MODE=${PAPER_MODE:-true} node dist/daemon/index.js >> "$LOGFILE" 2>&1 &
    DAEMON_PID=$!
    echo $DAEMON_PID > "$PIDFILE"
    echo "[$(date)] Daemon PID: $DAEMON_PID" >> logs/supervisor.log

    wait $DAEMON_PID
    EXIT_CODE=$?
    echo "[$(date)] Daemon exited (code $EXIT_CODE). Restarting in 5s..." >> logs/supervisor.log
    rm -f "$PIDFILE"
    sleep 5
done
