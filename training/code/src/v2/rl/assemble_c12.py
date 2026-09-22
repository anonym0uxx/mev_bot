#!/usr/bin/env python
"""Assemble c12 from the c12-relabelled families + the c11 pass-through families.

c12 = c11 with
  * decision family        <- /training/v2/candidate_sft_c12_entry   (DECISION_FAMILY_C12
                              ruling: c11 decision_grid actions passed through UNCHANGED —
                              the outcome-conditioned relabel failed its exam gate
                              (LB -0.018, see ENTRY_RELABEL_C12_GATEFAIL_NEGATIVE_RESULT.json)
                              — with the approved ev_gated_binary sizing rewrite only:
                              SIZE {NONE,SMALL,FULL}, MID retired, amm BUY->FULL,
                              curve BUY->SMALL)
  * management_replay      <- /training/v2/reports/mgmt_c12_aligned  (--align-to-entry-barrier,
                              1800s barrier-aligned horizon; replaces the fixed-300s clock)
  * utility_reasoning      <- c11 pass-through (unchanged)
  * utility_regression     <- c11 pass-through (regresses realized utility off the SAME
                              barrier_triplet v1 labels; relabel does not change its targets)

Same split policy and identity contract as assemble_c10.py (frozen_mint_sha256_10_10_80,
family:episode_id:step candidate ids), same one-lambda default rule. Sources that carry
all four families (the entry staging dir copies non-decision families through) are
FILTERED to the family they own, so nothing is double-counted.
"""
import argparse
import collections
import hashlib
import json
import os
import sys

FAMILIES = [
    ("decision", "/training/v2/candidate_sft_c12_entry"),
    ("management_replay", "/training/v2/reports/mgmt_c12_aligned"),
    ("utility_reasoning", "/training/v2/candidate_sft_c11"),
    ("utility_regression", "/training/v2/candidate_sft_c11"),
]
SPLITS = ("train", "validation", "examination")
FILE_OF = {"train": "train", "val": "validation", "test": "examination",
           "validation": "validation", "examination": "examination"}
OUT = "/training/v2/candidate_sft_c12"
REVISION = "c12_final_v1"


def frozen_split(mint):
    """frozen_mint_sha256_10_10_80_legacy_union_v1 (verbatim from resplit_c3.py:33)."""
    x = int(hashlib.sha256(mint.encode()).hexdigest()[:8], 16) / 0xFFFFFFFF
    return "val" if x < .1 else "test" if x < .2 else "train"


def rows_of(family, src, split):
    p = os.path.join(src, f"{split}.jsonl")
    if not os.path.exists(p):
        return
    with open(p) as fh:
        for line in fh:
            r = json.loads(line)
            fam = (r.get("meta") or {}).get("family") or r.get("family")
            # pass-through sources hold all families; keep only the one this slot owns
            if fam is not None and fam != family:
                continue
            yield r


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--outdir", default=OUT)
    ap.add_argument("--default-lambda", type=float, default=None,
                    help="corpus-wide lambda_exposure for rows without one; "
                         "REQUIRED, take it from ENTRY_RELABEL_C12.json (AMM lambda)")
    args = ap.parse_args()
    if args.default_lambda is None:
        print("REFUSING: --default-lambda not given (read the re-derived AMM lambda "
              "from /training/v2/reports/ENTRY_RELABEL_C12.json; do not reuse c10's)")
        return 1
    os.makedirs(args.outdir, exist_ok=True)

    for family, src in FAMILIES:
        if not any(os.path.exists(os.path.join(src, f"{s}.jsonl")) for s in SPLITS):
            print(f"REFUSING: no split files for {family} at {src}")
            return 1

    all_mints = set()
    for split in SPLITS:
        for family, src in FAMILIES:
            for r in rows_of(family, src, split):
                m = r.get("meta") or {}
                all_mints.add(r.get("mint") or m.get("mint"))
    canonical = {mi: frozen_split(mi) for mi in all_mints if mi}
    dist = collections.Counter(canonical.values())
    print(f"  mints {len(canonical)} -> frozen policy {dict(dist)}", flush=True)

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
                    m["corpus_revision"] = REVISION
                    _eid = str(r.get("episode_id") or m.get("episode_id") or "")
                    _step = m.get("step")
                    _step = 0 if _step is None else int(_step)
                    m["candidate_id"] = "%s:%s:%d" % (family, _eid, _step)
                    if m.get("lambda_exposure") is None:
                        m["lambda_exposure"] = args.default_lambda
                    per_file[dest] += 1
                    per_family[family] += 1
                    eid[(family, r.get("episode_id"), _step)] += 1
                    outs[dest].write(json.dumps(r, ensure_ascii=False) + "\n")
                    if FILE_OF.get(split, split) != dest:
                        moved += 1
    finally:
        for fh in outs.values():
            fh.close()
    for s, p in tmps.items():
        os.replace(p, os.path.join(args.outdir, f"{s}.jsonl"))

    manifest = {"corpus": "candidate_sft_c12", "revision": REVISION,
                "total_rows": sum(per_file.values()),
                "by_file": dict(per_file), "by_family": dict(per_family),
                "mints_canonicalised": len(canonical), "rows_moved_across_files": moved,
                "duplicate_candidate_ids": sum(v - 1 for v in eid.values() if v > 1),
                "default_lambda": args.default_lambda,
                "sources": {f: s for f, s in FAMILIES}}
    json.dump(manifest, open("/training/v2/reports/C12_ASSEMBLY_MANIFEST.json", "w"),
              indent=1)
    print(json.dumps(manifest, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
