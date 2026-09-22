#!/usr/bin/env python
"""RL READINESS GATE - one command that says whether we may train RL yet.

Runs every reward/mechanics/GRPO suite and checks the artifacts the RL run needs,
then prints a single verdict with explicit blockers. Nothing here touches the
GPUs while SFT owns them (all checks are CPU-only except an OPTIONAL checkpoint
probe, which is skipped unless --probe-ckpt is passed).

Exit 0 = ready, 1 = blocked (blockers listed).

Usage:
  python rl_readiness.py            # suites + artifact checks
  python rl_readiness.py --probe-ckpt /training/runs/sft-004/<best>  # + load test
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable

SUITES = [
    ("regime_pricing", [PY, "test_regime_pricing.py"]),
    ("causality_discipline", [PY, "test_causality_discipline.py"]),
    ("reward_intent", [PY, "test_reward_intent.py"]),
    ("reward_terms", [PY, "reward_terms.py", "--self-check"]),
    ("exit_mechanics", [PY, "exit_mechanics.py", "--self-check"]),
    ("exit_policy", [PY, "exit_policy.py", "--self-check"]),
    ("grpo_bridge", [PY, "grpo_reward_bridge.py", "--self-check"]),
    ("grpo_trainer", [PY, "grpo_trainer.py", "--self-check"]),
    ("forward_amm_on_real_reserves", [PY, "validate_forward_amm.py"]),
    # v3 engine + RL harness (added 2026-09-13)
    ("rl_reward_v3", [PY, "rl_reward_v3.py", "--selftest"]),
    ("v3_bridge", [PY, "v3_bridge.py", "--selftest"]),
    ("grpo_loss", [PY, "grpo_loss.py", "--self-check"]),
    ("grpo_loop", [PY, "grpo_loop.py", "--selfcheck"]),
    ("grpo_dataset", [PY, "grpo_dataset.py", "--smoke"]),
]

# artifacts RL needs, with the reason it needs them
ARTIFACTS = [
    ("/training/v2/reports/FORWARD_AMM_VALIDATION.json",
     "AMM path validated on real captured reserves"),
    ("/training/v2/reserves/forward/pumpswap_pool_reserves_v1_part0000.parquet",
     "forward pool-reserve capture the oracle prices from"),
    ("/training/v2/reports/EXIT_FAMILY_ESTIMATE.json",
     "dG/dlnx + tail-index measurement that settles trail-vs-ladder BEFORE RL"),
    ("/training/v2/reports/FORWARD_AMM_VALIDATION.md",
     "human-readable AMM validation record"),
]

SFT_RUNS = ["/training/runs/sft-004", "/training/runs/sft-003"]


def run_suite(name, cmd):
    try:
        p = subprocess.run(cmd, cwd=HERE, capture_output=True, text=True, timeout=1800)
    except subprocess.TimeoutExpired:
        return {"suite": name, "ok": False, "reason": "timeout"}
    tail = (p.stdout or "").strip().splitlines()[-1:] or [""]
    return {"suite": name, "ok": p.returncode == 0, "rc": p.returncode,
            "last": tail[0][:200]}


def find_best_checkpoint():
    """The SFT BEST-VALIDATION checkpoint, from the run's own trainer_state.json.

    REPLACED 2026-09-21. The old body scanned two hardcoded runs (sft-004/sft-003 - stale
    while sft-013 was the live run) and returned the most-recently-MODIFIED checkpoint dir.
    That is wrong twice over: "best" for SFT is best-VALIDATION (and ties resolve EARLIER),
    and mtime picks the newest. It also accepted any dir holding a `.bin` file, which an
    FSDP2 checkpoint satisfies with its OPTIMIZER shards alone - a false positive that would
    hand RL a non-loadable init. Authority is now trainer_state.json's best_model_checkpoint,
    cross-checked against val_history.jsonl's lowest val_loss (disagreement = refusal).
    """
    try:
        if HERE not in sys.path:
            sys.path.insert(0, HERE)
        from sft_checkpoint_handoff import resolve
        res = resolve()
        if res.get("ok"):
            return res["best"]["dir"]
        print(f"[readiness] best-checkpoint resolution REFUSED: "
              f"{res.get('reason') or res.get('DISAGREEMENT')}")
    except Exception as exc:                                      # noqa: BLE001
        print(f"[readiness] best-checkpoint resolution FAILED: {exc!r}")
    return None


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--probe-ckpt", default=None)
    a = ap.parse_args()

    results = [run_suite(n, c) for n, c in SUITES]
    blockers = [f"suite_failed:{r['suite']}" for r in results if not r["ok"]]

    artifacts = []
    for path, why in ARTIFACTS:
        ok = os.path.exists(path)
        artifacts.append({"path": path, "why": why, "present": ok})
        if not ok:
            blockers.append(f"missing_artifact:{os.path.basename(path)}")

    ckpt = a.probe_ckpt or find_best_checkpoint()
    sft = {"best_checkpoint": ckpt,
           "status": "found" if ckpt else "PENDING - SFT has not written a checkpoint yet"}

    cache = os.path.join(HERE, "ref_cache.jsonl")
    ref = {"cache": cache, "present": os.path.exists(cache),
           "note": "build with grpo_ref_cache.py once the SFT best checkpoint exists"}
    # the ref cache is a HARD blocker only if a checkpoint already exists
    if ckpt and not ref["present"]:
        blockers.append("missing_ref_logprob_cache")

    out = {"ready_for_rl": not blockers, "blockers": blockers,
           "suites": results, "artifacts": artifacts, "sft_checkpoint": sft,
           "ref_cache": ref}
    print(json.dumps(out, indent=1))
    return 0 if not blockers else 1


if __name__ == "__main__":
    sys.exit(main())