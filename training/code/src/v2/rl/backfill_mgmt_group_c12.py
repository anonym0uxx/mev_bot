"""Backfill meta.group + meta.source_associations (+ enrichment_status if absent)
for management_replay rows in candidate_sft_c12.

COPY OF backfill_mgmt_group.py (the c10 precedent) with three deliberate changes:
  1. SRC -> candidate_sft_c12
  2. also restores enrichment_status (dropped by the barrier-aligned rebuild) by
     copying it from the c11 corpus keyed on (episode_id, step) — same rows, same
     enrichment authority; the rebuild changed horizons/labels, not enrichment.
  3. backup dir name versioned for c12.

Purely additive: meta.group, meta.source_associations, meta.enrichment_status only.
No label, no prompt, no size change.
"""
import json, os, shutil

TRADES_SHA = "112511144d78ecb50d87914093dd1ecc1bec5735e98d814ca4d9cb58f2931120"
SRC = "/training/v2/candidate_sft_c12"
BAK = "/training/v2/candidate_sft_c12_pre_group_backfill"
C11 = "/training/v2/candidate_sft_c11"

if not os.path.isdir(BAK):
    shutil.copytree(SRC, BAK)
    print("backup ->", BAK)

# enrichment_status authority: c11 corpus, keyed (episode_id, step)
enrich = {}
for sp in ("train", "validation", "examination"):
    for line in open(f"{C11}/{sp}.jsonl"):
        r = json.loads(line)
        m = r.get("meta", {})
        if m.get("family") == "management_replay" and "enrichment_status" in m:
            enrich[(m.get("episode_id"), m.get("step"))] = m["enrichment_status"]
print("c11 enrichment keys:", len(enrich))

total = miss_enrich = 0
for sp in ("train", "validation", "examination"):
    p = f"{SRC}/{sp}.jsonl"
    tmp = p + ".tmp"
    n = 0
    with open(p) as fh, open(tmp, "w") as out:
        for line in fh:
            r = json.loads(line)
            m = r.get("meta", {})
            if m.get("family") == "management_replay":
                mint = r.get("mint") or m.get("mint")
                eid = m.get("episode_id")
                m["group"] = mint
                m["source_associations"] = [{
                    "source": {"file_sha256": TRADES_SHA, "table": "trades", "row": eid}
                }]
                if "enrichment_status" not in m:
                    es = enrich.get((eid, m.get("step")))
                    if es is not None:
                        m["enrichment_status"] = es
                    else:
                        miss_enrich += 1
                r["meta"] = m
                n += 1
            out.write(json.dumps(r, ensure_ascii=False) + "\n")
    os.replace(tmp, p)
    print(sp, "backfilled", n)
    total += n

print("TOTAL", total, "missing_enrichment_key", miss_enrich)
