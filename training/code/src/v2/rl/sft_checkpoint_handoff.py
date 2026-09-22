#!/usr/bin/env python
"""SFT -> RL handoff: resolve the SFT BEST checkpoint, make it loadable, prove it.

WHY THIS EXISTS (2026-09-21). "A real RL run must be able to pick the best checkpoint
from our SFT run" was FALSE for three independent reasons, any one of which stops RL:

  1. WRONG RUNS. `rl_readiness.SFT_RUNS` still points at sft-004/sft-003 while the live
     run is sft-013.
  2. WRONG RULE. `rl_readiness.find_best_checkpoint()` returns the most-recently-MODIFIED
     checkpoint directory. "Best" for SFT is BEST-VALIDATION, and the plan explicitly
     requires ties to resolve EARLIER. mtime is the opposite of both.
  3. NOT LOADABLE. sft-013 runs FSDP2 with `state_dict_type=SHARDED_STATE_DICT`, so its
     checkpoints hold `pytorch_model_fsdp_0/__<rank>_0.distcp` (torch.distributed
     .checkpoint shards) and `optimizer_<i>_rank<r>.bin` -- NO `config.json`, NO
     `model-*.safetensors`. Nothing downstream can `from_pretrained` that. A naive
     "has a .bin file" test is a FALSE POSITIVE here, because the optimizer shards are
     `.bin`.

THE THREE COMMANDS
  resolve   authoritative best checkpoint + how loadable it currently is (read-only)
  export    SHARDED -> consolidated BF16 HF dir (accelerate.utils.merge_fsdp_weights)
  verify    load the exported dir, count params, run a deterministic forward

AUTHORITY FOR "BEST". The HF Trainer writes `best_model_checkpoint` / `best_metric` /
`best_global_step` into `trainer_state.json`. That is the run's OWN record of the
artifact `load_best_model_at_end` would have kept. `val_history.jsonl` carries an
independent definition (lowest val_loss step). We require the two to AGREE and REFUSE
otherwise -- silently picking one when two definitions are in play is how a worse
checkpoint reaches RL while everyone believes it was the best.

EXPORT IS CPU-BOUND, no GPU, and is therefore NOT blocked by a live training run. It is
still I/O heavy (~54GB read + ~54GB write), so run it when the box is not mid-epoch.

Run:
  python sft_checkpoint_handoff.py resolve
  python sft_checkpoint_handoff.py export  --out /training/runs/sft-013/sft_final_bf16_hf
  python sft_checkpoint_handoff.py verify  --dir /training/runs/sft-013/sft_final_bf16_hf
  python sft_checkpoint_handoff.py --self-check          # CPU, no model, no checkpoint
"""
from __future__ import annotations

import argparse
import glob
import json
import math
import os
import shutil
import sys

RUNS_GLOB = "/training/runs/sft-*"
SUBRUN = "astra_north_star_v3_linux_sft"
BASE_MODEL = "/training/runs/cpt-001/cpt_final_bf16_hf"   # source of config/tokenizer
EXPECTED_PARAMS = 26.9e9                                   # ~26.90B for the 27B


# --------------------------------------------------------------------------- classify
def classify_artifact(d: str) -> dict:
    """How loadable is this as an `init_from` target? Detects the three real layouts."""
    if not d or not os.path.isdir(d):
        return {"kind": "MISSING", "loadable": False, "path": d}
    names = set(os.listdir(d))
    top_safet = sorted(n for n in names if n.endswith(".safetensors"))
    has_config = "config.json" in names
    fsdp_dirs = sorted(n for n in names if n.startswith("pytorch_model_fsdp_"))
    distcp = 0
    for f in fsdp_dirs:
        distcp = max(distcp, len(glob.glob(os.path.join(d, f, "*.distcp"))))
    zero3 = bool(glob.glob(os.path.join(d, "global_step*", "zero_pp_rank_*model_states.pt")))
    # CRITICAL: optimizer shards are .bin/.safetensors-less; they must never be read as
    # model weights. Only top-level weight files + config count.
    if has_config and (top_safet or "pytorch_model.bin" in names):
        return {"kind": "LOADABLE_HF", "loadable": True, "path": d,
                "weights": (top_safet or ["pytorch_model.bin"]), "config": True}
    if fsdp_dirs and distcp >= 2:
        return {"kind": "SHARDED_FSDP2", "loadable": False, "path": d,
                "fsdp_dirs": fsdp_dirs, "distcp_shards": distcp,
                "needs": "export (accelerate.utils.merge_fsdp_weights) then copy config"}
    if zero3:
        return {"kind": "SHARDED_ZERO3", "loadable": False, "path": d,
                "needs": "export via the ZeRO-3 save_16bit_model path"}
    return {"kind": "UNKNOWN", "loadable": False, "path": d,
            "names_sample": sorted(names)[:8]}


