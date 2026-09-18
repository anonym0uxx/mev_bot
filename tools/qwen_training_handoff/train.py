#!/usr/bin/env python3
"""
Qwen 27B Full-Parameter BF16 Training — Self-Contained Entrypoint
==================================================================
Primary route: Unsloth + Accelerate + DeepSpeed ZeRO-3 on 3×96GB GPUs
NO Hermes/LLM calls required. Fully self-contained.

Usage:
  accelerate launch --config_file accelerate_config.yaml train.py --phase cpt
  accelerate launch --config_file accelerate_config.yaml train.py --phase sft

Phases:
  cpt  — Continued pre-training on qwen_cpt_v1 (~34M tokens)
  sft  — Supervised fine-tuning on qwen_sft_v1 (~15M tokens)
"""
import os
import sys
import json
import logging
import argparse
import traceback
from pathlib import Path

import torch
from datasets import load_dataset
from transformers import (
    AutoModelForCausalLM,
    AutoTokenizer,
    TrainingArguments,
    DataCollatorForLanguageModeling,
    set_seed,
)

# Optional Unsloth acceleration
try:
    from unsloth import FastLanguageModel
    UNSLOTH_AVAILABLE = True
except ImportError:
    UNSLOTH_AVAILABLE = False

logger = logging.getLogger(__name__)
logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")

# ════════════════════════════════════════════════════════════════
# CONFIG
# ════════════════════════════════════════════════════════════════

SCRIPT_DIR = Path(__file__).parent.resolve()
TRAINING_ARGS_PATH = SCRIPT_DIR / "training_args.json"

# Export paths — set these to the certified export locations
DEFAULT_EXPORT_DIR = Path("D:/repos/mev_bot/tools/data-pipeline/output/qwen_curriculum_v1")
CPT_FILE = DEFAULT_EXPORT_DIR / "cpt" / "qwen_cpt_v1.jsonl"
SFT_FILE = DEFAULT_EXPORT_DIR / "sft" / "qwen_sft_v1.jsonl"
EVAL_FILE = DEFAULT_EXPORT_DIR / "eval" / "qwen_eval_v1.jsonl"

MODEL_NAME = "Qwen/Qwen2.5-27B"


def load_training_args(phase: str) -> dict:
    """Load base training args and apply phase-specific overrides."""
    with open(TRAINING_ARGS_PATH) as f:
        args = json.load(f)

    # Apply SFT overrides if phase is sft
    if phase == "sft":
        overrides = args.get("_sft_overrides", {})
        for k, v in overrides.items():
            if not k.startswith("_"):
                args[k] = v

    # Remove comment fields
    return {k: v for k, v in args.items() if not k.startswith("_")}


