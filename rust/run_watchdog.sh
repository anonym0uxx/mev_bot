#!/usr/bin/env bash
# Minimal launcher for pq-watchdog (Rev-14)
cd "$(dirname "$0")"
export PQ_CREDS_FILE="$HOME/.hermes/creds/pump-quant.env"
exec ./target/release/pq-watchdog.exe \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200 --refiner-every-ticks 72000 --strategy-label rev14-crash-reversion"
