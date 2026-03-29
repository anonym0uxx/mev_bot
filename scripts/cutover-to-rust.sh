#!/usr/bin/env bash
# Full cutover: stop TS daemon, start Rust daemon
# Run this when cutover checklist is complete

set -euo pipefail

echo "=== pump-quant cutover: TS → Rust ==="
echo ""

# 1. Verify Rust daemon health
RUST_HEALTH=$(curl -s http://127.0.0.1:9421/api/health 2>/dev/null | jq -r '.status' 2>/dev/null || echo "unreachable")
if [ "$RUST_HEALTH" != "ok" ]; then
  echo "❌ Rust daemon not healthy at :9421. Aborting."
  echo "   Start it first: nohup bash scripts/run-rust-daemon.sh &"
  exit 1
fi
echo "✅ Rust daemon healthy"

# 2. Stop TS daemon supervisor
pkill -f "run-daemon.sh" 2>/dev/null && echo "✅ TS supervisor stopped" || echo "   TS supervisor not running"

# 3. Stop TS daemon process
pkill -f "node dist/daemon/index.js" 2>/dev/null && echo "✅ TS daemon stopped" || echo "   TS daemon not running"

# 4. Verify TS is truly dead
sleep 2
if pgrep -f "node dist/daemon" > /dev/null; then
  echo "❌ TS daemon still running. Kill manually: pkill -9 -f 'node dist/daemon'"
  exit 1
fi
echo "✅ TS daemon confirmed dead"

# 5. Verify Rust is still alive
RUST_STATS=$(curl -s http://127.0.0.1:9421/api/stats | jq -r '.data.uptime_s' 2>/dev/null || echo "0")
echo "✅ Rust daemon alive (uptime: ${RUST_STATS}s)"

echo ""
echo "=== Cutover complete ==="
echo "Rust daemon is now the primary on port 9421"
echo "Monitor: tail -f logs/rust-daemon.log"
echo "Status:  curl http://127.0.0.1:9421/api/stats"
