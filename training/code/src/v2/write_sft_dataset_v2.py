import json, hashlib, os

REL_ORIG = "/training/rel/CANDIDATE_RELEASE_LINUX.json"
CON_ORIG = "/training/code/qwen27b/LAUNCH_CONTRACT_SFT.json"
OUT_DS = "/training/rel/SFT_DATASET_V2.json"
OUT_CON = "/training/code/qwen27b/LAUNCH_CONTRACT_SFT_V2.json"
C3 = "/training/v2/candidate_sft_c3"


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def entry(p):
    return {"path": p, "rows": sum(1 for _ in open(p)),
            "bytes": os.path.getsize(p), "sha256": sha(p)}


TASK_TARGETS = {"decision_action": 0.45, "next_action_decision": 0.25,
                "decision_reasoning": 0.30}
ds = {"schema": "north_star_sft_dataset_v1", "phase": "sft",
      "split_policy": "frozen_mint_sha256_10_10_80_legacy_union_v1",
      "task_targets": TASK_TARGETS,
      "train": [entry(f"{C3}/train.jsonl")],
      "validation": [entry(f"{C3}/validation.jsonl")],
      "examination": [entry(f"{C3}/examination.jsonl")]}
json.dump(ds, open(OUT_DS, "w"), indent=1)
ds_sha = sha(OUT_DS)

con = json.load(open(CON_ORIG))
# release manifest stays the FROZEN original -> CPT lineage intact
con["release_manifest"] = {"path": REL_ORIG, "sha256": sha(REL_ORIG)}
con["seed"]["release_sha256"] = sha(REL_ORIG)
con["run_id"] = "sft-002"
con["run_dir"] = "/training/runs/sft-002"
# Parallelism selector + its pinned binding (same path+sha256 convention as the
# deepspeed binding). FSDP full-shard with use_orig_params shards bf16 params
# with no fp32 master set; the ZeRO-3 binding is kept as the pinned fallback.
FSDP_PLAN = os.environ.get("SFT_FSDP_PLAN",
                           "/training/code/qwen27b/fsdp_full_shard_native.json")
con["parallel"] = "fsdp"
con["fsdp"] = {"path": FSDP_PLAN, "sha256": sha(FSDP_PLAN)}
con["sft_dataset"] = {"path": OUT_DS, "sha256": ds_sha}
# refresh pinned code hashes (release_data.py was extended for the override)
for cf in con["code_files"]:
    p = cf["path"]
    if os.path.exists(p):
        cf["sha256"] = sha(p)
con["trainer"]["sha256"] = sha(con["trainer"]["path"])
json.dump(con, open(OUT_CON, "w"), indent=1)

print(json.dumps({
    "sft_dataset": OUT_DS, "sft_dataset_sha256": ds_sha,
    "train_rows": ds["train"][0]["rows"], "val_rows": ds["validation"][0]["rows"],
    "examination_rows": ds["examination"][0]["rows"],
    "release_manifest": REL_ORIG, "release_sha256": sha(REL_ORIG),
    "launch_contract": OUT_CON, "contract_sha256": sha(OUT_CON),
    "task_targets": TASK_TARGETS,
}, indent=1))
