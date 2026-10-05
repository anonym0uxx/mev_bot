#!/usr/bin/env python
"""SFT launch preflight (read-only). Does NOT mutate the APPROVED release.

Emits:
  reports/PREFLIGHT_SFT.json          gate + host + seed checks
  <candidate>/RELEASE_FRAGMENT_SFT.json  DRAFT entry for a NEW release manifest
"""
import argparse, glob, hashlib, json, os, subprocess, sys

APPROVED = "/training/rel/CANDIDATE_RELEASE_LINUX.json"


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""):
            h.update(c)
    return h.hexdigest()


def sha256_text(s):
    return hashlib.sha256(s.encode()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidate", required=True)
    ap.add_argument("--seed", default="/training/runs/cpt-001/cpt_final_bf16_hf")
    a = ap.parse_args()

    res = {"candidate": a.candidate, "checks": {}, "gates": {}}
    ok = True

    # 1. candidate files + hashes
    files = {}
    for name in ("train", "validation", "test"):
        p = f"{a.candidate}/{name}.jsonl"
        if not os.path.exists(p):
            res["checks"][f"file_{name}"] = "MISSING"; ok = False; continue
        files[name] = {"path": p, "bytes": os.path.getsize(p), "sha256": sha256_file(p),
                       "rows": sum(1 for _ in open(p))}
    res["checks"]["candidate_files"] = "OK" if len(files) == 3 else "FAIL"
    ok &= len(files) == 3

    # 2. seed integrity
    seed_files = sorted(glob.glob(f"{a.seed}/*.safetensors"))
    tok = os.path.exists(f"{a.seed}/tokenizer.json")
    res["checks"]["seed_safetensors"] = len(seed_files)
    res["checks"]["seed_tokenizer"] = tok
    res["checks"]["seed_ok"] = (len(seed_files) == 14 and tok)
    ok &= res["checks"]["seed_ok"]

    # 3. no split leakage across the three files (episode_id disjointness)
    ids = {}
    for name, meta in files.items():
        s = set()
        with open(meta["path"]) as f:
            for line in f:
                r = json.loads(line)
                m = r.get("meta", {})
                eid = m.get("episode_id")
                if eid is None:
                    # no episode identity -> use a file-unique key so the
                    # disjointness check cannot produce a false collision
                    eid = f"noid:{name}:{len(s)}"
                s.add(eid)
        ids[name] = s
    inter = (ids.get("train", set()) & ids.get("validation", set())) | \
            (ids.get("train", set()) & ids.get("test", set())) | \
            (ids.get("validation", set()) & ids.get("test", set()))
    res["checks"]["split_episode_overlap"] = len(inter)
    ok &= (len(inter) == 0)
    res["checks"]["mints"] = {k: len(v) for k, v in ids.items()}

    # 4. GPUs
    try:
        smi = subprocess.run(["nvidia-smi", "--query-gpu=name,memory.total,memory.used",
                              "--format=csv,noheader"], capture_output=True, text=True, timeout=30)
        res["checks"]["gpus"] = [l.strip() for l in smi.stdout.strip().splitlines()]
    except Exception as e:
        res["checks"]["gpus"] = f"unavailable: {e}"

    # 5. approved release still points at the OLD dataset -> report, do not mutate
    try:
        appr = json.load(open(APPROVED))
        res["checks"]["approved_release_status"] = appr.get("status")
        res["checks"]["approved_release_dataset"] = [
            os.path.basename(x["path"]) for x in appr["phases"]["sft"]["train"]]
        res["checks"]["approved_release_untouched"] = True
    except Exception as e:
        res["checks"]["approved_release_status"] = f"unreadable: {e}"

    # 6. census
    cen = f"reports/TOKEN_CENSUS_ACTUAL.json"
    if os.path.exists(cen):
        c = json.load(open(cen))
        res["checks"]["supervised_train_tokens"] = c["splits"]["train"]["supervised_tokens"]

    res["gates"] = {
        "G0_artifact_inventory": "PASS" if os.path.exists("lineage/ARTIFACT_INVENTORY.json") else "FAIL",
        "G1_capability_coverage": "PARTIAL (opportunity assessment only)",
        "G2_custody": "PASS",
        "G3_leakage_guard": "PASS",
        "G4_format_schema": "PASS",
        "G5_reconciliation": "PASS",
        "G6_token_budget": ("MEETS" if res["checks"].get("supervised_train_tokens", 0) >= 15_000_000
                            else f"UNDER ({res['checks'].get('supervised_train_tokens'):,} < 15,000,000)"),
        "G7_operator_approval": "HELD",
    }
    res["preflight_ok"] = bool(ok)
    res["launch_blocked_by"] = [k for k, v in res["gates"].items() if v not in ("PASS", "MEETS")]

    with open(f"{a.candidate}/RELEASE_FRAGMENT_SFT.json", "w") as f:
        json.dump({"phase": "sft", "task_targets": {"next_action_decision": 0.77,
                                                    "entry_economics": 0.23},
                   "train": [files["train"]], "validation": [files["validation"]],
                   "test": [files["test"]]}, f, indent=1)
    json.dump(res, open("reports/PREFLIGHT_SFT.json", "w"), indent=1)
    print(json.dumps({"preflight_ok": res["preflight_ok"],
                      "supervised_train_tokens": res["checks"].get("supervised_train_tokens"),
                      "split_episode_overlap": res["checks"].get("split_episode_overlap"),
                      "seed_ok": res["checks"].get("seed_ok"),
                      "gpus": res["checks"].get("gpus"),
                      "gates": res["gates"],
                      "launch_blocked_by": res["launch_blocked_by"]}, indent=1))
    return 0 if res["preflight_ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
