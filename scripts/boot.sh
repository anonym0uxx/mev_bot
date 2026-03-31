#!/usr/bin/env bash
# boot.sh — Cold start all pump-quant processes after Docker restart
#
# Called from OpenClaw heartbeat when it detects processes are down.
# Idempotent: safe to run multiple times. Checks before starting.
#
# Start order matters:
#   1. Jito ShredStream proxy (gRPC must be up before daemon connects)
#   2. Wait for proxy gRPC port
#   3. Rust trading daemon (connects to proxy + all WS feeds)
#   4. Verify all feeds healthy

set -euo pipefail

PROJECT_DIR="/data/.openclaw/workspace/projects/pump-quant"
cd "$PROJECT_DIR"

LOG="logs/boot.log"
mkdir -p logs data

log() {
  echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" | tee -a "$LOG"
}

log "═══ BOOT SEQUENCE STARTING ═══"

# ── 1. Kill any stale/zombie processes ─────────────────────────────────────
log "Step 1: Cleaning stale processes"
pkill -f "node dist/daemon" 2>/dev/null || true
# Don't kill existing healthy processes — check first

# ── 2. Start Jito ShredStream proxy (if not running) ──────────────────────
PROXY_PID=$(pgrep -f "jito-shredstream-proxy" 2>/dev/null | head -1 || true)
if [ -n "$PROXY_PID" ] && timeout 2 bash -c 'echo > /dev/tcp/127.0.0.1/20100' 2>/dev/null; then
  log "Step 2: ShredStream proxy already running (PID $PROXY_PID, port 20100 up) — skipping"
else
  log "Step 2: Starting ShredStream proxy"
  pkill -f "jito-shredstream-proxy" 2>/dev/null || true
  sleep 1
  
  nohup shredstream-proxy/target/release/jito-shredstream-proxy shredstream \
    --block-engine-url https://mainnet.block-engine.jito.wtf \
    --auth-keypair config/keys/shredstream-keypair.json \
    --desired-regions ny \
    --dest-ip-ports 127.0.0.1:20000 \
    --grpc-service-port 20100 \
    >> logs/shredstream-proxy.log 2>&1 &
  
  PROXY_PID=$!
  echo "$PROXY_PID" > data/shredstream-proxy.pid
  
  # Wait for gRPC port
  log "  Waiting for gRPC port 20100..."
  retries=0
  while [ $retries -lt 15 ]; do
    if timeout 2 bash -c 'echo > /dev/tcp/127.0.0.1/20100' 2>/dev/null; then
      log "  ✅ gRPC port up after ${retries}s"
      break
    fi
    sleep 1
    retries=$((retries + 1))
  done
  
  if [ $retries -ge 15 ]; then
    log "  ⚠️ gRPC port not up after 15s — continuing anyway (proxy may need more time)"
  fi
fi

# ── 3. Start Rust trading daemon (if not running) ─────────────────────────
DAEMON_PID=$(pgrep -f "target/release/pump-quant$" 2>/dev/null | head -1 || true)
DAEMON_HEALTHY=false
if [ -n "$DAEMON_PID" ]; then
  health=$(curl -s --connect-timeout 3 --max-time 3 http://127.0.0.1:9421/api/health 2>/dev/null || echo "FAIL")
  if [ "$health" != "FAIL" ]; then
    DAEMON_HEALTHY=true
  fi
fi

if $DAEMON_HEALTHY; then
  log "Step 3: Rust daemon already running and healthy (PID $DAEMON_PID) — skipping"
else
  log "Step 3: Starting Rust daemon"
  bash scripts/ensure-single-daemon.sh --start 2>&1 | while read line; do log "  $line"; done || true
  
  # Wait for API
  log "  Waiting for API on :9421..."
  retries=0
  while [ $retries -lt 20 ]; do
    if curl -s --connect-timeout 2 --max-time 2 http://127.0.0.1:9421/api/health > /dev/null 2>&1; then
      log "  ✅ API up after ${retries}s"
      break
    fi
    sleep 1
    retries=$((retries + 1))
  done
fi

# ── 4. Verify everything ──────────────────────────────────────────────────
log "Step 4: Final verification"
sleep 3

health=$(curl -s --connect-timeout 5 --max-time 5 http://127.0.0.1:9421/api/health 2>/dev/null || echo "FAIL")
if [ "$health" = "FAIL" ]; then
  log "❌ API not responding after boot — check logs/rust-daemon.log"
  exit 1
fi

echo "$health" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)['data']
    feeds = d.get('feeds', {})
    for name in ['pumpportal', 'helius', 'corecast']:
        f = feeds.get(name, {})
        print(f'  {name}: {f.get(\"status\",\"?\")} age={f.get(\"age_s\",\"?\")}s')
    print(f'  overall: {d[\"overall\"]} | paused: {d[\"trading_paused\"]}')
except:
    print('  (parse error)')
" 2>/dev/null | while read line; do log "$line"; done

grpc_status=$(grep "shredstream.*gRPC" logs/rust-daemon.log 2>/dev/null | tail -1 | sed 's/\x1B\[[0-9;]*m//g' | grep -o "connected\|failed\|error" || echo "unknown")
log "  ShredStream gRPC: $grpc_status"

log "═══ BOOT SEQUENCE COMPLETE ═══"

# Summary
PROXY_PID=$(pgrep -f "jito-shredstream-proxy" 2>/dev/null | head -1 || echo "DEAD")
DAEMON_PID=$(pgrep -f "target/release/pump-quant$" 2>/dev/null | head -1 || echo "DEAD")
log "  Proxy PID: $PROXY_PID | Daemon PID: $DAEMON_PID"
