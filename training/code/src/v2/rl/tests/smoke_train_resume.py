#!/usr/bin/env python
"""SMOKE TEST for the --train path of grpo_loop.py (CPU only, no GPU touched).

WHAT IT DOES
  Builds a fully synthetic RL workspace in the REAL record schema
  (grpo_dataset.build_records), a matching action-distribution ref cache, a wall
  eval set, a tiny randomly-initialised Qwen2 model (the real 27B tokenizer, so the
  action token ids are the real ones) and a smoke config DERIVED FROM
  configs/rl_v1.json, then drives the REAL CLI:

    1. --train --mode fresh   --max-steps 1  -> 1 optimizer step + checkpoint-1
    2. --train --mode resume  --max-steps 4  -> loads checkpoint-1, skips the
      already-completed prompts, takes ONE more optimizer step, checkpoint-2

  Then it independently re-reads the artifacts and reports the measurements.

TEST SEAM (stated plainly): a 2-layer random-init model cannot emit a parseable
"DECISION: X", so phase 1/2 pass --test-completions with FIXED completions. That
seam is refused by the driver on any accelerator and is unreachable from
launch_rl.py's fixed command line. Everything else - FSDP2 sharding, the local-shard
bnb binding, the guards, backward, clip, optimizer step, DCP save/load, per-rank
optimizer files, scheduler/RNG, step counter, resume skipping - is the production
code path.

USAGE
  accelerate launch --cpu --num_processes 1 tests/smoke_train_resume.py [--phase1-only]
  (or plain: /home/alon/qwen27b-venv/bin/python tests/smoke_train_resume.py)
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
RL = os.path.dirname(HERE)
sys.path.insert(0, RL)

PY = os.environ.get("RL_PY", "/home/alon/qwen27b-venv/bin/python")
SEED_TOK = os.environ.get(
    "RL_SEED_TOKENIZER",
    "/training/seed/models--unsloth--Qwen3.8-27B/snapshots/"
    "c5f9b8a95d5dc1e0e0f0d0c0b0a0908070605040302010ffeeddccbbaa9988700")


def find_seed_tokenizer() -> str:
    root = "/training/seed/models--unsloth--Qwen3.8-27B/snapshots"
    if os.path.isdir(root):
        for d in sorted(os.listdir(root)):
            p = os.path.join(root, d)
            if os.path.isfile(os.path.join(p, "tokenizer_config.json")) or \
                    os.path.isfile(os.path.join(p, "chat_template.jinja")):
                return p
    return SEED_TOK


def sha256_text(t: str) -> str:
    return hashlib.sha256(t.encode("utf-8")).hexdigest()


def sha256_file(p: str) -> str:
    return hashlib.sha256(open(p, "rb").read()).hexdigest()


def prompt_of(messages: list) -> str:
    """Same definition as grpo_dataset.prompt_of (non-assistant turns, sorted)."""
    keep = [m for m in messages if m.get("role") != "assistant"]
    return json.dumps(keep, ensure_ascii=False, sort_keys=True)


def build_messages(i: int) -> list:
    return [
        {"role": "system", "content":
         "You are a memecoin trading decision engine. Answer with "
         "DECISION: BUY|WATCH|SKIP then SIZE: <fraction>."},
        {"role": "user", "content":
         f"# DECISION TIME (unix ms): 17000000000{i:02d}\n"
         f"mint: MINT{i}\nreserves: 42.0 SOL / 1.0e9 tokens\n"
         f"State rules: choose DECISION: BUY, WATCH or SKIP."},
    ]


def build_fixtures(root: str, n_records: int = 4) -> dict:
    os.makedirs(root, exist_ok=True)
    init_dir = os.path.join(root, "init")
    out_root = os.path.join(root, "out")
    os.makedirs(init_dir, exist_ok=True)

    # ---- tokenizer: the REAL 27B tokenizer, so ' BUY'/' WATCH'/' SKIP' are real
    from transformers import AutoTokenizer
    src_tok = find_seed_tokenizer()
    tok = AutoTokenizer.from_pretrained(src_tok)
    tok.save_pretrained(init_dir)

    # ---- tiny model with the real vocab (a 2-layer Qwen2), saved bf16
    from transformers import Qwen2Config, Qwen2ForCausalLM
    import torch
    cfg = Qwen2Config(vocab_size=len(tok), hidden_size=64, num_hidden_layers=2,
                      num_attention_heads=4, num_key_value_heads=2, head_dim=16,
                      intermediate_size=128, max_position_embeddings=4096,
                      tie_word_embeddings=True)
    torch.manual_seed(0)
    model = Qwen2ForCausalLM(cfg).to(torch.bfloat16)
    model.save_pretrained(init_dir)
    n_params = sum(p.numel() for p in model.parameters())
    del model

    # ---- train rows in the REAL schema
    train_path = os.path.join(root, "rl_train.jsonl")
    shas = []
    with open(train_path, "w", encoding="utf-8") as fh:
        for i in range(n_records):
            p = prompt_of(build_messages(i))
            s = sha256_text(p)
            shas.append(s)
            group = {"SKIP": 0.0, "WATCH": 0.0, "BUY": 0.001 + 0.0001 * i}
            adv = {"SKIP": -group["BUY"] / 3.0, "WATCH": -group["BUY"] / 3.0,
                   "BUY": 2.0 * group["BUY"] / 3.0}
            fh.write(json.dumps({
                "episode_id": f"v2:TESTMINT{i}:17000000000{i:02d}",
                "mint": f"MINT{i}", "t_dec_ms": 1700000000000 + i,
                "split": "train",
                "prompt": p, "prompt_sha256": s,
                "actions": ["SKIP", "WATCH", "BUY"], "group": group,
                "advantages": adv, "best_action": "BUY", "n_arms": 3,
                "group_usable": True,
                "buy_policies": {"BUY:MOONSHOT_TAIL": {"reward": group["BUY"],
                                                      "held_ms": 60000,
                                                      "exit_reason": "horizon",
                                                      "regime": "normal"}},
                "sft_action": "BUY", "regime": "normal",
                "provenance": {"regime": "normal", "legacy_priced": False,
                               "reserve_joins_count": 3,
                               "staleness_ms_max": 1200.0, "staleness_ms_p50": 900.0,
                               "lookahead_joins": 0, "stale_budget_ms": None,
                               "reserve_sources": ["pumpswap"],
                               "reserve_refusals": 0, "refusals_by_reason": {}},
            }, ensure_ascii=False) + "\n")

    # ---- ref cache: the exact RefActionDist schema + manifest
    ref_path = os.path.join(root, "ref_action_dist.jsonl")
    with open(ref_path, "w", encoding="utf-8") as fh:
        for i, s in enumerate(shas):
            fh.write(json.dumps({"prompt_sha256": s,
                                 "ref_action_probs": [0.5, 0.3, 0.2],
                                 # the 5-arm JOINT (ARM_ORDER) is MANDATORY in the
                                 # cache: without it grpo_loop refuses before the
                                 # first forward pass, because the size token would
                                 # be unregularised. Chain rule from the marginal:
                                 # P(BUY_TIER) = P(BUY) * P(TIER|BUY); BUY=0.5 split
                                 # 0.4/0.3/0.3 -> 0.20/0.15/0.15.
                                 "ref_arm_probs": [0.2, 0.3, 0.2, 0.15, 0.15]}) + "\n")
    json.dump({"schema": "grpo_ref_action_v1", "actions": ["BUY", "WATCH", "SKIP"]},
              open(ref_path + ".manifest.json", "w"), indent=1)

    # ---- wall eval set: prompts + group + reserves/staleness evidence
    eval_path = os.path.join(root, "wall_episodes_scored.jsonl")
    with open(eval_path, "w", encoding="utf-8") as fh:
        for i in range(3):
            p = prompt_of(build_messages(i))
            s = sha256_text(p)
            fh.write(json.dumps({
                "episode_id": f"v2:WALL{i}:17000000000{i:02d}", "mint": f"WALL{i}",
                "t_dec_ms": 1700000000000 + i, "prompt": p, "prompt_sha256": s,
                "actions": ["SKIP", "WATCH", "BUY"],
                "group": {"SKIP": 0.0, "WATCH": 0.0, "BUY": 0.002},
                "advantages": {"SKIP": -0.0007, "WATCH": -0.0007, "BUY": 0.0014},
                "group_usable": True, "best_action": "BUY",
                "provenance": {"regime": "normal", "legacy_priced": False,
                               "reserve_joins_count": 4,
                               "staleness_ms_max": 800.0, "staleness_ms_p50": 500.0,
                               "lookahead_joins": 0, "stale_budget_ms": None,
                               "reserve_sources": ["pumpswap"],
                               "reserve_refusals": 0, "refusals_by_reason": {}},
            }, ensure_ascii=False) + "\n")

    # ---- TEST-SEAM completions: 3 parseable + 1 unparseable per prompt
    tc_path = os.path.join(root, "test_completions.jsonl")
    with open(tc_path, "w", encoding="utf-8") as fh:
        for s in shas:
            fh.write(json.dumps({"prompt_sha256": s, "completions": [
                "DECISION: BUY\nSIZE: 0.40\nINVALIDATION: none",
                "DECISION: SKIP\nSIZE: 0.00\nINVALIDATION: none",
                "DECISION: WATCH\nSIZE: 0.00\nINVALIDATION: none",
                "I cannot tell without more data",
            ]}) + "\n")

    # ---- smoke config: DERIVED from configs/rl_v1.json, only the documented
    #      small-scale overrides differ
    base = json.load(open(os.path.join(RL, "configs", "rl_v1.json"), encoding="utf-8"))
    smoke = json.loads(json.dumps(base))
    overrides = {
        ("data", "train"): train_path,
        ("data", "validation"): train_path,
        ("data", "eval"): eval_path,
        ("loop", "batch_prompts"): 2,
        ("loop", "max_new_tokens"): 24,
        ("loop", "grad_accum"): 1,
        ("optim", "max_steps"): 1,
        ("checkpoint", "save_steps"): 1,
        ("checkpoint", "keep_last"): 3,
        ("init_from", None): init_dir,
    }
    for (blk, key), val in overrides.items():
        if key is None:
            smoke[blk] = val
        else:
            smoke[blk][key] = val
    cfg_path = os.path.join(root, "rl_smoke.json")
    json.dump(smoke, open(cfg_path, "w"), indent=1)

    # prove nothing else drifted from the pinned config
    drift = []
    for blk in ("loop", "optim", "gate", "checkpoint", "cost_calibration"):
        for k, v in base[blk].items():
            if (blk, k) in overrides:
                continue
            if smoke[blk].get(k) != v:
                drift.append(f"{blk}.{k}")
    for k, v in base["data"].items():
        if ("data", k) in overrides:
            continue
        if smoke["data"].get(k) != v:
            drift.append(f"data.{k}")
    return {"root": root, "init_dir": init_dir, "out_root": out_root,
            "train_path": train_path, "ref_path": ref_path, "eval_path": eval_path,
            "tc_path": tc_path, "cfg_path": cfg_path, "shas": shas,
            "n_params": n_params, "config_drift": drift,
            "base_max_steps": base["optim"]["max_steps"],
            "base_lr": base["optim"]["lr"]}


def run_cli(args: list, log: str) -> dict:
    cmd = [PY, os.path.join(RL, "grpo_loop.py")] + args
    print("\n$ " + " ".join(cmd), flush=True)
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=3600)
    out = p.stdout + ("\n--- stderr ---\n" + p.stderr if p.stderr.strip() else "")
    open(log, "w", encoding="utf-8").write(out)
    print(out, flush=True)
    return {"cmd": " ".join(cmd), "returncode": p.returncode, "output": out}


def _cfg_variant(fx, root, name, **data_over):
    """A copy of the smoke config with only `data.*` overridden."""
    cfg = json.load(open(fx["cfg_path"], encoding="utf-8"))
    cfg["data"].update(data_over)
    p = os.path.join(root, name)
    json.dump(cfg, open(p, "w"), indent=1)
    return p


def refusal_suite(root, fx) -> dict:
    """Every refusal must fire BEFORE any model load / forward pass, exit non-zero,
    and print REFUSING. `--device cpu` keeps it GPU-free; these all fail in the
    pre-flight, so they cost nothing."""
    base = ["--out-root", os.path.join(root, "refuse_out"),
            "--device", "cpu", "--train", "--mode", "fresh"]
    rec = json.loads(open(fx["train_path"], encoding="utf-8").readline())
    leaky = os.path.join(root, "leaky_train.jsonl")
    with open(leaky, "w", encoding="utf-8") as fh:
        r2 = dict(rec)
        r2["t_dec_ms"] = int(json.load(open(fx["cfg_path"]))["data"]["wall_ms"]) + 1
        fh.write(json.dumps(r2) + "\n")
    emptyf = os.path.join(root, "empty_train.jsonl")
    open(emptyf, "w").close()
    masked = os.path.join(root, "masked_train.jsonl")
    with open(masked, "w", encoding="utf-8") as fh:
        m = dict(rec)
        m["group_usable"] = False
        fh.write(json.dumps(m) + "\n")
    badref = os.path.join(root, "ref_bad_order.jsonl")
    with open(badref, "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"prompt_sha256": fx["shas"][0],
                             "ref_action_probs": [0.6, 0.3, 0.1]}) + "\n")
    json.dump({"schema": "grpo_ref_action_v1", "actions": ["SKIP", "WATCH", "BUY"]},
              open(badref + ".manifest.json", "w"))

    cases = {
        "missing_init_from": ["--config", fx["cfg_path"], "--init-from",
                              os.path.join(root, "nope"),
                              "--ref-cache", fx["ref_path"]],
        "missing_ref_cache": ["--config", fx["cfg_path"], "--init-from", fx["init_dir"],
                              "--ref-cache", os.path.join(root, "nope.jsonl")],
        "ref_action_order_mismatch": ["--config", fx["cfg_path"],
                                      "--init-from", fx["init_dir"],
                                      "--ref-cache", badref],
        "empty_train_file": ["--config", _cfg_variant(fx, root, "cfg_empty.json",
                                                      train=emptyf),
                             "--init-from", fx["init_dir"], "--ref-cache", fx["ref_path"]],
        "all_rows_masked": ["--config", _cfg_variant(fx, root, "cfg_masked.json",
                                                     train=masked),
                            "--init-from", fx["init_dir"], "--ref-cache", fx["ref_path"]],
        "wall_leak_in_train_file": ["--config", _cfg_variant(fx, root, "cfg_leak.json",
                                                             train=leaky),
                                    "--init-from", fx["init_dir"],
                                    "--ref-cache", fx["ref_path"]],
        "wall_eval_missing": ["--config", _cfg_variant(
            fx, root, "cfg_nowall.json",
            eval=os.path.join(root, "no_wall_episodes.jsonl")),
            "--init-from", fx["init_dir"], "--ref-cache", fx["ref_path"]],
    }
    out = {}
    for name, extra in cases.items():
        r = run_cli(base + extra, os.path.join(root, f"log_refuse_{name}.txt"))
        out[name] = {"returncode": r["returncode"],
                     "refused": "REFUSING" in r["output"],
                     "no_model_load": "Loading weights" not in r["output"],
                     "message": next((ln.strip() for ln in r["output"].splitlines()
                                      if "REFUSING" in ln), "")[:200]}
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default="")
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    root = a.root or tempfile.mkdtemp(prefix="rl_smoke_")
    fx = build_fixtures(root)
    res = {"workspace": root, "n_params": fx["n_params"],
           "config_drift": fx["config_drift"]}
    print(json.dumps({"workspace": root, "tiny_model_params": fx["n_params"],
                      "config_drift_from_rl_v1": fx["config_drift"]}, indent=1))

    base = [ "--config", fx["cfg_path"], "--init-from", fx["init_dir"],
             "--out-root", fx["out_root"], "--ref-cache", fx["ref_path"],
             "--device", "cpu", "--test-completions", fx["tc_path"] ]

    # ---- negative control: resume with no checkpoint must REFUSE (non-zero)
    res["resume_without_checkpoint"] = run_cli(
        base + ["--train", "--mode", "resume"],
        os.path.join(root, "log_resume_no_ckpt.txt"))

    # ---- phase 1: fresh, one optimizer step, one checkpoint
    res["phase1"] = run_cli(base + ["--train", "--mode", "fresh", "--max-steps", "1"],
                            os.path.join(root, "log_phase1.txt"))
    r1 = os.path.join(fx["out_root"], "train_report.json")
    res["phase1_report"] = json.load(open(r1, encoding="utf-8")) if os.path.isfile(r1) else None

    # ---- phase 2: resume, must load step 1 and take ONE more step
    res["phase2"] = run_cli(base + ["--train", "--mode", "resume", "--max-steps", "4"],
                            os.path.join(root, "log_phase2.txt"))
    r2 = os.path.join(fx["out_root"], "train_report.json")
    res["phase2_report"] = json.load(open(r2, encoding="utf-8")) if os.path.isfile(r2) else None

    # ---- independent artifact checks
    import torch
    cks = sorted(int(d.split("-")[1]) for d in os.listdir(fx["out_root"])
                 if d.startswith("checkpoint-"))
    res["checkpoints"] = cks
    c1 = os.path.join(fx["out_root"], "checkpoint-1")
    c1_files = sorted(os.listdir(c1)) if os.path.isdir(c1) else []
    res["checkpoint1_files"] = c1_files
    res["checkpoint1_manifest"] = (json.load(open(os.path.join(c1, "checkpoint_manifest.json")))
                                   if os.path.isfile(os.path.join(c1, "checkpoint_manifest.json"))
                                   else None)
    def _count_tensors(o):
        if torch.is_tensor(o):
            return 1
        if isinstance(o, dict):
            return sum(_count_tensors(v) for v in o.values())
        if isinstance(o, (list, tuple)):
            return sum(_count_tensors(v) for v in o)
        return 0

    obin = os.path.join(c1, "optimizer_0_rank0.bin")
    st = torch.load(obin, map_location="cpu", weights_only=False) if os.path.isfile(obin) else None
    if isinstance(st, dict):
        states = st.get("state") or {}
        # bitsandbytes groups its quantisation tensors under
        # __bnb_optimizer_quant_state__, so the count must recurse.
        n_t = sum(_count_tensors(v) for v in states.values())
        res["optimizer_bin"] = {"keys": sorted(st.keys()), "n_param_states": len(states),
                                "n_state_tensors": n_t,
                                "sha256": hashlib.sha256(open(obin, "rb").read()).hexdigest()[:16]}
    else:
        res["optimizer_bin"] = None
    # model shards are a DCP directory
    res["dcp_model_files"] = [f for f in c1_files if f.startswith("pytorch_model_fsdp")]
    res["gate_history"] = [json.loads(l) for l in
                           open(os.path.join(fx["out_root"], "gate_history.jsonl"),
                                encoding="utf-8")] if os.path.isfile(
        os.path.join(fx["out_root"], "gate_history.jsonl")) else []

    # ---- requirement 7: every loud refusal, proven before any forward pass
    res["refusals"] = refusal_suite(root, fx)
    for name, r in res["refusals"].items():
        print(f"[refusal] {name}: exit={r['returncode']} {r['message'][:110]}")

    # ---- gate honesty: the wall gate must refuse a thin partition and accept a
    #      thick one (2000 episodes) with the SAME preregistered values
    sys.path.insert(0, RL)
    import grpo_loop as G
    cfg = json.load(open(fx["cfg_path"], encoding="utf-8"))
    res["gate_thin"] = G.run_gate([0.001] * 3, cfg)
    big = [0.0015] * 2000
    res["gate_thick_pass"] = G.run_gate(big, cfg)
    res["gate_thick_fail"] = G.run_gate([-0.001] * 2000, cfg)
    res["wall_leak_selftest"] = G.test_wall_leaks()

    # ---- verdict
    p1, p2 = res["phase1_report"] or {}, res["phase2_report"] or {}
    rt = ((p2.get("resume_report") or {}).get("optimizer_state_round_trip") or {})
    checks = {
        "resume_without_checkpoint_refused":
            res["resume_without_checkpoint"]["returncode"] != 0
            and "NEVER silently restart" in res["resume_without_checkpoint"]["output"],
        "config_drift_empty": not res["config_drift"],
        "phase1_exit0": res["phase1"]["returncode"] == 0,
        "phase1_one_optimizer_step": p1.get("optimizer_steps_this_process") == 1,
        "phase1_steps_done_1": p1.get("steps_done") == 1,
        "phase1_fsdp2_sharded": bool((p1.get("last_batch_stats") or {}).get("loss") is not None)
                                and p1.get("decoder_layer_cls") == "Qwen2DecoderLayer",
        "phase1_local_shard_bound": bool((p1.get("local_shard_bind") or {}).get("bound")),
        "phase2_exit0": res["phase2"]["returncode"] == 0,
        "phase2_resumed_from_step1": rt.get("resumed_from_step") == 1
            or (p2.get("resume_report") or {}).get("resumed_from_step") == 1,
        "phase2_loaded_optimizer": "optimizer_per_rank" in (
            (p2.get("resume_report") or {}).get("loaded") or []),
        "phase2_optimizer_bit_exact": rt.get("verdict") == "BIT-EXACT"
            and rt.get("sha256_mismatches") == 0 and (rt.get("tensors_compared") or 0) > 0,
        "phase2_optimizer_max_abs_diff_0": rt.get("max_abs_diff") == 0.0,
        "phase2_skipped_completed_prompts": (p2.get("skipped_batches") or 0) >= 1,
        "phase2_took_a_new_step": p2.get("steps_done") == 2
            and p2.get("optimizer_steps_this_process") == 1,
        "checkpoint1_has_dcp_model_shards": bool(res["dcp_model_files"]),
        "checkpoint1_has_per_rank_optimizer": bool(res["optimizer_bin"])
            and res["optimizer_bin"]["n_state_tensors"] > 0,
        "checkpoint1_has_scheduler_rng_state":
            "scheduler.pt" in c1_files and "rng_rank0.pt" in c1_files,
        "gate_refuses_thin_partition": not res["gate_thin"]["passes"]
            and res["gate_thin"]["n"] == 3,
        "gate_accepts_2000_thick": res["gate_thick_pass"]["passes"],
        "gate_rejects_negative_2000": not res["gate_thick_fail"]["passes"],
        "wall_leak_selftest_pass": res["wall_leak_selftest"] == 0,
        "gate_never_used_training_reward":
            bool(res["gate_history"])
            and all(g["gate"].get("selected_on") == "wall_gate_only"
                    and g["gate"].get("training_reward_used_for_selection") is False
                    for g in res["gate_history"])
            and "training_reward" in res["gate_history"][-1],
    }
    for name, r in res["refusals"].items():
        checks[f"refuse_{name}"] = bool(r["returncode"] != 0 and r["refused"]
                                        and r["no_model_load"])
    res["verdict"] = {"checks": checks, "failed": [k for k, v in checks.items() if not v],
                      "result": "PASS" if all(checks.values()) else "FAIL"}
    print("\n================ SMOKE RESULT ================")
    print(json.dumps(res, indent=1, default=str))
    out = os.path.join(root, "smoke_result.json")
    json.dump(res, open(out, "w"), indent=1, default=str)
    print(f"\nresult written: {out}")
    if not a.keep:
        pass
    return 0 if res["verdict"]["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
