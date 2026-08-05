#!/usr/bin/env bash
# Launch pq-watchdog which spawns pq-daemon (paper trading)
# Requires PQ_CREDS_FILE env var (set at User scope)
cd "$(dirname "$0")"
mkdir -p data

# ─── Pre-launch dedup check ───────────────────────────────────────────────
# Before starting, verify no existing watchdog or daemon processes are running.
# The watchdog itself has a PID-file guard, but this script-level check catches
# edge cases (orphaned daemons without a watchdog, stale PID files, etc.).

check_dedup() {
    local wd_count dae_count sc_count
    wd_count=$(tasklist 2>/dev/null | grep -c "pq-watchdog" 2>/dev/null)
    dae_count=$(tasklist 2>/dev/null | grep -c "pq-daemon" 2>/dev/null)
    sc_count=$(tasklist 2>/dev/null | grep -c "pq-stream-capture" 2>/dev/null)
    fc_count=$(tasklist 2>/dev/null | grep -c "pq-firecrawl-bridge" 2>/dev/null)
    # Strip any stray whitespace/newlines from Windows tasklist output
    wd_count=$(echo "$wd_count" | tr -d '[:space:]')
    dae_count=$(echo "$dae_count" | tr -d '[:space:]')
    sc_count=$(echo "$sc_count" | tr -d '[:space:]')
    fc_count=$(echo "$fc_count" | tr -d '[:space:]')
    wd_count=${wd_count:-0}
    dae_count=${dae_count:-0}
    sc_count=${sc_count:-0}
    fc_count=${fc_count:-0}

    if [ "$wd_count" -gt 0 ] || [ "$dae_count" -gt 0 ] || [ "$sc_count" -gt 0 ] || [ "$fc_count" -gt 0 ]; then
        echo "DEDUP CHECK FAILED — existing pq processes found:"
        echo "  pq-watchdog:           $wd_count"
        echo "  pq-daemon:             $dae_count"
        echo "  pq-stream-capture:     $sc_count"
        echo "  pq-firecrawl-bridge:   $fc_count"
        echo ""
        echo "Refusing to launch. Kill existing processes first:"
        echo "  taskkill /F /IM pq-daemon.exe"
        echo "  taskkill /F /IM pq-watchdog.exe"
        echo "  taskkill /F /IM pq-stream-capture.exe"
        echo "  taskkill /F /IM pq-firecrawl-bridge.exe"
        echo "  rm -f data/watchdog.pid"
        exit 1
    fi

    # Also check for a stale PID file without a live process
    if [ -f data/watchdog.pid ]; then
        local pid_in_file
        pid_in_file=$(cat data/watchdog.pid 2>/dev/null)
        if tasklist /FI "PID eq $pid_in_file" /NH 2>/dev/null | grep -q "$pid_in_file"; then
            echo "DEDUP CHECK FAILED — watchdog PID file points to a live process (pid=$pid_in_file)"
            echo "Refusing to launch."
            exit 1
        else
            echo "Stale PID file (pid=$pid_in_file no longer alive) — cleaning up."
            rm -f data/watchdog.pid
        fi
    fi

    echo "Dedup check: CLEAN (no existing pq processes)"
}

check_dedup

# Clean up any stale sentinels
rm -f data/EMERGENCY_STOP.sentinel data/DAEMON_STOP.sentinel

# Point to the LaserStream gRPC stream-capture binary
export PQ_LASERSTREAM_BIN="${PQ_LASERSTREAM_BIN:-D:/repos/mev_bot/tools/stream-capture-rs/target/release/pq-stream-capture.exe}"

# Point to the Firecrawl web-intelligence bridge binary
export PQ_FIRECRAWL_BIN="${PQ_FIRECRAWL_BIN:-D:/repos/mev_bot/tools/firecrawl-bridge-rs/target/release/pq-firecrawl-bridge.exe}"

exec ./target/release/pq-watchdog \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200"
