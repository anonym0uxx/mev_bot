#!/usr/bin/env bash
# quick VM state probe
free -g | sed -n 2p
findmnt -n /training || echo NO-MOUNT
nvidia-smi --query-gpu=memory.used,utilization.gpu --format=csv,noheader 2>/dev/null || echo NO-GPU
pgrep -cf train_qwen27b.py || true
