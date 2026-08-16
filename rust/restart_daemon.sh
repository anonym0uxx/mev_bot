#!/usr/bin/env bash
# Restart daemon helper — kills old, starts fresh
cd /d/repos/mev_bot/rust
export PQ_CREDS_FILE="$HOME/.hermes/creds/pump-quant.env"

# Kill any existing daemon
powershell.exe -Command "Get-Process pq-daemon -ErrorAction SilentlyContinue | Stop-Process -Force" 2>/dev/null
sleep 2

# Start fresh daemon
./target/release/pq-daemon.exe \
    --junction-cap 8192 \
    --commitment processed \
    --status-every-ticks 50 \
    --brain-snapshot-every-ticks 5000 \
    --tape-every-ticks 200 \
    --refiner-every-ticks 72000 \
    --strategy-label rev16-tight-sl-fat-tail \
    > data/daemon_stderr.log 2>&1 &

echo "Daemon PID: $!"
sleep 5
tasklist 2>/dev/null | grep -i pq-daemon || echo "FAILED TO START"
