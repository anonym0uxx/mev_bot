"""Backfill meta.group + meta.source_associations for the management_replay family.

The entry (decision) family carries these; management_replay was assembled
episode-keyed and never carried the source-group identity that release_data.
record_groups() requires. The management rows ARE PumpSwap-AMM-only (venue field),
so the group is the mint (the underlying token, same as the entry family) and the
source is the same trades tape as the entry rows.

Purely additive: touches meta.group + meta.source_associations only, no label,
no prompt, no size change. Preserves the HOLD/ADD/REDUCE/EXIT labels byte-for-byte.
"""
import json, os, shutil

TRADES_SHA = "112511144d78ecb50d87914093dd1ecc1bec5735e98d814ca4d9cb58f2931120"
SRC = "/training/v2/candidate_sft_c10"
BAK = "/training/v2/candidate_sft_c10_pre_group_backfill"

if not os.path.isdir(BAK):
    shutil.copytree(SRC, BAK)
    print("backup ->", BAK)

total = 0
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
                r["meta"] = m
                n += 1
            out.write(json.dumps(r, ensure_ascii=False) + "\n")
    os.replace(tmp, p)
    print(sp, "backfilled", n)
    total += n

print("TOTAL", total)