def load_jsonl_dataset(filepath: str, text_field: str = "text", max_seq_length: int = 4096):
    """Load a JSONL dataset for training.
    For CPT: each line has {"text": "..."} — raw text for LM training.
    For SFT: each line has {"prompt": "...", "completion": "..."} — instruction format.
    """
    if not os.path.exists(filepath):
        raise FileNotFoundError(f"Training data not found: {filepath}")

    def gen():
        with open(filepath, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                yield json.loads(line)

    ds = load_dataset("json", data_files=filepath, split="train", streaming=False)
    return ds


def format_sft_example(example, tokenizer, max_seq_length):
    """Format SFT example: prompt + completion -> input_ids + labels."""
    prompt = example.get("prompt", "")
    completion = example.get("completion", "")
    full_text = prompt + completion

    # Tokenize prompt and full text separately for masked loss
    prompt_ids = tokenizer(prompt, truncation=True, max_length=max_seq_length)["input_ids"]
    full_ids = tokenizer(full_text, truncation=True, max_length=max_seq_length)["input_ids"]

    labels = full_ids.copy()
    # Mask prompt tokens (set to -100)
    for i in range(min(len(prompt_ids), len(labels))):
        labels[i] = -100

    return {"input_ids": full_ids, "labels": labels, "attention_mask": [1] * len(full_ids)}


def main():
    parser = argparse.ArgumentParser(description="Qwen 27B Full-FT Training")
    parser.add_argument("--phase", choices=["cpt", "sft"], required=True,
                        help="Training phase: cpt (pre-training) or sft (fine-tuning)")
    parser.add_argument("--resume_from_checkpoint", type=str, default=None,
                        help="Path to checkpoint directory to resume from")
    parser.add_argument("--data_path", type=str, default=None,
                        help="Override path to training data JSONL")
    parser.add_argument("--output_dir", type=str, default=None,
                        help="Override output directory")
    parser.add_argument("--dry_run", action="store_true",
                        help="Load data + model but don't start training")
    args = parser.parse_args()

    # Load training config
    config = load_training_args(args.phase)

    if args.output_dir:
        config["output_dir"] = args.output_dir

    # Determine data path
    if args.data_path:
        data_path = args.data_path
    elif args.phase == "cpt":
        data_path = str(CPT_FILE)
    else:
        data_path = str(SFT_FILE)

    logger.info("=" * 60)
    logger.info(f"Qwen 27B Full-Parameter BF16 Training")
    logger.info(f"Phase: {args.phase.upper()}")
    logger.info(f"Data: {data_path}")
    logger.info(f"Output: {config['output_dir']}")
    logger.info(f"Unsloth: {'available' if UNSLOTH_AVAILABLE else 'not available — using HF Trainer'}")
    logger.info(f"GPUs: {torch.cuda.device_count()}")
    for i in range(torch.cuda.device_count()):
        logger.info(f"  GPU {i}: {torch.cuda.get_device_name(i)} "
                    f"({torch.cuda.get_device_properties(i).total_mem / 1e9:.1f} GB)")
    logger.info("=" * 60)

    # Verify data exists
    if not os.path.exists(data_path):
        logger.error(f"Training data not found: {data_path}")
        logger.error("Ensure exports are generated and certified before training.")
        sys.exit(1)

    # Load dataset
    logger.info(f"Loading dataset from {data_path}...")
    ds = load_dataset("json", data_files=data_path, split="train")
    logger.info(f"Dataset loaded: {len(ds)} examples")

    # Load tokenizer
    logger.info(f"Loading tokenizer: {MODEL_NAME}")
    tokenizer = AutoTokenizer.from_pretrained(MODEL_NAME, trust_remote_code=True)
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    # Load model
    logger.info(f"Loading model: {MODEL_NAME}")
    if UNSLOTH_AVAILABLE:
        model, tokenizer = FastLanguageModel.from_pretrained(
            MODEL_NAME,
            max_seq_length=config.get("max_seq_length", 4096),
            dtype=torch.bfloat16,
        )
        model = FastLanguageModel.for_training(model)
    else:
        model = AutoModelForCausalLM.from_pretrained(
            MODEL_NAME,
            torch_dtype=torch.bfloat16,
            trust_remote_code=True,
            attn_implementation="flash_attention_2" if os.environ.get("USE_FLASH_ATTENTION") else "sdpa",
        )

    # Tokenize dataset
    max_seq_length = config.get("max_seq_length", 4096)

    if args.phase == "cpt":
        # CPT: use raw text field, packing enabled
        if "text" not in ds.column_names:
            # If no 'text' column, concatenate available fields
            def to_text(example):
                if "text" in example:
                    return {"text": example["text"]}
                # Fallback: join all string fields
                text_parts = [str(v) for v in example.values() if isinstance(v, (str, int, float))]
                return {"text": "\n".join(text_parts)}
            ds = ds.map(to_text)
    else:
        # SFT: format as prompt-completion pairs
        def format_example(example):
            return format_sft_example(example, tokenizer, max_seq_length)
        ds = ds.map(format_example, remove_columns=ds.column_names)

    logger.info(f"Dataset prepared for {args.phase}")

    # Training arguments
    training_args_kwargs = {
        "output_dir": config["output_dir"],
        "num_train_epochs": config.get("num_train_epochs", 3),
        "per_device_train_batch_size": config.get("per_device_train_batch_size", 2),
        "gradient_accumulation_steps": config.get("gradient_accumulation_steps", 16),
        "learning_rate": config.get("learning_rate", 5e-5),
        "lr_scheduler_type": config.get("lr_scheduler_type", "cosine"),
        "warmup_ratio": config.get("warmup_ratio", 0.05),
        "weight_decay": config.get("weight_decay", 0.01),
        "max_grad_norm": config.get("max_grad_norm", 1.0),
        "bf16": config.get("bf16", True),
        "gradient_checkpointing": config.get("gradient_checkpointing", True),
        "logging_steps": config.get("logging_steps", 10),
        "save_steps": config.get("save_steps", 200),
        "save_total_limit": config.get("save_total_limit", 5),
        "save_strategy": config.get("save_strategy", "steps"),
        "seed": config.get("seed", 42),
        "data_seed": config.get("data_seed", 42),
        "dataloader_num_workers": config.get("dataloader_num_workers", 4),
        "dataloader_pin_memory": config.get("dataloader_pin_memory", True),
        "report_to": config.get("report_to", "none"),
        "remove_unused_columns": config.get("remove_unused_columns", False),
    }

    if args.resume_from_checkpoint:
        training_args_kwargs["resume_from_checkpoint"] = args.resume_from_checkpoint

    training_args = TrainingArguments(**training_args_kwargs)

    # Data collator
    if args.phase == "cpt":
        data_collator = DataCollatorForLanguageModeling(
            tokenizer=tokenizer,
            mlm=False,
            padding=True,
        )
    else:
        # SFT: custom collator that pads input_ids and labels
        from transformers import DataCollatorForSeq2Seq
        data_collator = DataCollatorForSeq2Seq(
            tokenizer=tokenizer,
            padding=True,
            return_tensors="pt",
        )

    # Trainer
    from transformers import Trainer
    trainer = Trainer(
        model=model,
        args=training_args,
        train_dataset=ds,
        data_collator=data_collator,
    )

    if args.dry_run:
        logger.info("Dry run — data + model loaded successfully. Not starting training.")
        logger.info(f"Dataset size: {len(ds)}")
        logger.info(f"Model: {model.config.model_type}")
        logger.info(f"Parameters: {sum(p.numel() for p in model.parameters()) / 1e9:.2f}B")
        return

    # ════════════════════════════════════════════════════════════════
    # EARLY-STEP FEASIBILITY VALIDATION
    # The real run's early steps ARE the feasibility check.
    # Monitor: ZeRO-3 sharding, VRAM, RAM, finite loss, gradients.
    # ════════════════════════════════════════════════════════════════
    logger.info("Starting training. Early steps = feasibility validation.")
    logger.info("Monitor: ZeRO-3 sharding, per-GPU VRAM, system RAM, finite loss, non-zero gradients.")
    logger.info("Checkpoint cadence: every 200 steps (early: every 10-20 for first 100 steps).")

    # Early checkpoint override — save more frequently at start
    if not args.resume_from_checkpoint:
        logger.info("First run — setting early checkpoint cadence (every 20 steps for first 500)")
        # We rely on save_steps=200 but also save on first few steps via callback
        from transformers import TrainerCallback

        class EarlyCheckpointCallback(TrainerCallback):
            def __init__(self):
                self.early_save_steps = {20, 50, 100}
                self.done = False

            def on_step_end(self, args, state, control, **kwargs):
                if not self.done and state.global_step in self.early_save_steps:
                    logger.info(f"Early checkpoint at step {state.global_step}")
                    control.should_save = True
                    if state.global_step >= 100:
                        self.done = True
                return control

        trainer.add_callback(EarlyCheckpointCallback())

    try:
        trainer.train(resume_from_checkpoint=args.resume_from_checkpoint)
    except torch.cuda.OutOfMemoryError as e:
        logger.error("CUDA OOM! Reduce per_device_train_batch_size or gradient_accumulation_steps.")
        logger.error(f"Error: {e}")
        logger.error("See RUNBOOK.md for OOM recovery procedures.")
        raise
    except Exception as e:
        logger.error(f"Training error: {e}")
        logger.error(traceback.format_exc())
        raise

    # Save final model
    logger.info("Training complete. Saving final model.")
    trainer.save_model(os.path.join(config["output_dir"], "final"))
    tokenizer.save_pretrained(os.path.join(config["output_dir"], "final"))

    logger.info("=" * 60)
    logger.info("TRAINING COMPLETE")
    logger.info(f"Final model: {os.path.join(config['output_dir'], 'final')}")
    logger.info("=" * 60)


if __name__ == "__main__":
    main()
