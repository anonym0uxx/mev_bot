#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════
# RESUME — Qwen 27B Full-FT from checkpoint
# One-command resume from the latest or specified checkpoint.
# ════════════════════════════════════════════════════════════════
# USAGE:
#   ./resume.sh cpt                  # Resume latest CPT checkpoint
#   ./resume.sh sft                  # Resume latest SFT checkpoint
#   ./resume.sh cpt ./output/qwen_cpt/checkpoint-400   # Resume specific
# ════════════════════════════════════════════════════════════════

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

PHASE="${1:-cpt}"
CHECKPOINT="${2:-}"

if [[ "$PHASE" != "cpt" && "$PHASE" != "sft" ]]; then
    echo "ERROR: Phase must be 'cpt' or 'sft'"
    exit 1
fi

# Auto-detect latest checkpoint if not specified
if [[ -z "$CHECKPOINT" ]]; then
    if [[ "$PHASE" == "cpt" ]]; then
        CKPT_DIR="./output/qwen_cpt"
    else
        CKPT_DIR="./output/qwen_sft"
    fi

    if [[ -d "$CKPT_DIR" ]]; then
        # Find latest checkpoint
        CHECKPOINT=$(ls -d "${CKPT_DIR}"/checkpoint-* 2>/dev/null | sort -t- -k2 -n | tail -1)
        if [[ -z "$CHECKPOINT" ]]; then
            echo "ERROR: No checkpoint found in $CKPT_DIR"
            exit 1
        fi
        echo "Auto-detected latest checkpoint: $CHECKPOINT"
    else
        echo "ERROR: Checkpoint directory not found: $CKPT_DIR"
        exit 1
    fi
fi

echo "========================================"
echo "Qwen 27B Full-FT — RESUME"
echo "Phase: ${PHASE^^}"
echo "Checkpoint: $CHECKPOINT"
echo "Time: $(date)"
echo "========================================"

# Pre-flight GPU check
echo "Pre-flight: GPU check..."
if ! ./gpu_check.sh; then
    echo "ERROR: GPU check failed."
    exit 1
fi

if [[ "$PHASE" == "cpt" ]]; then
    DATA_FILE="../data-pipeline/output/qwen_curriculum_v1/cpt/qwen_cpt_v1.jsonl"
else
    DATA_FILE="../data-pipeline/output/qwen_curriculum_v1/sft/qwen_sft_v1.jsonl"
fi

export TOKENIZERS_PARALLELISM=false
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True

accelerate launch \
    --config_file accelerate_config.yaml \
    --machine_rank 0 \
    --main_process_port 29501 \
    train.py \
    --phase "$PHASE" \
    --data_path "$DATA_FILE" \
    --resume_from_checkpoint "$CHECKPOINT" \
    2>&1 | tee "logs/resume_${PHASE}_$(date +%Y%m%d_%H%M%S).log"

echo ""
echo "Resume process ended at $(date)"
