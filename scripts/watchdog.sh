#!/usr/bin/env bash
# watchdog.sh — Self-healing process monitor for pump-quant
#
# Ensures ALL required processes are alive and connected:
#   1. Jito ShredStream proxy (gRPC on :20100)
#   2. Rust trading daemon (HTTP on :9421)
#
# Checks feed health via the daemon's /api/health endpoint.
# Auto-restarts dead processes. Logs all actions.
#
# Usage:
#   bash scripts/watchdog.sh              # Run once (for cron / heartbeat)
#   bash scripts/watchdog.sh --loop       # Run as persistent supervisor loop
#   bash scripts/watchdog.sh --status     # Print status and exit
#
# Exit codes:
#   0 = all healthy
#   1 = something was restarted (check logs)
#   2 = critical failure (could not recover)

set -euo pipefail

PROJECT_DIR="/data/.openclaw/workspace/projects/pump-quant"
cd "$PROJECT_DIR"

LOG="logs/watchdog.log"
mkdir -p logs data

PROXY_BINARY="shredstream-proxy/target/release/jito-shredstream-proxy"
PROXY_LOG="logs/shredstream-proxy.log"
PROXY_PIDFILE="data/shredstream-proxy.pid"

DAEMON_BINARY="rust/target/release/pump-quant"
DAEMON_LOG="logs/rust-daemon.log"
DAEMON_PIDFILE="data/pump-quant.pid"

HEALTH_URL="http://127.0.0.1:9421/api/health"
HEALTH_TIMEOUT=5

RESTARTED=0

log() {
  echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" >> "$LOG"
  echo "$*"
}

# ── 1. Jito ShredStream Proxy ────────────────────────────────────────────────

check_proxy() {
  local pid
  pid=$(pgrep -f "jito-shredstream-proxy" 2>/dev/null | head -1 || true)
  
  if [ -z "$pid" ]; then
    log "⚠️  ShredStream proxy DEAD — starting"
    start_proxy
    return 1
  fi
  
  # Verify gRPC port is reachable (ss doesn't always show gRPC listeners)
  # Extended grace window: proxy takes 10-15s to bind gRPC after launch
  if ! timeout 2 bash -c 'echo > /dev/tcp/127.0.0.1/20100' 2>/dev/null; then
    log "  Port 20100 not yet bound — waiting up to 15s for proxy startup..."
    local waited=0
    while [ $waited -lt 15 ]; do
      sleep 1
      waited=$((waited + 1))
      if timeout 2 bash -c 'echo > /dev/tcp/127.0.0.1/20100' 2>/dev/null; then
        log "  ✅ Port 20100 bound after ${waited}s"
        break
      fi
    done
    if ! timeout 2 bash -c 'echo > /dev/tcp/127.0.0.1/20100' 2>/dev/null; then
      log "⚠️  ShredStream proxy running (PID $pid) but port 20100 unreachable after 15s — restarting"
      pkill -f "jito-shredstream-proxy" 2>/dev/null || true
      sleep 2
      start_proxy
      return 1
    fi
  fi
  
  log "✅ ShredStream proxy: PID $pid, port 20100 bound"
  return 0
}

start_proxy() {
  # Kill any existing proxy processes first
  pkill -f "jito-shredstream-proxy" 2>/dev/null || true
  sleep 1
  
  if [ ! -f "$PROXY_BINARY" ]; then
    log "❌ CRITICAL: Proxy binary not found at $PROXY_BINARY"
    return 2
  fi
  
  nohup "$PROXY_BINARY" shredstream \
    --block-engine-url https://mainnet.block-engine.jito.wtf \
    --auth-keypair config/keys/shredstream-keypair.json \
    --desired-regions ny \
    --dest-ip-ports 127.0.0.1:20000 \
    --grpc-service-port 20100 \
    >> "$PROXY_LOG" 2>&1 &
  
  local new_pid=$!
  echo "$new_pid" > "$PROXY_PIDFILE"
  
  # Wait for gRPC port to come up
  local retries=0
  while [ $retries -lt 10 ]; do
    if timeout 2 bash -c 'echo > /dev/tcp/127.0.0.1/20100' 2>/dev/null; then
      log "✅ ShredStream proxy started: PID $new_pid, port 20100 up"
      RESTARTED=1
      return 0
    fi
    sleep 1
    retries=$((retries + 1))
  done
  
  log "⚠️  ShredStream proxy started (PID $new_pid) but port 20100 not yet bound after 10s"
  RESTARTED=1
  return 0
}

