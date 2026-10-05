import json, collections
c = collections.Counter(); tot = collections.Counter(); sample = {}
for split in ("train", "validation", "test"):
    for line in open(f"/training/v2/candidate_sft_final/{split}.jsonl"):
        r = json.loads(line); m = r.get("meta", {})
        fam = m.get("family", "decision")
        tot[fam] += 1
        if not m.get("mint"):
            c[fam] += 1
            if fam not in sample:
                sample[fam] = list(m.keys())
print("missing meta.mint by family:", dict(c))
print("total by family:", dict(tot))
for k, v in sample.items():
    print(f"  {k}: meta keys = {v}")
print("other top-level keys on a sample:", list(json.loads(open('/training/v2/candidate_sft_final/train.jsonl').readline()).keys()))
