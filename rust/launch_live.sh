#!/usr/bin/env bash
# Live mode launcher for Rev-17
cd /d/repos/mev_bot/rust
export PQ_CREDS_FILE="C:/Users/Alon/.hermes/creds/pump-quant.env"

# Kill any existing daemon/watchdog
powershell.exe -Command "Get-Process pq-daemon,pq-watchdog -ErrorAction SilentlyContinue | Stop-Process -Force" 2>/dev/null
sleep 3

# Start watchdog with live-mode daemon args
./target/release/pq-watchdog.exe \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--live --wallet-address 7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC --junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200 --refiner-every-ticks 72000 --strategy-label rev17-live" \
    > data/watchdog_rev17.log 2>&1 &

echo "Live watchdog launched, pid=$!"
sleep 5
tasklist 2>/dev/null | grep -i "pq-" | head -5
