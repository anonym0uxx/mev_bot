import json, collections
FAMS = collections.defaultdict(collections.Counter)
tot = collections.Counter()
for split in ("train", "validation", "test"):
    p = f"/training/v2/candidate_v2_final/{split}.jsonl"
    try:
        f = open(p)
    except FileNotFoundError:
        continue
    for line in f:
        r = json.loads(line)
        m = r.get("meta", {})
        fam = m.get("family", "?")
        a = m.get("action")
        FAMS[(split, fam)][a] += 1
        tot[(split, a)] += 1
    f.close()

print("=== per split x family ===")
for (split, fam), c in sorted(FAMS.items()):
    print(f"{split:11s} {fam:18s} n={sum(c.values()):7,}  {dict(c)}")
print("\n=== action totals per split (whole corpus) ===")
for split in ("train", "validation", "test"):
    c = {a: n for (s, a), n in tot.items() if s == split}
    print(f"{split:11s} {dict(sorted(c.items(), key=lambda kv: str(kv[0])))}")
