import json, collections, hashlib

for part in ("train", "validation"):
    seen = {}
    dup = 0
    conflicts = 0
    fams = collections.Counter()
    ex = []
    for line in open(f"/training/v2/candidate_sft_c3/{part}.jsonl"):
        r = json.loads(line)
        ph = hashlib.sha256(json.dumps(r["messages"][:-1], sort_keys=True).encode()).hexdigest()
        tgt = json.dumps(r["messages"][-1], sort_keys=True)
        m = r["meta"]
        if ph in seen:
            dup += 1
            fams[(m.get("family"), "dup")] += 1
            if seen[ph] != tgt:
                conflicts += 1
                fams[(m.get("family"), "conflict")] += 1
            if len(ex) < 2:
                ex.append((m.get("family"), m.get("step"), m.get("candidate_id"),
                           seen[ph][:60], tgt[:60]))
        else:
            seen[ph] = tgt
    print(f"--- {part}: rows={len(seen)+dup} unique_prompts={len(seen)} dup_rows={dup} "
          f"conflicting={conflicts}")
    print("   by family:", dict(fams))
    for e in ex:
        print("   ex:", e)
