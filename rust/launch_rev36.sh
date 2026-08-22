#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════
# Rev-36 Launch Script — LaserStream gRPC PRIMARY + Helius WS FALLBACK
# ════════════════════════════════════════════════════════════════════
#
# Architecture:
#   PRIMARY:   pq-laserstream-grpc (Linux/WSL2) — Yellowstone gRPC stream
#              Helius LaserStream endpoint, SDK-owned reconnect + from_slot replay
#   FALLBACK:  Helius WebSocket (slotSubscribe + accountSubscribe)
#              PumpPortal WS for new-coin discovery
#
# The daemon spawns LS as a subprocess, reads NDJSON from stdout, and
# feeds decoded events into the engine. If LS dies, the daemon respawns
# it (up to LS_MAX_RESPAWN_ATTEMPTS) while WS continues as fallback.
# If LS is exhausted, WS becomes sole data source (degraded but alive).
#
# Prerequisites:
#   1. pq-daemon.exe and pq-watchdog.exe built (cargo build --release)
#   2. pq-laserstream-grpc built in WSL2 (stream-capture-rs/grpc-server-only)
#   3. Credentials at C:/Users/Alon/.hermes/creds/pump-quant.env with:
#      HELIUS_API_KEY, LASERSTREAM_ENDPOINT, HELIUS_WS_URL, SENDER_ENDPOINT
#
# ════════════════════════════════════════════════════════════════════

set -euo pipefail

REPO_DIR="/d/repos/mev_bot/rust"
CREDS="C:/Users/Alon/.hermes/creds/pump-quant.env"
LS_BIN_LINUX="/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/target/release/pq-laserstream-grpc"

cd "$REPO_DIR"
export PQ_CREDS_FILE="$CREDS"

# ── Load credential env vars ──────────────────────────────────────────
HELIUS_API_KEY=$(grep '^HELIUS_API_KEY=' "$CREDS" | cut -d= -f2-)
LASERSTREAM_ENDPOINT=$(grep '^LASERSTREAM_ENDPOINT=' "$CREDS" | cut -d= -f2-)
HELIUS_WS_URL=$(grep '^HELIUS_WS_URL=' "$CREDS" | cut -d= -f2-)
SENDER_ENDPOINT=$(grep '^SENDER_ENDPOINT=' "$CREDS" | cut -d= -f2-)
WALLET_ADDRESS=$(grep '^WALLET_ADDRESS=' "$CREDS" | cut -d= -f2-)
PUMPPORTAL_WS_URL=$(grep '^PUMPPORTAL_WS_URL=' "$CREDS" | cut -d= -f2- || echo "")

export HELIUS_API_KEY LASERSTREAM_ENDPOINT HELIUS_WS_URL SENDER_ENDPOINT
export WALLET_ADDRESS
[ -n "$PUMPPORTAL_WS_URL" ] && export PUMPPORTAL_WS_URL

# ── Kill any existing daemon/watchdog/LS orphans ──────────────────────
echo "[launch] Killing existing pq- processes and WSL2 LS orphans..."
powershell.exe -Command "Get-Process pq-daemon,pq-watchdog -ErrorAction SilentlyContinue | Stop-Process -Force" 2>/dev/null || true
sleep 2

# Kill orphaned wsl.exe processes that might hold LaserStream connections
# (burning Helius credits). Only kill wsl.exe spawned by prior daemon runs.
powershell.exe -Command "Get-Process wsl -ErrorAction SilentlyContinue | Where-Object {\$_.CommandLine -like '*pq-laserstream*'} | Stop-Process -Force" 2>/dev/null || true
sleep 1

# ── Verify LaserStream binary exists ──────────────────────────────────
if ! wsl.exe -d Ubuntu -- test -x "$LS_BIN_LINUX" 2>/dev/null; then
    echo "[launch] FATAL: LaserStream binary not found at $LS_BIN_LINUX"
    echo "[launch] Build it: cd tools/stream-capture-rs/grpc-server-only && cargo build --release"
    exit 1
fi

# ── Configure LaserStream as primary via env vars ────────────────────
# PQ_LASERSTREAM_BIN: tells the daemon how to spawn the LS binary.
#   We use wsl.exe to run the Linux binary from Windows. The daemon's
#   spawn_ls closure passes HELIUS_API_KEY and LASERSTREAM_ENDPOINT via
#   cmd.env(), and sets WSLENV so the vars cross the WSL2 boundary.
export PQ_LASERSTREAM_BIN="wsl.exe"
export PQ_LASERSTREAM_ARGS="-d Ubuntu -- $LS_BIN_LINUX"

# ── Start watchdog with live-mode daemon args ────────────────────────
echo "[launch] Starting watchdog with Rev-36 daemon args..."
echo "[launch]   LaserStream: PRIMARY (gRPC, SDK-owned reconnect)"
echo "[launch]   Helius WS:   FALLBACK (slotSubscribe + accountSubscribe)"
echo "[launch]   PumpPortal:  NEW COIN DISCOVERY"

./target/release/pq-watchdog.exe \
    --max-restarts 10 \
    --health-timeout-secs 60 \
    --backoff-cap-secs 30 \
    --daemon-args "--live --wallet-address $WALLET_ADDRESS --junction-cap 8192 --commitment processed --status-every-ticks 50 --brain-snapshot-every-ticks 5000 --tape-every-ticks 200 --refiner-every-ticks 72000 --strategy-label rev36-live" \
    > data/watchdog_rev36.log 2>&1 &

WATCHDOG_PID=$!
echo "[launch] Watchdog launched, pid=$WATCHDOG_PID"
sleep 5

# ── Verify processes are alive ────────────────────────────────────────
echo ""
echo "[launch] === PROCESS STATUS ==="
tasklist 2>/dev/null | grep -i "pq-" | head -10
echo ""

# Check watchdog log for LS spawn confirmation
if grep -q "LaserStream spawned" data/watchdog_rev36.log 2>/dev/null || \
   grep -q "LaserStream spawned" data/launch.log 2>/dev/null; then
    echo "[launch] ✓ LaserStream gRPC spawned successfully"
else
    echo "[launch] ⚠ LaserStream spawn not yet confirmed — check daemon log"
fi

if grep -q "Helius WS" data/watchdog_rev36.log 2>/dev/null || \
   grep -q "Helius WS" data/launch.log 2>/dev/null; then
    echo "[launch] ✓ Helius WS connected as fallback"
else
    echo "[launch] ⚠ Helius WS connection not yet confirmed — check daemon log"
fi

echo ""
echo "[launch] Rev-36 daemon is live. Monitor with:"
echo "  tail -f $REPO_DIR/data/launch.log"
echo "  cat $REPO_DIR/data/daemon_health.json"
echo "  cat $REPO_DIR/data/live_status.json"
