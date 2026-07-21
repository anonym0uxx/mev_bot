#!/usr/bin/env bash
# ensure-single-daemon.sh
# Enforces exactly ONE pump-quant daemon running at all times.
# Kills ALL duplicates (TS + Rust) before allowing a start.
# Source this or call it directly — safe to run at any time.
#
# Usage:
#   bash scripts/ensure-single-daemon.sh           # kill all, don't start
#   bash scripts/ensure-single-daemon.sh --start   # kill all, then start Rust daemon

set -euo pipefail

PIDFILE="/data/.openclaw/workspace/projects/pump-quant/data/pump-quant.pid"
LOG="logs/rust-daemon.log"
BINARY="./rust/target/release/pump-quant"

echo "[ensure-single-daemon] Checking for duplicate daemons..."

# ── 1. Kill TS daemon (dead/gone but belt+suspenders) ────────────────────────
TS_PIDS=$(pgrep -f "node dist/daemon" 2>/dev/null || true)
if [ -n "$TS_PIDS" ]; then
  echo "[ensure-single-daemon] ⚠️  TS daemon running ($TS_PIDS) — killing"
  kill $TS_PIDS 2>/dev/null || true
  sleep 1
  kill -9 $TS_PIDS 2>/dev/null || true
  echo "[ensure-single-daemon] ✅ TS daemon killed"
else
  echo "[ensure-single-daemon] ✅ TS daemon: not running"
fi

# ── 2. Kill TS supervisor ────────────────────────────────────────────────────
pkill -f "run-daemon.sh" 2>/dev/null || true

# ── 3. Kill ALL Rust daemon processes ────────────────────────────────────────
RUST_PIDS=$(pgrep -f "pump-quant" 2>/dev/null | grep -v "ensure-single\|run-rust-daemon\|$$" || true)
if [ -n "$RUST_PIDS" ]; then
  COUNT=$(echo "$RUST_PIDS" | wc -w)
  echo "[ensure-single-daemon] Found $COUNT Rust daemon process(es): $RUST_PIDS"
  kill $RUST_PIDS 2>/dev/null || true
  sleep 2
  # Force kill anything still alive
  STILL_ALIVE=$(pgrep -f "pump-quant" 2>/dev/null | grep -v "ensure-single\|run-rust-daemon\|$$" || true)
  if [ -n "$STILL_ALIVE" ]; then
    kill -9 $STILL_ALIVE 2>/dev/null || true
  fi
  echo "[ensure-single-daemon] ✅ All Rust daemons killed"
else
  echo "[ensure-single-daemon] ✅ Rust daemon: not running"
fi

# ── 4. Kill supervisor loop if running ───────────────────────────────────────
pkill -f "run-rust-daemon.sh" 2>/dev/null || true

# ── 5. Clean stale PID file ──────────────────────────────────────────────────
if [ -f "$PIDFILE" ]; then
  STALE_PID=$(cat "$PIDFILE" 2>/dev/null || echo "")
  if [ -n "$STALE_PID" ] && ! kill -0 "$STALE_PID" 2>/dev/null; then
    rm -f "$PIDFILE"
    echo "[ensure-single-daemon] Removed stale PID file ($STALE_PID)"
  fi
fi

# ── 6. Verify clean ──────────────────────────────────────────────────────────
sleep 1
REMAINING=$(pgrep -f "pump-quant\|node dist/daemon" 2>/dev/null | grep -v "ensure-single\|$$" || true)
if [ -n "$REMAINING" ]; then
  echo "[ensure-single-daemon] ❌ Still running: $REMAINING — force killing"
  kill -9 $REMAINING 2>/dev/null || true
  sleep 1
fi

echo "[ensure-single-daemon] ✅ Clean — no duplicate daemons"

# ── 7. Optionally start Rust daemon ──────────────────────────────────────────
if [ "${1:-}" = "--start" ]; then
  mkdir -p logs data
  if [ -f "./rust/.env" ]; then
    set -a && source ./rust/.env && set +a
  fi
  # Load build flags (target-cpu=native for SIMD/AVX2 in hot path)
  if [ -f "./rust/.env.build" ]; then
    set -a && source ./rust/.env.build && set +a
  fi
  echo "[ensure-single-daemon] Starting Rust daemon..."
  RUST_LOG=info,pump_quant_core::momentum=debug nohup "$BINARY" > "$LOG" 2>&1 &
  NEW_PID=$!
  echo $NEW_PID > "$PIDFILE"
  echo "[ensure-single-daemon] ✅ Rust daemon started (PID $NEW_PID)"
  sleep 2
  # Verify it actually started
  if kill -0 "$NEW_PID" 2>/dev/null; then
    echo "[ensure-single-daemon] ✅ Confirmed alive"
  else
    echo "[ensure-single-daemon] ❌ Daemon died immediately — check $LOG"
    exit 1
  fi
fi
