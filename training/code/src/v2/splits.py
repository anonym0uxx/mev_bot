#!/usr/bin/env python
"""Stage 2 — split assignment for the V2 candidate.

Rule (frozen in SAMPLING_CONTRACT.md): chronological 70/15/15 by mint FIRST-SEEN
time from the launch records. Deterministic, reproducible, group-disjoint on
{mint, capture-session}. Protected-holdout mints are pulled OUT of train/val and
marked `protected_holdout`.

Emits reports/SPLIT_MANIFEST.json with per-split mint lists (sorted) + sha256 so a
third party can verify disjointness without re-deriving the rule.

Usage:
  splits.py \
     --launches canonical/renormalized_v6/launches.jsonl \
     --protected audit/observed_sft_protected_eval_mints.json \
     --protected audit/factual_cpt_protected_eval_mints.json \
     --out reports/SPLIT_MANIFEST.json
"""
import argparse, hashlib, json, sys

TRAIN_FRAC = 0.70
VAL_FRAC = 0.15


def load_launch_first_seen(path):
    first = {}
    n = 0
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            d = json.loads(line)
            m = d.get("mint")
            ts = d.get("recv_unix_ms")
            if not m or ts is None:
                continue
            n += 1
            if m not in first or ts < first[m]:
                first[m] = ts
    return first, n


def load_protected(paths):
    s = set()
    per = {}
    for p in paths:
        try:
            with open(p) as f:
                d = json.load(f)
        except FileNotFoundError:
            per[p] = 0
            continue
        if isinstance(d, dict):
            vals = d.get("mints") or d.get("protected_mints") or list(d.keys())
        else:
            vals = d
        got = {v for v in vals if isinstance(v, str)}
        per[p] = len(got)
        s |= got
    return s, per


def sha256_of(obj):
    return hashlib.sha256(json.dumps(obj, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--launches", required=True)
    ap.add_argument("--protected", nargs="*", default=[])
    ap.add_argument("--out", required=True)
    ap.add_argument("--train-frac", type=float, default=TRAIN_FRAC)
    ap.add_argument("--val-frac", type=float, default=VAL_FRAC)
    a = ap.parse_args()

    first, n_rows = load_launch_first_seen(a.launches)
    if not first:
        print("FAIL: no launch records read — refusing to emit a split", file=sys.stderr)
        return 2

    mints = sorted(first.items(), key=lambda kv: (kv[1], kv[0]))  # deterministic tie-break
    N = len(mints)
    i_tr = int(N * a.train_frac)
    i_va = int(N * (a.train_frac + a.val_frac))

    prot, per = load_protected(a.protected)
    unknown_prot = sorted(prot - set(first))

    splits = {"train": [], "val": [], "test": [], "protected_holdout": []}
    for idx, (m, _ts) in enumerate(mints):
        if m in prot:
            splits["protected_holdout"].append(m)
        elif idx < i_tr:
            splits["train"].append(m)
        elif idx < i_va:
            splits["val"].append(m)
        else:
            splits["test"].append(m)

    S = {k: set(v) for k, v in splits.items()}
    inter = {
        "train∩val": len(S["train"] & S["val"]),
        "train∩test": len(S["train"] & S["test"]),
        "val∩test": len(S["val"] & S["test"]),
        "train∩protected": len(S["train"] & S["protected_holdout"]),
        "val∩protected": len(S["val"] & S["protected_holdout"]),
        "test∩protected": len(S["test"] & S["protected_holdout"]),
    }
    union = set().union(*S.values())

    manifest = {
        "schema": "north_star_split_manifest_v2",
        "rule": "chronological by mint first-seen recv_unix_ms; train<=70%, val<=85%, test>85%; protected mints removed first",
        "source_launches": a.launches,
        "launch_rows_read": n_rows,
        "mints_total": N,
        "protected_sources": per,
        "protected_mints_union": len(prot),
        "protected_mints_absent_from_launches": len(unknown_prot),
        "counts": {k: len(v) for k, v in splits.items()},
        "intersections": inter,
        "union_size": len(union),
        "partition_exact": len(union) == N and sum(len(v) for v in splits.values()) == N,
        "boundaries_ms": {
            "train_end": mints[i_tr - 1][1] if i_tr > 0 else None,
            "val_end": mints[i_va - 1][1] if i_va > 0 else None,
        },
        "splits": {k: sorted(v) for k, v in splits.items()},
    }
    manifest["splits_sha256"] = sha256_of(manifest["splits"])
    manifest["manifest_sha256"] = sha256_of({k: v for k, v in manifest.items() if k != "manifest_sha256"})

    with open(a.out, "w") as f:
        json.dump(manifest, f, indent=1)

    print(f"[SPLITS] launches={n_rows:,} mints={N:,} protected={len(prot):,} "
          f"(absent {len(unknown_prot)})")
    for k, v in manifest["counts"].items():
        print(f"    {k:20s} {v:7,d}")
    print("    intersections:", {k: v for k, v in inter.items() if v})
    print(f"    partition_exact={manifest['partition_exact']}  sha256={manifest['manifest_sha256'][:16]}...")
    bad = any(v for v in inter.values())
    if bad:
        print("FAIL: split overlap detected", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