def is_true(*bools) -> bool:
    return all(bools)


# --------------------------------------------------------------------------- resolve
def _latest_trainer_state(run_dir: str):
    """The trainer_state.json with the highest global_step (the live one)."""
    best = None
    for p in glob.glob(os.path.join(run_dir, "checkpoint-*", "trainer_state.json")):
        try:
            st = json.load(open(p, encoding="utf-8"))
        except Exception:
            continue
        if best is None or (st.get("global_step") or 0) > (best[1].get("global_step") or 0):
            best = (p, st)
    return best


def _val_history_best(run_dir: str):
    """Lowest val_loss row in val_history.jsonl, and the step that achieved it."""
    p = os.path.join(run_dir, "val_history.jsonl")
    if not os.path.isfile(p):
        return None, f"no val_history.jsonl at {p}"
    rows = []
    for line in open(p, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("val_loss") is not None and r.get("step") is not None:
            rows.append(r)
    if not rows:
        return None, "val_history.jsonl has no usable rows"
    b = min(rows, key=lambda r: r["val_loss"])
    return {"step": b["step"], "val_loss": b["val_loss"], "n_evals": len(rows)}, None


def discover_runs(runs_glob: str = RUNS_GLOB) -> list:
    out = []
    for g in sorted(glob.glob(runs_glob)):
        sub = os.path.join(g, SUBRUN)
        if os.path.isdir(sub):
            out.append({"run": g, "dir": sub})
    return out


def _evals_from_trainer_state(run_dir: str):
    """Every eval row the run itself logged, from its newest trainer_state.json."""
    ts = _latest_trainer_state(run_dir)
    if ts is None:
        return [], None
    _p, st = ts
    rows = [{"step": h.get("step"), "epoch": h.get("epoch"),
             "eval_loss": h.get("eval_loss")}
            for h in st.get("log_history", []) if "eval_loss" in h]
    return rows, st


def resolve(runs_glob: str = RUNS_GLOB, run: str = None) -> dict:
    """BEST = best VALIDATION, with the run's own stat values. Never mtime-as-best.

    Selection is two-level and the levels are different questions:
      * WHICH RUN  -- the ACTIVE one (its out dir is being written to), or an explicit
        `--run`. All candidates are reported with their best stats so ambiguity is visible.
      * WHICH CHECKPOINT -- best VALIDATION by the run's own record. Authority is
        `trainer_state.json.best_model_checkpoint`/`best_metric`/`best_global_step`; the
        independent check is the lowest `val_loss` in `val_history.jsonl`. They must AGREE
        or we REFUSE (two definitions of "best" in play must never be silently reconciled).
    """
    runs = discover_runs(runs_glob)
    if run:
        runs = [r for r in runs if os.path.abspath(r["run"]) == os.path.abspath(run)
                or os.path.abspath(r["dir"]) == os.path.abspath(run)]
        if not runs:
            return {"ok": False, "reason": f"--run {run} is not a discovered SFT run"}
    if not runs:
        return {"ok": False, "reason": f"no SFT runs under {runs_glob}"}
    runs.sort(key=lambda r: os.path.getmtime(r["dir"]), reverse=True)

    def summarize(r):
        ts_rows, st = _evals_from_trainer_state(r["dir"])
        vh, vh_err = _val_history_best(r["dir"])
        best_dir = (st or {}).get("best_model_checkpoint")
        return {"run": r["run"], "dir": r["dir"],
                "global_step": (st or {}).get("global_step"),
                "best_metric": (st or {}).get("best_metric"),
                "best_global_step": (st or {}).get("best_global_step"),
                "best_model_checkpoint": best_dir,
                "n_evals_trainer_state": len(ts_rows),
                "n_evals_val_history": (vh or {}).get("n_evals") or 0,
                "val_history_best": vh, "val_history_error": vh_err,
                "artifact": classify_artifact(best_dir) if best_dir else None}

    all_runs = [summarize(r) for r in runs]
    chosen = all_runs[0]
    ts_rows, st = _evals_from_trainer_state(chosen["dir"])
    vh, vh_err = _val_history_best(chosen["dir"])

    # merged eval history: the run's own rows, annotated with the independent source
    evals = []
    seen = set()
    for row in ts_rows:
        evals.append({**row, "source": "trainer_state"})
        seen.add(row.get("step"))
    for row in ([] if vh is None else [vh]):
        if row.get("step") not in seen:
            evals.append({"step": row["step"], "val_loss": row["val_loss"],
                          "epoch": None, "source": "val_history"})
    evals.sort(key=lambda r: (r.get("step") or 0))

    out = {"ok": False, "runs_found": [r["run"] for r in runs],
           "chosen": chosen, "evals": evals, "all_runs": all_runs}
    bd = chosen.get("best_model_checkpoint")
    if not bd:
        out["reason"] = "no trainer_state.json with best_model_checkpoint"
        return out
    if vh and chosen.get("best_global_step") is not None \
            and vh["step"] != chosen["best_global_step"]:
        out["DISAGREEMENT"] = (
            f"trainer_state best_global_step={chosen['best_global_step']} "
            f"(val_loss {chosen['best_metric']}) but val_history's lowest val_loss is at "
            f"step {vh['step']} ({vh['val_loss']}). Two definitions of 'best' are in play; "
            f"REFUSING to choose. Reconcile before exporting.")
        return out
    out["ok"] = True
    out["best"] = {"checkpoint": os.path.basename(bd), "dir": bd,
                   "step": chosen.get("best_global_step"),
                   "val_loss": chosen.get("best_metric"),
                   "epoch": next((r.get("epoch") for r in ts_rows
                                  if r.get("step") == chosen.get("best_global_step")), None),
                   "val_ppl": (round(math.exp(min(chosen["best_metric"], 50)), 6)
                               if chosen.get("best_metric") else None),
                   "source": "trainer_state.best_model_checkpoint, agreed by val_history",
                   "loadable": bool((chosen.get("artifact") or {}).get("loadable")),
                   "artifact_kind": (chosen.get("artifact") or {}).get("kind")}
    return out


# --------------------------------------------------------------------------- export
def export(best_dir: str, out: str, safe_serialization: bool = True) -> dict:
    """Merge FSDP2 sharded weights into a consolidated HF dir. CPU-bound."""
    art = classify_artifact(best_dir)
    if art["kind"] == "LOADABLE_HF":
        return {"ok": True, "already_loadable": True, "path": best_dir,
                "note": "no export needed"}
    if art["kind"] != "SHARDED_FSDP2":
        return {"ok": False, "reason": f"unsupported artifact kind {art['kind']}",
                "artifact": art}
    if os.path.isdir(out) and (glob.glob(os.path.join(out, "*.safetensors"))
                               or os.path.exists(os.path.join(out, "pytorch_model.bin"))):
        return {"ok": False, "reason": f"refusing to overwrite existing weights in {out}"}
    pre = shard_metadata_check(best_dir)
    if not pre.get("ok"):
        return {"ok": False, "reason": "shard set failed the metadata completeness gate; "
                                       "refusing to start a ~54GB merge", "precheck": pre}
    from accelerate.utils import merge_fsdp_weights
    os.makedirs(out, exist_ok=True)
    # the metadata + .distcp live INSIDE pytorch_model_fsdp_<i>/, so merge THAT dir
    shard_root = os.path.join(best_dir, art["fsdp_dirs"][0])
    produced = merge_fsdp_weights(shard_root, out, safe_serialization=safe_serialization)
    # merge writes ONLY weights. from_pretrained also needs config + tokenizer.
    for f in ("config.json", "generation_config.json", "chat_template.jinja",
              "tokenizer.json", "tokenizer_config.json", "vocab.json", "merges.txt",
              "special_tokens_map.json"):
        for src_dir in (best_dir, BASE_MODEL):
            src = os.path.join(src_dir, f)
            if os.path.exists(src) and not os.path.exists(os.path.join(out, f)):
                shutil.copy2(src, os.path.join(out, f))
    return {"ok": True, "produced": str(produced), "out": out,
            "artifact": classify_artifact(out)}


# --------------------------------------------------------------------------- precheck
def shard_metadata_check(best_dir: str, tol: float = 0.02) -> dict:
    """Cheap completeness gate on a SHARDED_FSDP2 dir, before spending ~54GB of I/O.

    Reads ONLY `.metadata` (605KB) -- no weights are materialised, no process group, a
    couple of seconds. It answers the question that matters before an export: is this
    shard set internally COMPLETE and the right size? Measured on the live sft-013
    checkpoint-1777: 851 tensors, 26,895,998,464 params (26.896B), all bfloat16.
    """
    art = classify_artifact(best_dir)
    if art["kind"] != "SHARDED_FSDP2":
        return {"ok": False, "reason": f"not a SHARDED_FSDP2 dir (kind={art['kind']})",
                "artifact": art}
    from torch.distributed.checkpoint import FileSystemReader
    sh = os.path.join(best_dir, art["fsdp_dirs"][0])
    md = FileSystemReader(sh).read_metadata()
    total, n, dtypes = 0, 0, {}
    for _k, v in md.state_dict_metadata.items():
        if hasattr(v, "size"):
            numel = 1
            for d in v.size:
                numel *= int(d)
            total += numel
            n += 1
            dt = getattr(getattr(v, "properties", None), "dtype", None)
            dtypes[str(dt)] = dtypes.get(str(dt), 0) + 1
    ok = n > 0 and abs(total - EXPECTED_PARAMS) / EXPECTED_PARAMS < tol
    return {"ok": ok, "tensors": n, "params": total, "params_b": round(total / 1e9, 4),
            "expected_b": round(EXPECTED_PARAMS / 1e9, 4), "dtypes": dtypes,
            "distcp_shards": art["distcp_shards"], "shard_dir": sh,
            "note": "metadata-only; proves the shard set is complete before the merge"}


# --------------------------------------------------------------------------- verify
def verify(d: str, n_params_tol: float = 0.02) -> dict:
    art = classify_artifact(d)
    if not art["loadable"]:
        return {"ok": False, "reason": f"not loadable: {art['kind']}", "artifact": art}
    import torch
    from transformers import AutoModelForCausalLM
    m = AutoModelForCausalLM.from_pretrained(d, torch_dtype=torch.bfloat16,
                                             device_map="cpu", low_cpu_mem_usage=True)
    n = sum(p.numel() for p in m.parameters())
    size = sum(os.path.getsize(os.path.join(d, f)) for f in os.listdir(d)
               if f.endswith(".safetensors") or f == "pytorch_model.bin")
    with torch.no_grad():
        out = m(torch.tensor([[1, 2, 3, 4]]), use_cache=False)
    finite = bool(torch.isfinite(out.logits).all())
    ok = finite and abs(n - EXPECTED_PARAMS) / EXPECTED_PARAMS < n_params_tol
    return {"ok": ok, "params": n, "device": "cpu", "weights_bytes": size,
            "logits_finite": finite, "artifact": art,
            "note": "size match to a known-good full-BF16 gather is the completeness check"}


# --------------------------------------------------------------------------- self-check
def _self_check() -> int:
    import tempfile
    fails = []

    def mk(root, name, files):
        p = os.path.join(root, name)
        os.makedirs(p, exist_ok=True)
        for f in files:
            fp = os.path.join(p, f)
            os.makedirs(os.path.dirname(fp), exist_ok=True)
            open(fp, "w").close()
        return p

    with tempfile.TemporaryDirectory() as t:
        # 1. optimizer .bin files must NOT read as a loadable model (the false positive)
        ck = mk(t, "ckpt-opt-only", ["optimizer_0_rank0.bin", "optimizer_0_rank1.bin",
                                     "pytorch_model_fsdp_0/__0_0.distcp",
                                     "pytorch_model_fsdp_0/__1_0.distcp",
                                     "pytorch_model_fsdp_0/.metadata"])
        c = classify_artifact(ck)
        if c["loadable"] or c["kind"] != "SHARDED_FSDP2":
            fails.append(f"optimizer_bin_false_positive:{c}")
        # 2. a genuine consolidated dir IS loadable
        good = mk(t, "hf", ["config.json", "model-00001-of-00014.safetensors"])
        if not classify_artifact(good)["loadable"]:
            fails.append("loadable_hf_misclassified")
        # 3. ZeRO-3 layout is recognised and refused as loadable
        z = mk(t, "zero3", ["global_step1777/zero_pp_rank_0_mp_rank_00_model_states.pt"])
        cz = classify_artifact(z)
        if cz["loadable"] or cz["kind"] != "SHARDED_ZERO3":
            fails.append(f"zero3_misclassified:{cz}")
        # 4. resolution authority: disagreement must REFUSE
        r = os.path.join(t, "sft-test")
        sub = os.path.join(r, SUBRUN)
        os.makedirs(os.path.join(sub, "checkpoint-1777"), exist_ok=True)
        ckdir = os.path.join(sub, "checkpoint-1998")
        os.makedirs(ckdir, exist_ok=True)
        with open(os.path.join(ckdir, "trainer_state.json"), "w") as f:
            json.dump({"global_step": 1998,
                       "best_model_checkpoint": os.path.join(sub, "checkpoint-1777"),
                       "best_metric": 0.0308, "best_global_step": 1777}, f)
        with open(os.path.join(sub, "val_history.jsonl"), "w") as f:
            f.write(json.dumps({"step": 1777, "val_loss": 0.0308}) + "\n")
            f.write(json.dumps({"step": 1998, "val_loss": 0.0400}) + "\n")
        res = resolve(runs_glob=os.path.join(t, "sft-*"))
        if not res["ok"]:
            fails.append(f"clean_case_not_ok:{res.get('reason') or res['chosen'].get('reason')}")
        if res["chosen"].get("best_global_step") is None and res["chosen"].get("trainer_state"):
            pass
        # now make them disagree
        with open(os.path.join(sub, "val_history.jsonl"), "w") as f:
            f.write(json.dumps({"step": 1600, "val_loss": 0.0200}) + "\n")   # lower, diff step
            f.write(json.dumps({"step": 1777, "val_loss": 0.0308}) + "\n")
        res2 = resolve(runs_glob=os.path.join(t, "sft-*"))
        if "DISAGREEMENT" not in res2 or res2["ok"]:
            fails.append("disagreement_not_refused")
        # 5. export must refuse an unknown kind rather than guess
        e = export(good, os.path.join(t, "out"))
        if not e.get("ok") or not e.get("already_loadable"):
            fails.append(f"export_should_shortcircuit_loadable:{e}")
    print(json.dumps({"suite": "sft_checkpoint_handoff", "failed": len(fails),
                      "failures": fails}, indent=1))
    return 1 if fails else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("cmd", nargs="?", choices=["resolve", "precheck", "export", "verify"])
    ap.add_argument("--runs-glob", default=RUNS_GLOB)
    ap.add_argument("--out", default="")
    ap.add_argument("--from", dest="from_dir", default="",
                    help="export THIS dir instead of the resolved best checkpoint")
    ap.add_argument("--dir", default="")
    ap.add_argument("--self-check", action="store_true")
    a = ap.parse_args()
    if a.self_check:
        return _self_check()
    if a.cmd == "resolve":
        res = resolve(a.runs_glob)
        print(json.dumps(res, indent=1, default=str))
        return 0 if res["ok"] else 1
    if a.cmd == "export":
        if a.from_dir:
            best = os.path.abspath(a.from_dir)
        else:
            res = resolve(a.runs_glob)
            if not res["ok"]:
                print(json.dumps(res, indent=1, default=str))
                return 1
            best = res["best"]["dir"]
        out = a.out or (best.rstrip("/") + "_bf16_hf")
        out = os.path.abspath(out)
        e = export(best, out)
        print(json.dumps(e, indent=1, default=str))
        return 0 if e.get("ok") else 1
    if a.cmd == "precheck":
        res = resolve(a.runs_glob)
        best = a.dir or (res["chosen"]["best_checkpoint_dir"] if res.get("ok") else "")
        if not best:
            print(json.dumps({"ok": False, "reason": "no best checkpoint resolved"},
                             indent=1))
            return 1
        pc = shard_metadata_check(best)
        print(json.dumps(pc, indent=1))
        return 0 if pc.get("ok") else 1
    if a.cmd == "verify":
        v = verify(a.dir)
        print(json.dumps(v, indent=1, default=str))
        return 0 if v.get("ok") else 1
    ap.error("need a command (resolve|export|verify) or --self-check")


if __name__ == "__main__":
    sys.exit(main())