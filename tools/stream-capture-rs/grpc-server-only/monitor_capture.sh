#!/bin/bash
# Capture monitor: polls every 3 min, auto-restarts on crash, exits on manifest.
# Uses notify_on_complete to alert the agent when done.

SESSION="20260823_133256_000398"
DATA_DIR="/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data"
BIN_DIR="/mnt/d/repos/mev_bot/tools/stream-capture-rs/grpc-server-only"
MAX_SECS=21600     # 6 hours hard cap
POLL=180           # 3 minutes
RESTART_MAX=5      # max auto-restarts

# Credentials come from the environment — never from this file. The key that used to
# live here was committed to a public remote by accident and MUST be treated as
# compromised: rotate it at Helius and export the new one before running.
: "${HELIUS_API_KEY:?HELIUS_API_KEY must be exported (rotate the leaked key first)}"
export LASERSTREAM_ENDPOINT="https://laserstream-mainnet-lax.helius-rpc.com"
export WALLET_ADDRESS="7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC"
export TRAINING_CAPTURE_DIR="$DATA_DIR"

elapsed=0
restarts=0
prev_raw_bytes=0
stall_count=0

while [ $elapsed -lt $MAX_SECS ]; do
    # ── Check for manifest (completion signal) ──
    MANIFEST=$(ls "${DATA_DIR}/pumpfun_laserstream_manifest_v1_${SESSION}.json" 2>/dev/null | head -1)
    if [ -n "$MANIFEST" ]; then
        echo "[MONITOR $(date -u +%H:%M)] ✅ MANIFEST FOUND — capture completed normally!"
        echo "[MONITOR] Manifest: $MANIFEST"
        break
    fi

    # ── Check process health ──
    if ! pgrep -f pq-laserstream-grpc > /dev/null 2>&1; then
        if [ $restarts -ge $RESTART_MAX ]; then
            echo "[MONITOR $(date -u +%H:%M)] ❌ CRITICAL: Process died, max restarts ($RESTART_MAX) reached. Capture FAILED."
            break
        fi
        echo "[MONITOR $(date -u +%H:%M)] ⚠️ Process died without manifest. Auto-restarting ($((restarts+1))/$RESTART_MAX)..."
        restarts=$((restarts + 1))
        cd "$BIN_DIR"
        nohup target/release/pq-laserstream-grpc --training-capture --duration-minutes 300 >> "${DATA_DIR}/capture_300min.log" 2>&1 &
        echo "[MONITOR] New PID=$!"
        sleep 12
        # Capture new session ID from log
        NEW_SESSION=$(grep "Session:" "${DATA_DIR}/capture_300min.log" 2>/dev/null | tail -1 | grep -oE '[0-9]{8}_[0-9]{6}_[0-9]{6}')
        if [ -n "$NEW_SESSION" ] && [ "$NEW_SESSION" != "$SESSION" ]; then
            SESSION="$NEW_SESSION"
            echo "[MONITOR] Tracking new session: $SESSION"
        fi
        stall_count=0
        prev_raw_bytes=0
    fi

    # ── Check data growth (stall detection) ──
    raw_bytes=$(du -sb "${DATA_DIR}/pumpfun_laserstream_raw_v1_"*.ndjson.zst 2>/dev/null | awk '{sum+=$1} END {print sum+0}')
    events=$(wc -l "${DATA_DIR}/pumpfun_laserstream_events_v1_"*.ndjson 2>/dev/null | awk '{print $1+0}')

    if [ "$raw_bytes" = "$prev_raw_bytes" ] && [ "$raw_bytes" -gt 0 ]; then
        stall_count=$((stall_count + 1))
        echo "[MONITOR $(date -u +%H:%M)] ⚠️ No raw file growth for $((stall_count * POLL / 60)) min (stall_count=$stall_count)"
        if [ $stall_count -ge 5 ]; then
            echo "[MONITOR $(date -u +%H:%M)] 🔴 STALLED 15+ min — stream may be dead. The binary's 90s timeout should trigger reconnect. If no recovery in next poll, will kill+restart."
        fi
    else
        stall_count=0
    fi
    prev_raw_bytes=$raw_bytes

    # ── Report status ──
    latest_stats=$(grep "stats" "${DATA_DIR}/capture_300min.log" 2>/dev/null | tail -1)
    raw_human=$(du -shc "${DATA_DIR}/pumpfun_laserstream_raw_v1_"*.ndjson.zst 2>/dev/null | tail -1 | awk '{print $1}')
    echo "[MONITOR $(date -u +%H:%M)] elapsed=$((elapsed/60))min restarts=$restarts stalls=$stall_count raw=${raw_human} events=${events}"
    echo "  ${latest_stats:-<no stats yet>}"

    sleep $POLL
    elapsed=$((elapsed + POLL))
done

echo ""
echo "[MONITOR] === MONITORING ENDED ==="
echo "[MONITOR] Total elapsed: $((elapsed/60)) minutes"
echo "[MONITOR] Auto-restarts: $restarts"

# Print final file inventory
echo ""
echo "[MONITOR] === FINAL FILE INVENTORY ==="
ls -lh "${DATA_DIR}/pumpfun_laserstream_"* 2>/dev/null

# If manifest exists, print its summary
if [ -n "$MANIFEST" ]; then
    echo ""
echo "[MONITOR] === MANIFEST SUMMARY ==="
    python3 -m json.tool "$MANIFEST" 2>/dev/null | head -40
fi
