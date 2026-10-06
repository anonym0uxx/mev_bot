#!/usr/bin/env python
"""launch_rl.py — validated launcher for the RL phase (mirrors launch_native.py).

Discipline copied from the SFT launcher, because the same failure modes apply:
  * every input is HASHED into a run_identity.json, so a run can never be
    described as something it was not;
  * every prerequisite is checked BEFORE any process starts, and a missing one is
    a refusal with an exit code, never a warning;
  * `--plan` prints the exact command without running it (the default);
    `--execute` is required to spend GPU time;
  * resume is explicit: an existing run directory is RESUMED (optimizer + step
    from the newest checkpoint), never silently restarted from scratch.

MODES
  --plan     verify + print the command + write launch_receipt.json (no GPU)
  --execute  actually launch under accelerate with --num_processes 3

The loop itself lives in grpo_loop.py. This file never trains anything; it
decides whether training is allowed and records what was decided.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable
WALL = "/training/v2/reports/FORWARD_WALL_V1.json"


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(8 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_dir(path: str) -> str:
    h = hashlib.sha256()
    for root, dirs, files in os.walk(path):
        dirs.sort()
        for f in sorted(files):
            p = os.path.join(root, f)
            h.update(os.path.relpath(p, path).encode())
            h.update(str(os.path.getsize(p)).encode())
    return h.hexdigest()


def count_lines(path: str) -> int:
    n = 0
    with open(path, encoding="utf-8") as f:
        for _ in f:
            n += 1
    return n


def wall_clean(train_path: str, wall_ms: int) -> dict:
    """Assert no wall episode leaked into the training file. This is the one
    contamination that would silently invalidate the whole RL result."""
    import hashlib as _h
    if not os.path.isfile(WALL):
        return {"checked": False, "reason": "wall json missing"}
    w = json.load(open(WALL, encoding="utf-8"))
    ids_path = w.get("files", {}).get("ids")
    want = set()
    if ids_path and os.path.isfile(ids_path):
        for line in open(ids_path, encoding="utf-8"):
            try:
                ms, mint, _fam, _split = json.loads(line)
                want.add((int(ms), mint))
            except Exception:                                   # noqa: BLE001
                continue
    leaks, n = [], 0
    for line in open(train_path, encoding="utf-8"):
        n += 1
        try:
            r = json.loads(line)
        except Exception:                                       # noqa: BLE001
            continue
        if int(r.get("t_dec_ms", -1)) >= int(wall_ms):
            leaks.append(r.get("episode_id"))
        elif (int(r.get("t_dec_ms", -1)), r.get("mint")) in want:
            leaks.append(r.get("episode_id"))
        if len(leaks) >= 5:
            break
    return {"checked": True, "records": n, "leaks": leaks,
            "clean": not leaks}


def verify(cfg_path: str, mode: str) -> dict:
    if not os.path.isfile(cfg_path):
        raise SystemExit(f"REFUSING: config not found: {cfg_path}")
    cfg = json.load(open(cfg_path, encoding="utf-8"))
    problems, notes = [], []

    init_from = cfg.get("init_from") or ""
    if "BEST" in init_from and not os.path.isdir(init_from):
        problems.append(f"init_from is still the placeholder and does not exist: "
                        f"{init_from} - fill it with the SFT best checkpoint")
    elif not os.path.isdir(init_from):
        problems.append(f"init_from is not a directory: {init_from}")
    else:
        # A .bin/.safetensors ANY-file test is a FALSE POSITIVE on FSDP2 checkpoints:
        # those dirs carry `optimizer_<i>_rank<r>.bin`, which passed the old check while
        # the dir held no loadable HF weights at all. Require a real HF model dir
        # (config.json + top-level weights) and route sharded artifacts to the exporter.
        try:
            if HERE not in sys.path:
                sys.path.insert(0, HERE)
            from sft_checkpoint_handoff import classify_artifact
            art = classify_artifact(init_from)
        except Exception as exc:                                  # noqa: BLE001
            art = {"kind": "UNKNOWN", "loadable": False, "error": repr(exc)}
        if not art.get("loadable"):
            kind = str(art.get("kind"))
            fix = (" Run: python sft_checkpoint_handoff.py export --out <dir>, then point "
                   "init_from at the exported dir.") if kind.startswith("SHARDED") else \
                  " Expected config.json + model-*.safetensors at the top level."
            problems.append(
                f"init_from is NOT a loadable HF model dir (kind={kind}): {init_from}.{fix}")
        notes.append(f"init_from_kind={art.get('kind')}")
    notes.append(f"init_from={init_from}")

    d = cfg.get("data") or {}
    tr, va = d.get("train"), d.get("validation")
    for label, p in (("train", tr), ("validation", va)):
        if not p or not os.path.isfile(p):
            problems.append(f"{label} dataset missing: {p}")
        elif os.path.getsize(p) == 0:
            problems.append(f"{label} dataset is EMPTY: {p}")
    wall_ms = d.get("wall_ms")
    if not wall_ms:
        problems.append("data.wall_ms missing: the wall cannot be enforced")
    wc = {}
    if tr and os.path.isfile(tr) and wall_ms:
        wc = wall_clean(tr, wall_ms)
        if wc.get("checked") and not wc.get("clean"):
            problems.append(f"WALL LEAK: {wc['leaks']} - refusing to train on "
                            f"the forward test")

    ref = cfg.get("reference") or {}
    ref_cache = ref.get("action_dist_cache")
    if not ref_cache:
        # default location mirrors rl_plan.py
        ref_cache = "/training/runs/rl-001/ref_action_dist.jsonl"
    if not os.path.isfile(ref_cache):
        problems.append(f"reference action-distribution cache missing: {ref_cache} "
                        f"- build with grpo_ref_cache.py --action-dist")
    else:
        mf = ref_cache + ".manifest.json"
        if os.path.isfile(mf):
            m = json.load(open(mf, encoding="utf-8"))
            if m.get("checkpoint_sha256") and os.path.isdir(init_from):
                notes.append(f"ref cache rows={m.get('rows_written')} "
                             f"schema={m.get('schema')}")

    run_dir = cfg.get("run_dir") or f"/training/runs/{cfg.get('run_id', 'rl-001')}"
    resume = bool((cfg.get("checkpoint") or {}).get("resume"))
    existing = [d2 for d2 in (os.path.join(run_dir, x)
                              for x in (os.listdir(run_dir) if os.path.isdir(run_dir)
                                        else []))
                if os.path.isdir(d2) and os.path.basename(d2).startswith("checkpoint-")]
    mode_run = "resume" if (existing and resume) else "fresh"

    cmd = [
        PY, "-B", "-m", "accelerate.commands.launch",
        "--num_processes", "3", "--num_machines", "1", "--machine_rank", "0",
        os.path.join(HERE, "grpo_loop.py"),
        "--config", os.path.abspath(cfg_path),
        "--init-from", init_from,
        "--out-root", run_dir,
        "--ref-cache", ref_cache,
        "--mode", mode_run,
    ]
    identity = {
        "schema": "rl_launch_identity_v1", "mode": mode, "run_id": cfg.get("run_id"),
        "created_unix": int(time.time()),
        "config_sha256": sha256_file(cfg_path),
        "init_from": init_from,
        "init_from_sha256": sha256_dir(init_from) if os.path.isdir(init_from) else None,
        "train_records": count_lines(tr) if tr and os.path.isfile(tr) else None,
        "validation_records": count_lines(va) if va and os.path.isfile(va) else None,
        "wall_ms": wall_ms, "wall_clean": wc,
        "ref_cache": ref_cache,
        "ref_cache_sha256": (sha256_file(ref_cache)
                             if os.path.isfile(ref_cache) else None),
        "run_dir": run_dir, "mode_run": mode_run,
        "existing_checkpoints": sorted(os.path.basename(x) for x in existing),
        "command": cmd,
        "notes": notes, "problems": problems,
    }
    return {"cfg": cfg, "identity": identity, "problems": problems,
            "command": cmd, "run_dir": run_dir, "mode_run": mode_run}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--config", default=os.path.join(HERE, "configs", "rl_v1.json"))
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--plan", action="store_true", default=True)
    mode.add_argument("--execute", action="store_true")
    a = ap.parse_args(argv)
    mode_name = "execute" if a.execute else "plan"

    v = verify(a.config, mode_name)
    ident = v["identity"]
    os.makedirs(v["run_dir"], exist_ok=True)
    receipt = os.path.join(v["run_dir"], "launch_receipt.json")
    with open(receipt, "w", encoding="utf-8") as fh:
        json.dump(ident, fh, indent=2, default=str)
    print(json.dumps({"mode_run": v["mode_run"],
                      "problems": v["problems"],
                      "command": " ".join(shlex.quote(c) for c in v["command"]),
                      "receipt": receipt}, indent=1))
    if v["problems"]:
        print(f"\nREFUSING to launch: {len(v['problems'])} problem(s) above.",
              file=sys.stderr)
        return 2
    if not a.execute:
        print("\n[plan only] re-run with --execute to launch.")
        return 0
    print(f"\n[executing] {v['mode_run']} run in {v['run_dir']}", flush=True)
    return subprocess.call(v["command"], cwd=HERE)


if __name__ == "__main__":
    sys.exit(main())
