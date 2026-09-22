#!/usr/bin/env python
"""sft_to_rl_handoff — automatic SFT -> RL transition. Completes the manual step.

WHY THIS EXISTS: the transition was "fill init_from when SFT writes best, then
build the reference cache, then launch". Done by hand that is three commands, two
of which are easy to get wrong at 3am (which checkpoint is "best"? is it really
the best_val_loss one? is the ref cache keyed to the same prompt hash?). This
script does it deterministically and writes a receipt.

HOW "BEST" IS KNOWN (verified against train_qwen27b.py::ValHistoryCallback):
the SFT run appends one JSON line per internal validation to
    <sft_out_dir>/val_history.jsonl
with keys step / val_loss / best_step_so_far / best_val_loss_so_far. The best
checkpoint is therefore <sft_out_dir>/checkpoint-<best_step_so_far>. We pick the
row with the LOWEST val_loss across the file and require best_step_so_far to agree
with it - if they disagree we STOP, because that means two different definitions of
"best" are in play and silently picking one would poison the RL init.

STAGES (idempotent, resumable, each writes its own receipt):
  ckpt   resolve + verify the SFT best checkpoint, write init_from into the config
  ref    build the reference ACTION-distribution cache from that checkpoint
  plan   run rl_plan.py and report the transition state

Modes:
  --once    run the requested stages once (default: all)
  --wait    poll until the best checkpoint exists, then run the stages
  --status  report only, change nothing
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable
CFG = os.path.join(HERE, "configs", "rl_v1.json")
SFT_OUT = "/training/runs/sft-004/astra_north_star_v3_linux_sft"
STATE = "/training/runs/rl-001"
MODEL_BYTES_27B_BF16 = 56 * 1024**3        # weights + activations headroom


def read_val_history(sft_out: str):
    p = os.path.join(sft_out, "val_history.jsonl")
    if not os.path.isfile(p):
        return None, f"no val_history.jsonl yet at {p} (SFT has not run an eval)"
    rows = []
    for line in open(p, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:                                       # noqa: BLE001
            continue
        if "val_loss" in r and r.get("step") is not None:
            rows.append(r)
    if not rows:
        return None, "val_history.jsonl has no usable rows"
    return rows, None


def resolve_best(sft_out: str):
    """Return (checkpoint_dir, evidence) or (None, reason)."""
    rows, err = read_val_history(sft_out)
    if rows is None:
        return None, err
    best = min(rows, key=lambda r: float(r["val_loss"]))
    reported = best.get("best_step_so_far")
    if reported is None:
        return None, ("val_history has no best_step_so_far: checkpoint SELECTION "
                      "was never enabled, so there is no 'best' to transition to")
    if int(reported) != int(best["step"]):
        return None, (f"two definitions of best disagree: min(val_loss) is at step "
                      f"{best['step']} but best_step_so_far={reported}. Refusing to "
                      f"pick one silently.")
    ckpt = os.path.join(sft_out, f"checkpoint-{int(best['step'])}")
    if not os.path.isdir(ckpt):
        return None, f"best checkpoint directory does not exist yet: {ckpt}"
    weights = [f for f in os.listdir(ckpt) if f.endswith((".safetensors", ".bin"))]
    if not weights:
        return None, f"checkpoint has no weight files: {ckpt}"
    tok_files = [f for f in os.listdir(ckpt)
                 if f in ("tokenizer.json", "tokenizer_config.json")]
    ev = {"step": int(best["step"]), "val_loss": float(best["val_loss"]),
          "val_ppl": best.get("val_ppl"), "epoch": best.get("epoch"),
          "checkpoint": ckpt, "n_weight_files": len(weights),
          "has_tokenizer_files": len(tok_files) > 0,
          "history_rows": len(rows)}
    return ckpt, ev


def stage_ckpt(cfg_path: str, sft_out: str) -> dict:
    ckpt, ev = resolve_best(sft_out)
    if ckpt is None:
        return {"stage": "ckpt", "ok": False, "reason": ev}
    cfg = json.load(open(cfg_path, encoding="utf-8"))
    already = cfg.get("init_from") == ckpt
    if not already:
        bak = cfg_path + ".bak"
        if not os.path.isfile(bak):
            shutil.copy2(cfg_path, bak)
        cfg["init_from"] = ckpt
        tmp = cfg_path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(cfg, fh, indent=2)
        os.replace(tmp, cfg_path)
    return {"stage": "ckpt", "ok": True, "init_from": ckpt,
            "changed": not already, "evidence": ev}


def sft_running() -> bool:
    """True while an SFT trainer process is alive."""
    for pat in ("train_qwen27b.py --phase sft", "train_qwen27b.py"):
        r = subprocess.run(["pgrep", "-f", pat], capture_output=True, text=True)
        if r.returncode == 0 and r.stdout.strip():
            return True
    return False


def gpu_free_bytes():
    try:
        out = subprocess.check_output(
            ["nvidia-smi", "--query-gpu=memory.free",
             "--format=csv,noheader,nounits"], text=True)
        return [int(x.strip()) * 1024**2 for x in out.strip().splitlines()]
    except Exception:                                           # noqa: BLE001
        return []


def stage_ref(cfg_path: str) -> dict:
    cfg = json.load(open(cfg_path, encoding="utf-8"))
    ckpt = cfg.get("init_from") or ""
    if not ckpt or "BEST" in ckpt or not os.path.isdir(ckpt):
        return {"stage": "ref", "ok": False,
                "reason": f"init_from not resolved: {ckpt!r}"}
    out = (cfg.get("reference") or {}).get("action_dist_cache") or \
        os.path.join(STATE, "ref_action_dist.jsonl")
    if os.path.isfile(out) and os.path.isfile(out + ".manifest.json"):
        return {"stage": "ref", "ok": True, "cache": out, "skipped": "already built"}
    # SFT MUST BE FINISHED before the reference cache is built. Two reasons, both
    # real: (1) `best_step_so_far` exists after the FIRST eval, so a naive watcher
    # would anchor RL to a mid-training checkpoint rather than the final best;
    # (2) the cache needs a whole GPU and SFT owns all three. Refuse-and-retry
    # rather than race.
    if sft_running():
        return {"stage": "ref", "ok": False, "retry": True,
                "reason": "SFT is still training; the final best is not known yet "
                          "and the GPUs are in use"}
    free = gpu_free_bytes()
    need = MODEL_BYTES_27B_BF16
    if not free:
        return {"stage": "ref", "ok": False, "reason": "no GPU visible"}
    if max(free) < need:
        return {"stage": "ref", "ok": False, "retry": True,
                "reason": (f"no GPU has {need / 1024**3:.0f} GiB free "
                           f"(free: {[round(f / 1024**3, 1) for f in free]} GiB) - "
                           f"SFT still owns the devices")}
    os.makedirs(os.path.dirname(out), exist_ok=True)
    prompts = (cfg.get("data") or {}).get("train")
    cmd = [PY, os.path.join(HERE, "grpo_ref_cache.py"),
           "--prompts", prompts or "", "--ckpt", ckpt, "--out", out,
           "--action-dist", "--device", "cuda"]
    r = subprocess.run(cmd, cwd=HERE)
    return {"stage": "ref", "ok": r.returncode == 0, "cache": out,
            "cmd": cmd, "rc": r.returncode}


def stage_plan(cfg_path: str) -> dict:
    r = subprocess.run([PY, os.path.join(HERE, "rl_plan.py"),
                        "--json", os.path.join(STATE, "rl_plan.json")],
                       cwd=HERE, capture_output=True, text=True)
    tail = (r.stdout or "").strip().splitlines()
    return {"stage": "plan", "ok": r.returncode == 0,
            "rc": r.returncode, "stdout_tail": tail[-3:] if tail else [],
            "json": os.path.join(STATE, "rl_plan.json")}


STAGES = {"ckpt": stage_ckpt, "ref": stage_ref, "plan": stage_plan}


def run_stages(cfg_path: str, sft_out: str, stages) -> dict:
    os.makedirs(STATE, exist_ok=True)
    results = []
    for s in stages:
        if s == "ckpt":
            res = stage_ckpt(cfg_path, sft_out)
        else:
            res = STAGES[s](cfg_path)
        results.append(res)
        print(json.dumps(res, default=str), flush=True)
        if not res.get("ok"):
            break                    # do not run a stage whose input is missing
    receipt = {"schema": "sft_to_rl_handoff_v1", "unix": int(time.time()),
               "stages": results,
               "ok": all(r.get("ok") for r in results) and len(results) == len(stages)}
    with open(os.path.join(STATE, "handoff_receipt.json"), "w", encoding="utf-8") as fh:
        json.dump(receipt, fh, indent=2, default=str)
    return receipt


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--config", default=CFG)
    ap.add_argument("--sft-out", default=SFT_OUT)
    ap.add_argument("--stages", default="ckpt,ref,plan")
    ap.add_argument("--wait", action="store_true")
    ap.add_argument("--status", action="store_true")
    ap.add_argument("--poll-s", type=int, default=300)
    a = ap.parse_args(argv)

    if a.status:
        ck, ev = resolve_best(a.sft_out)
        print(json.dumps({"best_checkpoint": ck,
                          "evidence_or_reason": ev,
                          "init_from": (json.load(open(a.config, encoding="utf-8"))
                                        .get("init_from") if os.path.isfile(a.config)
                                        else None)}, indent=2, default=str))
        return 0 if ck else 1

    stages = [s.strip() for s in a.stages.split(",") if s.strip()]
    bad = [s for s in stages if s not in STAGES]
    if bad:
        ap.error(f"unknown stages {bad}; known: {sorted(STAGES)}")

    if not a.wait:
        r = run_stages(a.config, a.sft_out, stages)
        return 0 if r["ok"] else 1

    n = 0
    while True:
        n += 1
        ck, ev = resolve_best(a.sft_out)
        if ck:
            print(f"[handoff] best checkpoint found after {n} poll(s): {ck}",
                  flush=True)
            r = run_stages(a.config, a.sft_out, stages)
            if r["ok"]:
                return 0
            pend = [s for s in r["stages"] if not s.get("ok") and s.get("retry")]
            if not pend:
                return 1
            print(f"[handoff] waiting on {[s['stage'] for s in pend]}: "
                  f"{pend[0].get('reason')}", flush=True)
        else:
            print(f"[handoff] poll {n}: {ev}", flush=True)
        time.sleep(max(30, a.poll_s))


if __name__ == "__main__":
    sys.exit(main())
