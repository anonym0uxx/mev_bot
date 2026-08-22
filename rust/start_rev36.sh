#!/usr/bin/env bash
# Rev-36 daemon start (SL=10%, TP3=+99%, mcap_hi=40SOL, entry filters, TOCTOU mcap fix)
set -euo pipefail
cd /d/repos/mev_bot/rust

CREDS="C:/Users/Alon/.hermes/creds/pump-quant.env"
export PQ_CREDS_FILE="$CREDS"

HELIUS_API_KEY=$(grep '^HELIUS_API_KEY=' "$CREDS" | cut -d= -f2-)
LASERSTREAM_ENDPOINT=$(grep '^LASERSTREAM_ENDPOINT=' "$CREDS" | cut -d= -f2-)
HELIUS_WS_URL=$(grep '^HELIUS_WS_URL=' "$CREDS" | cut -d= -f2-)
SENDER_ENDPOINT=$(grep '^SENDER_ENDPOINT=' "$CREDS" | cut -d= -f2-)
WALLET_ADDRESS=$(grep '^WALLET_ADDRESS=' "$CREDS" | cut -d= -f2-)
PUMPPORTAL_WS_URL=$(grep '^PUMPPORTAL_WS_URL=' "$CREDS" | cut -d= -f2- || echo "")

export HELIUS_API_KEY LASERSTREAM_ENDPOINT HELIUS_WS_URL SENDER_ENDPOINT WALLET_ADDRESS
[ -n "$PUMPPORTAL_WS_URL" ] && export PUMPPORTAL_WS_URL

LS_BIN_LINUX="/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/target/release/pq-laserstream-grpc"
export PQ_LASERSTREAM_BIN="wsl.exe"
export PQ_LASERSTREAM_ARGS="-d Ubuntu -- $LS_BIN_LINUX"

echo "[start_rev36] Launching watchdog with Rev-36 daemon args..."
./target/release/pq-watchdog.exe \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--live --wallet-address $WALLET_ADDRESS --junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200 --refiner-every-ticks 72000 --strategy-label rev36-live" \
    > data/watchdog_rev36.log 2>&1 &
echo "[start_rev36] Watchdog launched, PID=$!"
sleep 8
echo "=== PROCESS STATUS ==="
tasklist 2>/dev/null | grep -i "pq-" | head -10
