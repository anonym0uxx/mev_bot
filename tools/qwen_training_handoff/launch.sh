#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════
# LAUNCH — Qwen 27B Full-Parameter BF16 Training
# One-command launch for the REAL Accelerate + DeepSpeed ZeRO-3 run.
# ════════════════════════════════════════════════════════════════
# PREREQUISITES:
#   1. Hermes stopped
#   2. GLM/llama.cpp stopped — VRAM verified free (run gpu_check.sh)
#   3. Python venv activated with requirements.txt installed
#   4. Training data exports certified (qwen_cpt_v1.jsonl / qwen_sft_v1.jsonl)
#
# USAGE:
#   ./launch.sh cpt    # Phase 1: Continued pre-training (~34M tokens)
#   ./launch.sh sft    # Phase 2: Supervised fine-tuning (~15M tokens)
# ════════════════════════════════════════════════════════════════

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

PHASE="${1:-cpt}"

if [[ "$PHASE" != "cpt" && "$PHASE" != "sft" ]]; then
    echo "ERROR: Phase must be 'cpt' or 'sft'"
    exit 1
fi

echo "========================================"
echo "Qwen 27B Full-FT — Phase: ${PHASE^^}"
echo "========================================"
echo "Script dir: $SCRIPT_DIR"
echo "Time: $(date)"
echo ""

# Pre-flight checks
echo "Pre-flight: GPU check..."
if ! ./gpu_check.sh; then
    echo "ERROR: GPU check failed. Ensure Hermes + GLM are stopped."
    exit 1
fi

echo ""
echo "Pre-flight: Data check..."
if [[ "$PHASE" == "cpt" ]]; then
    DATA_FILE="../data-pipeline/output/qwen_curriculum_v1/cpt/qwen_cpt_v1.jsonl"
else
    DATA_FILE="../data-pipeline/output/qwen_curriculum_v1/sft/qwen_sft_v1.jsonl"
fi

if [[ ! -f "$DATA_FILE" ]]; then
    echo "ERROR: Training data not found: $DATA_FILE"
    echo "Ensure qwen_curriculum_v1 exports are generated and certified."
    exit 1
fi

FILE_SIZE=$(stat --format="%s" "$DATA_FILE" 2>/dev/null || stat -f%z "$DATA_FILE" 2>/dev/null || echo "0")
echo "  Data file: $DATA_FILE ($FILE_SIZE bytes)"

echo ""
echo "Launching training..."
echo "  Config: accelerate_config.yaml"
echo "  DeepSpeed: deepspeed_config.json"
echo "  Args: training_args.json"
echo ""

export TOKENIZERS_PARALLELISM=false
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True

accelerate launch \
    --config_file accelerate_config.yaml \
    --machine_rank 0 \
    --main_process_port 29501 \
    train.py \
    --phase "$PHASE" \
    --data_path "$DATA_FILE" \
    2>&1 | tee "logs/train_${PHASE}_$(date +%Y%m%d_%H%M%S).log"

echo ""
echo "Training process ended at $(date)"
