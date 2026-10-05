import json, hashlib, os, copy

REL = "/training/rel/CANDIDATE_RELEASE_LINUX.json"
CON = "/training/code/qwen27b/LAUNCH_CONTRACT_SFT.json"
OUT_REL = "/training/rel/CANDIDATE_RELEASE_LINUX_V2.json"
OUT_CON = "/training/code/qwen27b/LAUNCH_CONTRACT_SFT_V2.json"

def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()

def entry(p):
    return {"path": p, "rows": sum(1 for _ in open(p)), "bytes": os.path.getsize(p),
            "sha256": sha(p)}

C = "/training/v2/candidate_sft_final"
man = json.load(open(REL))

# replace the SFT phase datasets with the verified V2 corpus
man["phases"]["sft"]["train"] = [entry(f"{C}/train.jsonl")]
man["phases"]["sft"]["validation"] = [entry(f"{C}/validation.jsonl")]
# task_targets must equal EXACTLY the set of task buckets present
man["phases"]["sft"]["task_targets"] = {
    "decision_action": 0.45,
    "next_action_decision": 0.25,
    "decision_reasoning": 0.30,
}
man["release_id"] = man.get("release_id", "astra_north_star_v3_linux")
json.dump(man, open(OUT_REL, "w"), indent=1)
rel_sha = sha(OUT_REL)

con = json.load(open(CON))
con["release_manifest"]["path"] = OUT_REL
con["release_manifest"]["sha256"] = rel_sha
con["run_id"] = "sft-002"
con["run_dir"] = "/training/runs/sft-002"
con["seed"]["release_sha256"] = rel_sha
con["seed"]["release_id"] = man["release_id"]
json.dump(con, open(OUT_CON, "w"), indent=1)
con_sha = sha(OUT_CON)

print(json.dumps({
    "release_manifest": OUT_REL, "release_sha256": rel_sha,
    "launch_contract": OUT_CON, "contract_sha256": con_sha,
    "sft_train_rows": man["phases"]["sft"]["train"][0]["rows"],
    "sft_val_rows": man["phases"]["sft"]["validation"][0]["rows"],
    "task_targets": man["phases"]["sft"]["task_targets"],
}, indent=1))
