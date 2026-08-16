#!/usr/bin/env bash
# Rev-17 launcher: pre-entry momentum gate (buy_pressure_bp + unique_buyers)
# Research-backed entry thresholds from ArXiv papers
cd /d/repos/mev_bot/rust
export PQ_CREDS_FILE="$HOME/.hermes/creds/pump-quant.env"

# Kill any existing daemon/watchdog
powershell.exe -Command "Get-Process pq-daemon,pq-watchdog -ErrorAction SilentlyContinue | Stop-Process -Force" 2>/dev/null
sleep 3

# Start fresh watchdog with Rev-17 daemon args
./target/release/pq-watchdog.exe \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200 --refiner-every-ticks 72000 --strategy-label rev17-momentum-gate" \
    > data/watchdog_rev17.log 2>&1 &

echo "Rev-17 watchdog launched, pid=$!"
sleep 5
tasklist 2>/dev/null | grep -i "pq-" | head -5
