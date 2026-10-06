#!/usr/bin/env python
"""rl_plan — the transition gate: is RL cleared to start, and with what command?

Run this the moment SFT writes a best checkpoint. It distinguishes three states,
because conflating them is how a handoff goes wrong:

  READY    every prerequisite is present and verified
  PENDING  a prerequisite that can only exist AFTER SFT (the best checkpoint, the
           reference cache built from it). Not an error, just not yet.
  BLOCKED  something that is wrong NOW and needs a human

Exit 0 only if nothing is BLOCKED. PENDING items print the exact command that
creates them, so the transition is mechanical rather than remembered.

All checks are CPU-only: SFT owns the GPUs, and this must be runnable during it.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable

WALL = "/training/v2/reports/FORWARD_WALL_V1.json"
RL_OUT = "/training/v2/rl_targets_v3"
RL_TRAIN = os.path.join(RL_OUT, "rl_train.jsonl")
RL_VAL = os.path.join(RL_OUT, "rl_validation.jsonl")
REF_CACHE = "/training/runs/rl-001/ref_action_dist.jsonl"
SFT_RUNS = "/training/runs"
CFG = os.path.join(HERE, "configs", "rl_v1.json")

# suites that must pass before a single RL step is allowed
SUITES = [
    ("rl_reward_v3", [PY, "rl_reward_v3.py", "--selftest"]),
    ("v3_bridge", [PY, "v3_bridge.py", "--selftest"]),
    ("grpo_loss", [PY, "grpo_loss.py", "--self-check"]),
    ("grpo_trainer", [PY, "grpo_trainer.py", "--self-check"]),
    ("grpo_dataset", [PY, "grpo_dataset.py", "--smoke"]),
    ("regime_pricing", [PY, "test_regime_pricing.py"]),
    ("causality_discipline", [PY, "test_causality_discipline.py"]),
    ("reward_intent", [PY, "test_reward_intent.py"]),
    ("reward_terms", [PY, "reward_terms.py", "--self-check"]),
    ("exit_policy", [PY, "exit_policy.py", "--self-check"]),
    ("forward_amm_on_real_reserves", [PY, "validate_forward_amm.py"]),
]


def run_suite(name, cmd, timeout=1800):
    try:
        p = subprocess.run(cmd, cwd=HERE, capture_output=True, text=True,
                           timeout=timeout)
        out = (p.stdout or "") + (p.stderr or "")
        verdict = "PASS" if p.returncode == 0 and '"FAIL"' not in out else "FAIL"
        return {"suite": name, "verdict": verdict, "rc": p.returncode,
                "tail": out.strip().splitlines()[-1][:200] if out.strip() else ""}
    except subprocess.TimeoutExpired:
        return {"suite": name, "verdict": "BLOCKED", "rc": -1,
                "tail": f"timeout after {timeout}s"}


def best_ckpt():
    """Find the newest SFT best checkpoint, if one exists yet."""
    cands = []
    for root, dirs, files in os.walk(SFT_RUNS):
        if root.count(os.sep) - SFT_RUNS.count(os.sep) > 4:
            dirs[:] = []
            continue
        base = os.path.basename(root).lower()
        if base in ("best", "best_ckpt", "best_checkpoint") or base.startswith("best-"):
            if any(f.endswith((".safetensors", ".bin")) for f in files):
                cands.append(root)
    return sorted(cands, key=lambda p: os.path.getmtime(p))[-1] if cands else None


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--skip-suites", action="store_true",
                    help="artifact checks only (fast)")
    ap.add_argument("--json", default="")
    a = ap.parse_args(argv)

    ready, pending, blocked, info = [], [], [], {}

    # --- wall -----------------------------------------------------------------
    if not os.path.isfile(WALL):
        blocked.append(f"forward wall missing: {WALL}")
    else:
        w = json.load(open(WALL, encoding="utf-8"))
        info["wall_ms"] = w.get("wall_ms")
        info["wall_reserved_rows"] = w.get("reserved_rows")
        ready.append(f"forward wall pinned ({w.get('reserved_rows')} eps, "
                     f"{w.get('wall_hours')} h) - RL eval set is immutable")

    # --- libraries ------------------------------------------------------------
    for m in ("torch", "transformers", "accelerate", "deepspeed"):
        try:
            __import__(m)
            ready.append(f"lib {m} importable")
        except Exception as e:                                  # noqa: BLE001
            blocked.append(f"lib {m} NOT importable: {e}")
    for m in ("trl", "peft"):
        try:
            __import__(m)
            blocked.append(f"lib {m} is present: this stack deliberately does NOT "
                           f"use it (raw torch loop) - remove to avoid drift")
        except Exception:                                       # noqa: BLE001
            ready.append(f"lib {m} absent (expected)")

    # --- dataset --------------------------------------------------------------
    for label, path in (("train", RL_TRAIN), ("validation", RL_VAL)):
        if os.path.isfile(path) and os.path.getsize(path) > 0:
            n = sum(1 for _ in open(path, encoding="utf-8"))
            info[f"rl_{label}_records"] = n
            (ready if n > 0 else blocked).append(
                f"RL {label} records: {n}" if n > 0 else f"RL {label} empty: {path}")
        else:
            pending.append(f"RL {label} dataset not built yet -> "
                           f"{PY} grpo_dataset.py --split {label}")

    # --- config ---------------------------------------------------------------
    if os.path.isfile(CFG):
        cfg = json.load(open(CFG, encoding="utf-8"))
        info["rl_config"] = cfg.get("run_id")
        def dotted(d, path):
            cur = d
            for part in path.split("."):
                if not isinstance(cur, dict) or part not in cur:
                    return False
                cur = cur[part]
            return True
        missing = [k for k in ("run_id", "init_from", "loop.kl_coef", "optim.lr",
                              "loop.group_size", "gate.gate_floor_net_sol",
                              "data.train", "data.wall_ms")
                   if not dotted(cfg, k)]
        if missing:
            blocked.append(f"rl config missing keys: {missing}")
        else:
            ready.append(f"rl config present ({cfg.get('run_id')})")
    else:
        pending.append(f"rl config not written yet -> {CFG}")

    # --- best checkpoint (the only true dependency on SFT) -------------------
    bc = best_ckpt()
    if bc:
        info["sft_best_checkpoint"] = bc
        ready.append(f"SFT best checkpoint found: {bc}")
        if os.path.isfile(REF_CACHE):
            mf = REF_CACHE + ".manifest.json"
            if os.path.isfile(mf):
                info["ref_cache_rows"] = json.load(open(mf, encoding="utf-8")).get(
                    "rows_written")
            ready.append("reference action-distribution cache present")
        else:
            pending.append("reference cache not built -> "
                           f"{PY} grpo_ref_cache.py --action-dist --ckpt {bc} "
                           f"--prompts {RL_TRAIN} --out {REF_CACHE}")
    else:
        pending.append("no SFT best checkpoint yet (SFT still running) - "
                       "the reference cache and RL init both depend on it")

    # --- suites ---------------------------------------------------------------
    suites = []
    if not a.skip_suites:
        for name, cmd in SUITES:
            r = run_suite(name, cmd)
            suites.append(r)
            if r["verdict"] == "PASS":
                ready.append(f"suite {name}: PASS")
            else:
                blocked.append(f"suite {name}: {r['verdict']} ({r['tail']})")

    out = {"ready": ready, "pending": pending, "blocked": blocked, "info": info,
           "suites": suites, "state": ("BLOCKED" if blocked else
                                       ("READY" if not pending else "PENDING")),
           "next_command": (None if blocked else
                            (f"{PY} launch_rl.py --config {CFG} --execute"
                             if not pending else
                             "complete the PENDING steps above, then re-run"))}
    print(json.dumps(out, indent=1, default=str))
    if a.json:
        json.dump(out, open(a.json, "w", encoding="utf-8"), indent=2, default=str)
    return 1 if blocked else 0


if __name__ == "__main__":
    sys.exit(main())
