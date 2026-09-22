#!/usr/bin/env python
"""Merge ALL V2 families into the final SFT set + the DPO preference set.

SFT (train/validation/test):
    decision            candidate_v7max    (BUY / WATCH / SKIP)
    entry-economics     candidate_v7max    (utility_reasoning)
    management_replay   candidate_v7mgmt   (HOLD / ADD / REDUCE / EXIT)
    utility_regression  candidate_v7edge   (realized executable utility)
DPO (separate, not SFT):
    preference_pairs    candidate_v7edge   (chosen/rejected, market-judged)
"""
import json, os, hashlib, collections

SFT_SRC = [("candidate_v7max", {"train": "train.jsonl", "validation": "validation.jsonl", "test": "test.jsonl"}, None),
           ("candidate_v7mgmt", {"train": "train.jsonl", "validation": "validation.jsonl", "test": "test.jsonl"}, "management_replay"),
           ("candidate_v7edge", {"train": "value_train.jsonl", "validation": "value_validation.jsonl", "test": "value_test.jsonl"}, "utility_regression")]
OUT = "/training/v2/candidate_sft_final"
DPO_OUT = "/training/v2/candidate_dpo_final"
os.makedirs(OUT, exist_ok=True)
os.makedirs(DPO_OUT, exist_ok=True)

stats = {}
for split in ("train", "validation", "test"):
    fam = collections.Counter(); n = 0
    with open(f"{OUT}/{split}.jsonl", "w", buffering=1 << 20) as fo:
        for d, mapping, fam_override in SFT_SRC:
            p = f"/training/v2/{d}/{mapping[split]}"
            if not os.path.exists(p):
                continue
            with open(p) as fi:
                for line in fi:
                    r = json.loads(line)
                    m = r.setdefault("meta", {})
                    if fam_override:
                        m["family"] = fam_override
                    else:
                        m.setdefault("family", "decision")
                    fo.write(json.dumps(r, separators=(",", ":")) + "\n")
                    fam[m["family"]] += 1; n += 1
    h = hashlib.sha256(open(f"{OUT}/{split}.jsonl", "rb").read()).hexdigest()
    stats[split] = {"rows": n, "bytes": os.path.getsize(f"{OUT}/{split}.jsonl"),
                    "sha256": h, "families": dict(fam)}
    print(f"[MERGE] {split:11s} rows={n:7,} {dict(fam)}")

dpo = {}
for split, fn in (("train", "prefs_train.jsonl"), ("validation", "prefs_validation.jsonl"), ("test", "prefs_test.jsonl")):
    src = f"/training/v2/candidate_v7edge/{fn}"
    if not os.path.exists(src):
        continue
    dst = f"{DPO_OUT}/{split}.jsonl"
    os.replace(src, dst) if False else None
    data = open(src, "rb").read()
    open(dst, "wb").write(data)
    dpo[split] = {"rows": sum(1 for _ in open(dst)), "bytes": len(data),
                  "sha256": hashlib.sha256(data).hexdigest()}
    print(f"[MERGE] dpo/{split:8s} pairs={dpo[split]['rows']:7,}")

json.dump({"sft": stats, "dpo": dpo}, open("/training/v2/reports/MERGE_FINAL.json", "w"), indent=1)
