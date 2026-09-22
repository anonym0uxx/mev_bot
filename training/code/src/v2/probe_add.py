import json, re, collections
import numpy as np
p = "/training/v2/candidate_v2_final/train.jsonl"
NUM = re.compile(r"([a-z0-9_]+)=(-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)")
mg = de = None
keys = collections.Counter()
rows = []
with open(p) as f:
    for line in f:
        r = json.loads(line)
        fam = r["meta"].get("family")
        u = r["messages"][1]["content"]
        if fam == "management_replay":
            if mg is None:
                mg = u
            d = {k: float(v) for k, v in NUM.findall(u)}
            for k in d:
                keys[k] += 1
            rows.append((d, r["meta"]["action"]))
        elif de is None:
            de = u
print("=== decision prompt sample ===")
print(de[:400])
print("\n=== management prompt sample ===")
print(mg[:500])
print("\n=== parsed feature keys (management) ===")
print(keys.most_common(25))
acts = collections.Counter(a for _, a in rows)
print("\nmgmt actions:", acts)
