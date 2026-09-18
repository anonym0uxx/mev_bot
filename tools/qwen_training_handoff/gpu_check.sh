#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════
# GPU CHECK — Verify all 3 GPUs are free before training launch
# ════════════════════════════════════════════════════════════════
# Exits 0 if all 3 GPUs have <1GB VRAM used (free).
# Exits 1 if any GPU has significant VRAM in use.
# ════════════════════════════════════════════════════════════════

set -euo pipefail

echo "Checking GPU VRAM usage..."

if ! command -v nvidia-smi &> /dev/null; then
    echo "ERROR: nvidia-smi not found. NVIDIA drivers not installed?"
    exit 1
fi

GPU_COUNT=$(nvidia-smi --query-gpu=count --format=csv,noheader,nounits | head -1)
echo "  GPU count: $GPU_COUNT"

if [[ "$GPU_COUNT" -lt 3 ]]; then
    echo "ERROR: Expected 3 GPUs, found $GPU_COUNT"
    exit 1
fi

ALL_FREE=true

for i in 0 1 2; do
    NAME=$(nvidia-smi --query-gpu=name --format=csv,noheader -i $i)
    MEM_USED=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits -i $i)
    MEM_TOTAL=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits -i $i)
    MEM_USED_MB=$MEM_USED
    MEM_TOTAL_GB=$((MEM_TOTAL / 1024))
    MEM_USED_GB=$(echo "scale=1; $MEM_USED_MB / 1024" | bc)

    echo "  GPU $i: $NAME — VRAM: ${MEM_USED_MB}MiB / ${MEM_TOTAL_GB}GiB"

    if [[ "$MEM_USED_MB" -gt 1024 ]]; then
        echo "    WARNING: GPU $i has ${MEM_USED_MB}MiB in use (>1GB)"
        echo "    Processes on GPU $i:"
        nvidia-smi --query-compute-apps=pid,used_memory,process_name --format=csv -i $i 2>/dev/null || true
        ALL_FREE=false
    fi
done

echo ""

if [[ "$ALL_FREE" == "true" ]]; then
    echo "✓ ALL 3 GPUs are FREE. Ready for training."
    exit 0
else
    echo "✗ GPU VRAM in use. Stop Hermes + GLM/llama.cpp before training."
    echo "  To stop GLM: kill the llama-server process"
    echo "  To stop Hermes: hermes stop (or close the daemon)"
    exit 1
fi
