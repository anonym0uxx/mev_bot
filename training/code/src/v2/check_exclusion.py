import json, os
rel = json.load(open("/training/rel/CANDIDATE_RELEASE_LINUX.json"))
eu = rel["exclusion_union"]
print("exclusion_union binding:", eu)
base = "/training/rel"
p = eu["path"] if os.path.isabs(eu["path"]) else os.path.join(base, eu["path"])
print("exists:", os.path.exists(p))
ex = set()
if os.path.exists(p):
    for line in open(p):
        line = line.strip()
        if not line:
            continue
        try:
            d = json.loads(line)
            m = d.get("mint") or (d.get("meta") or {}).get("mint")
            if m:
                ex.add(m)
        except Exception:
            pass
print("exclusion mints:", len(ex))
ours = set()
for part in ("train", "validation", "examination"):
    for line in open(f"/training/v2/candidate_sft_c3/{part}.jsonl"):
        ours.add(json.loads(line)["meta"]["mint"])
print("our mints:", len(ours))
inter = ours & ex
print("OVERLAP:", len(inter), list(inter)[:5])