# ── 2. Rust Trading Daemon ───────────────────────────────────────────────────

check_daemon() {
  local pid
  pid=$(pgrep -f "target/release/pump-quant$" 2>/dev/null | head -1 || true)
  
  if [ -z "$pid" ]; then
    log "⚠️  Rust daemon DEAD — starting"
    start_daemon
    return 1
  fi
  
  # Verify HTTP API is responding
  local health
  health=$(curl -s --connect-timeout "$HEALTH_TIMEOUT" --max-time "$HEALTH_TIMEOUT" "$HEALTH_URL" 2>/dev/null || echo "FAIL")
  
  if [ "$health" = "FAIL" ]; then
    # API not responding — give it a moment (might be starting up)
    sleep 3
    health=$(curl -s --connect-timeout "$HEALTH_TIMEOUT" --max-time "$HEALTH_TIMEOUT" "$HEALTH_URL" 2>/dev/null || echo "FAIL")
    if [ "$health" = "FAIL" ]; then
      log "⚠️  Rust daemon running (PID $pid) but API not responding — restarting"
      kill "$pid" 2>/dev/null || true
      sleep 2
      kill -9 "$pid" 2>/dev/null || true
      start_daemon
      return 1
    fi
  fi
  
  log "✅ Rust daemon: PID $pid, API responding"
  return 0
}

start_daemon() {
  # Use ensure-single-daemon to kill any stale processes
  bash scripts/ensure-single-daemon.sh 2>/dev/null || true
  
  if [ ! -f "$DAEMON_BINARY" ]; then
    log "❌ CRITICAL: Daemon binary not found at $DAEMON_BINARY"
    return 2
  fi
  
  # Load environment
  if [ -f "rust/.env" ]; then
    set -a && source "rust/.env" && set +a
  fi
  if [ -f "rust/.env.build" ]; then
    set -a && source "rust/.env.build" && set +a
  fi
  
  PAPER_MODE=true RUST_LOG=info nohup "$DAEMON_BINARY" >> "$DAEMON_LOG" 2>&1 &
  local new_pid=$!
  echo "$new_pid" > "$DAEMON_PIDFILE"
  
  # Wait for API to come up
  local retries=0
  while [ $retries -lt 15 ]; do
    local health
    health=$(curl -s --connect-timeout 2 --max-time 2 "$HEALTH_URL" 2>/dev/null || echo "FAIL")
    if [ "$health" != "FAIL" ]; then
      log "✅ Rust daemon started: PID $new_pid, API up"
      RESTARTED=1
      return 0
    fi
    sleep 1
    retries=$((retries + 1))
  done
  
  # Check if process is still alive even if API isn't up yet
  if kill -0 "$new_pid" 2>/dev/null; then
    log "⚠️  Rust daemon started (PID $new_pid) but API not yet up after 15s — may be connecting feeds"
    RESTARTED=1
    return 0
  fi
  
  log "❌ Rust daemon failed to start — check $DAEMON_LOG"
  return 2
}

# ── 3. Feed Health Check ─────────────────────────────────────────────────────

check_feeds() {
  local health
  health=$(curl -s --connect-timeout "$HEALTH_TIMEOUT" --max-time "$HEALTH_TIMEOUT" "$HEALTH_URL" 2>/dev/null || echo "FAIL")
  
  if [ "$health" = "FAIL" ]; then
    log "⚠️  Cannot reach health API — skipping feed check"
    return 1
  fi
  
  echo "$health" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)['data']
    feeds = d.get('feeds', {})
    overall = d.get('overall', 'unknown')
    paused = d.get('trading_paused', False)
    
    stale = []
    for name in ['pumpportal', 'helius', 'corecast']:
        f = feeds.get(name, {})
        status = f.get('status', 'missing')
        age = f.get('age_s', 999)
        if status != 'healthy' or age > 60:
            stale.append(f'{name}({status},age={age}s)')
    
    if stale:
        print(f'STALE:{\"|\".join(stale)}')
    elif overall != 'healthy':
        print(f'DEGRADED:{overall}')
    elif paused:
        print('PAUSED')
    else:
        print('OK')
except:
    print('PARSE_ERROR')
