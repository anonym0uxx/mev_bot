#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════
# MONITOR — GPU/RAM monitoring during training
# Refresh every 5 seconds. Ctrl-C to stop.
# ════════════════════════════════════════════════════════════════

set -euo pipefail

INTERVAL="${1:-5}"

echo "Monitoring GPU/RAM every ${INTERVAL}s. Ctrl-C to stop."
echo ""

while true; do
    echo "=== $(date) ==="
    echo ""
    echo "--- GPU VRAM ---"
    nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu \
        --format=csv -i 0,1,2 2>/dev/null || nvidia-smi
    echo ""
    echo "--- System RAM ---"
    free -h 2>/dev/null || vm_stat 2>/dev/null | head -10 || echo "RAM info not available"
    echo ""
    echo "--- Training Processes ---"
    ps aux | grep -E "(train.py|accelerate)" | grep -v grep || echo "  No training processes running"
    echo ""
    sleep "$INTERVAL"
done
