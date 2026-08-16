#!/usr/bin/env bash
# Detached watchdog launcher — survives terminal session end
cd /d/repos/mev_bot/rust

export PQ_CREDS_FILE="$HOME/.hermes/creds/pump-quant.env"

# Redirect all output to a log file, detach from terminal
./target/release/pq-watchdog.exe \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200 --refiner-every-ticks 72000 --strategy-label rev16-tight-sl-fat-tail" \
    > /d/repos/mev_bot/rust/data/watchdog_rev16.log 2>&1 &
echo "watchdog launched, pid=$!"