" 2>/dev/null | while read -r result; do
    case "$result" in
      OK)
        log "✅ All feeds healthy"
        ;;
      PAUSED)
        log "⚠️  Trading paused — attempting resume"
        curl -s -X POST http://127.0.0.1:9421/api/control/resume \
          -H "Content-Type: application/json" -d '{}' > /dev/null 2>&1
        ;;
      STALE:*)
        log "⚠️  Stale feeds: ${result#STALE:}"
        # Don't restart — the daemon's own HealthMonitor handles stale feeds
        # But if feeds are stale for >5 min, a daemon restart may help
        ;;
      DEGRADED:*)
        log "⚠️  Health degraded: ${result#DEGRADED:}"
        ;;
      *)
        log "⚠️  Feed check: $result"
        ;;
    esac
  done
  
  return 0
}

# ── 4. ShredStream gRPC Connection Check ─────────────────────────────────────

check_shredstream_grpc() {
  # Check daemon logs for recent gRPC connection status
  local last_grpc
  last_grpc=$(grep -E "shredstream.*gRPC" "$DAEMON_LOG" 2>/dev/null | tail -1 | sed 's/\x1B\[[0-9;]*m//g' || echo "")
  
  if echo "$last_grpc" | grep -q "connected"; then
    log "✅ ShredStream gRPC: connected"
  elif echo "$last_grpc" | grep -q "failed\|error"; then
    log "⚠️  ShredStream gRPC issue: $last_grpc"
    # Check if proxy is alive — if not, restart it (daemon will auto-reconnect)
    if ! pgrep -f "jito-shredstream-proxy" > /dev/null 2>&1; then
      log "  → Proxy dead, restarting proxy (daemon will auto-reconnect)"
      start_proxy
    fi
  else
    log "ℹ️  ShredStream gRPC: no recent log entry"
  fi
}

# ── 5. Stale TS Daemon Cleanup ───────────────────────────────────────────────

kill_stale() {
  # Kill any old TS daemon processes
  local ts_pids
  ts_pids=$(ps aux | grep "node dist/daemon" | grep -v grep | awk '{print $2}' || true)
  if [ -n "$ts_pids" ]; then
    log "⚠️  Killing stale TS daemon: $ts_pids"
    echo "$ts_pids" | xargs kill 2>/dev/null || true
    sleep 1
    echo "$ts_pids" | xargs kill -9 2>/dev/null || true
  fi
  
  # Kill any duplicate Rust daemons (keep only newest)
  local rust_pids
  rust_pids=$(pgrep -f "target/release/pump-quant$" 2>/dev/null | sort -n || true)
  local count
  count=$(echo "$rust_pids" | grep -c . 2>/dev/null || echo 0)
  if [ "$count" -gt 1 ]; then
    log "⚠️  Multiple Rust daemons ($count) — killing all but newest"
    echo "$rust_pids" | head -n $((count - 1)) | xargs kill 2>/dev/null || true
  fi
}

# ── Main ─────────────────────────────────────────────────────────────────────

run_once() {
  log "─── Watchdog check starting ───"
  
  kill_stale
  check_proxy || true
  check_daemon || true
  check_feeds || true
  check_shredstream_grpc || true
  
  if [ "$RESTARTED" -gt 0 ]; then
    log "─── Watchdog: restarted $RESTARTED process(es) ───"
    return 1
  else
    log "─── Watchdog: all systems nominal ───"
    return 0
  fi
}

case "${1:-}" in
  --status)
    echo "=== pump-quant watchdog status ==="
    echo ""
    echo "Proxy:  $(pgrep -f 'jito-shredstream-proxy' | head -1 || echo 'DEAD')"
    echo "Daemon: $(pgrep -f 'target/release/pump-quant$' | head -1 || echo 'DEAD')"
    echo "Health: $(curl -s --connect-timeout 3 --max-time 3 http://127.0.0.1:9421/api/health 2>/dev/null | python3 -c "import sys,json; d=json.load(sys.stdin)['data']; print(f\"{d['overall']} paused={d['trading_paused']}\")" 2>/dev/null || echo 'API unreachable')"
    echo "gRPC:   $(grep 'shredstream.*gRPC' logs/rust-daemon.log 2>/dev/null | tail -1 | sed 's/\x1B\[[0-9;]*m//g' | grep -o 'connected\|failed\|error' || echo 'unknown')"
    ;;
  --loop)
    log "═══ Watchdog supervisor loop starting ═══"
    while true; do
      run_once || true
      sleep 60
    done
    ;;
  *)
    run_once
    ;;
esac
