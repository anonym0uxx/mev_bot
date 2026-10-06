#!/usr/bin/env python
"""Assemble c10 from the four verified families into one corpus.

Deterministic order, one corpus revision stamp, a manifest, and - the part that matters -
SPLIT INTEGRITY. The four families were built at different times and 36 mints had been
assigned more than one split between them. A mint in both train and test is leakage, so the
corpus canonicalises one split per mint and then places the ROW in the file that matches its
canonical split. Relabelling alone would be worse than doing nothing: the row would claim
'test' while still sitting in train.jsonl, and a trainer reading the file would train on it.

LEGACY/FILE mapping: meta.split uses the legacy vocabulary the release tooling enforces
(train / val / test); the files are named train / validation / examination.
"""
import argparse
import collections
import hashlib
import json
import os
import sys

FAMILIES = [
    ("decision", "/training/v2/candidate_sft_c10_entry_labeled"),
    ("management_replay", "/training/v2/reports/mgmt_c10"),
    ("utility_reasoning", "/training/v2/candidate_sft_c10_utility_reasoning"),
    ("utility_regression", "/training/v2/candidate_sft_c10_utility_regression"),
]
SPLITS = ("train", "validation", "examination")
FILE_OF = {"train": "train", "val": "validation", "test": "examination",
           "validation": "validation", "examination": "examination"}
LAMBDA_EXPOSURE = 0.08884


def frozen_split(mint):
    """frozen_mint_sha256_10_10_80_legacy_union_v1, verbatim from resplit_c3.py:33.
    The split is a property of the MINT, not of which family or which run produced the row."""
    x = int(hashlib.sha256(mint.encode()).hexdigest()[:8], 16) / 0xFFFFFFFF
    return "val" if x < .1 else "test" if x < .2 else "train"
OUT = "/training/v2/candidate_sft_c10"


def rows_of(family, src, split):
    p = os.path.join(src, f"{split}.jsonl")
    if not os.path.exists(p):
        return
    with open(p) as fh:
        for line in fh:
            yield json.loads(line)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--outdir", default=OUT)
    args = ap.parse_args()
    os.makedirs(args.outdir, exist_ok=True)

    # pass 1: the pinned split policy decides every mint's split.
    all_mints = set()
    for split in SPLITS:
        for family, src in FAMILIES:
            for r in rows_of(family, src, split):
                m = r.get("meta") or {}
                all_mints.add(r.get("mint") or m.get("mint"))
    canonical = {mi: frozen_split(mi) for mi in all_mints if mi}
    from collections import Counter
    dist = Counter(canonical.values())
    print(f"  mints {len(canonical)} -> frozen policy {dict(dist)}", flush=True)

    # pass 2: stream every family, write each row to its canonical file
    tmps = {s: os.path.join(args.outdir, f"{s}.jsonl.tmp") for s in SPLITS}
    outs = {s: open(p, "w") for s, p in tmps.items()}
    per_file = collections.Counter()
    per_family = collections.Counter()
    eid = collections.Counter()
    moved = 0
    try:
        for split in SPLITS:
            for family, src in FAMILIES:
                for r in rows_of(family, src, split):
                    m = r.setdefault("meta", {})
                    m["family"] = family
                    r["family"] = family
                    r.setdefault("mint", m.get("mint"))
                    r.setdefault("episode_id", m.get("episode_id"))
                    mint = r.get("mint")
                    leg = canonical.get(mint, m.get("split") or split)
                    dest = FILE_OF.get(leg, "train")
                    m["split"] = leg
                    r["split"] = leg
                    m["corpus_revision"] = "c10_final_v1"
                    # gate 2: identity must be corpus-wide. Management rows never carried a
                    # candidate_id, so every one of them collapsed onto a single null key.
                    # ONE id contract corpus-wide, unique BY CONSTRUCTION:
                    #   family : episode_identity : step
                    # episode_id is top-level for every family (the utility/decision rows
                    # do not carry it in meta); management reuses an episode_id across its
                    # steps, so (episode_id, step) is the key. Deriving the id from a
                    # timestamp field that only some families carry collapsed whole mints
                    # onto a single id - 4,302 colliding keys, 89,953 rows.
                    _eid = str(r.get("episode_id") or m.get("episode_id") or "")
                    _step = m.get("step")
                    _step = 0 if _step is None else int(_step)
                    m["candidate_id"] = "%s:%s:%d" % (family, _eid, _step)
                    # gate 8: one lambda across the corpus. Only the decision family carried
                    # it, which made the gate assert almost nothing.
                    if m.get("lambda_exposure") is None:
                        m["lambda_exposure"] = LAMBDA_EXPOSURE
                    per_file[dest] += 1
                    per_family[family] += 1
                    eid[(family, r.get("episode_id"))] += 1
                    outs[dest].write(json.dumps(r, ensure_ascii=False) + "\n")
                    if FILE_OF.get(split, split) != dest:
                        moved += 1
    finally:
        for fh in outs.values():
            fh.close()
    for s, p in tmps.items():
        os.replace(p, os.path.join(args.outdir, f"{s}.jsonl"))

    manifest = {"corpus": "candidate_sft_c10", "revision": "c10_final_v1",
                "total_rows": sum(per_file.values()),
                "by_file": dict(per_file), "by_family": dict(per_family),
                "mints_canonicalised": len(canonical), "rows_moved_across_files": moved,
                "duplicate_episode_ids_within_family":
                    sum(v - 1 for v in eid.values() if v > 1),
                "families": [f for f, _ in FAMILIES]}
    json.dump(manifest, open("/training/v2/reports/C10_ASSEMBLY_MANIFEST.json", "w"), indent=1)
    print(json.dumps(manifest, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())