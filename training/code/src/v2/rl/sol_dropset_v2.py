#!/usr/bin/env python
"""Recompute the SOL-only memecoin drop set against the AUTHORITATIVE pool map (V2).

WHY: candidate_sft_c4/c5 (and therefore the SFT-005 run) were filtered with
AMM_POOLS_RESOLVED_V1.json, whose pool address came from accounts[0] of the pump-amm
instruction. That is not a Pool account for ~half the rows (137/245-byte foreign
accounts under the pump-amm owner), and V1's per-mint map covers only 477 mints with a
byte-scan quote field that cannot tell base from quote. V2 resolves all 765 AMM mints
with getProgramAccounts(pump-amm, memcmp base_mint@43) + dataSlice[43:107] and a
byte[0:32]==mint self-check, and classifies every pool's quote at offset 75.

So the V1-based "no WSOL pool -> drop" verdict can be WRONG IN BOTH DIRECTIONS and the
corpus may be missing mints that V2 proves are WSOL-traded. This script recomputes the
keep/drop decision on V2 and reports the DELTA against V1 per mint and per row.

RULE (unchanged in spirit, now on data that survives a self-check):
  drop  : known quote asset (USDC/WSOL/USDT/PUMP + the identified xStock/other rows)
  drop  : the mint trades in pools and NONE of them is WSOL-quoted
          (a USDC pool's quote_reserve is 6dp USDC in a lamports slot -> 1000x bug)
  keep  : at least one WSOL-quoted pool
  keep  : no pool at all -> never graduated, bonding curve only, SOL by construction

OUT: /training/v2/reports/SOL_DROPSET_V2.json  (gate + annotation inputs for c6)
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import re

QUOTE_ASSETS = {
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v": "USDC",
    "So11111111111111111111111111111111111111112": "WSOL",
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB": "USDT",
    "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn": "PUMP",
}
SPLITS = ("train", "validation", "examination")
RE_MINT = re.compile(rb'"mint":\s*"([^"]+)"')


def sha256_file(path, chunk=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(chunk), b""):
            h.update(b)
    return h.hexdigest()


def load_v2(path):
    """mint -> {'pools': [{pool,quote}], 'wsol_pools': [...], 'n_pools': n}"""
    out = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            out[r["mint"]] = {"pools": r.get("pools") or [],
                              "wsol_pools": r.get("wsol_pools") or [],
                              "n_pools": int(r.get("n_pools") or 0)}
    return out


def load_v1(path):
    """mint -> [{'pool':..., 'quotes': [...]}] as c4 consumed it."""
    doc = json.load(open(path, encoding="utf-8"))
    return doc["per_mint_pools"]


def quote_asset_rows(zero_path):
    z = json.load(open(zero_path, encoding="utf-8"))
    return dict(z.get("quote_asset_rows") or {})


def corpus_universe(c3, splits=SPLITS):
    """Per-split line counts per mint, straight off the audit baseline bytes."""
    counts, mints = {}, {}
    for sp in splits:
        c = collections.Counter()
        n = 0
        with open(os.path.join(c3, sp + ".jsonl"), "rb") as f:
            for line in f:
                n += 1
                m = RE_MINT.search(line)
                if m:
                    c[m.group(1).decode()] += 1
        counts[sp] = c
        mints[sp] = set(c)
        print(json.dumps({"split": sp, "lines": n, "mints": len(c)}), flush=True)
    return counts, mints


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--v2", default="/training/v2/reports/AMM_POOLS_RESOLVED_V2.jsonl")
    ap.add_argument("--v1", default="/training/v2/reports/AMM_POOLS_RESOLVED_V1.json")
    ap.add_argument("--zero", default="/training/v2/reports/AMM_ZERO_POOL_MINTS_V1.json")
    ap.add_argument("--c3", default="/training/v2/candidate_sft_c3")
    ap.add_argument("--out", default="/training/v2/reports/SOL_DROPSET_V2.json")
    a = ap.parse_args()

    v2 = load_v2(a.v2)
    v1 = load_v1(a.v1)
    qa = quote_asset_rows(a.zero)
    print(json.dumps({"v2_mints": len(v2), "v1_mints": len(v1),
                      "quote_asset_rows": len(qa)}), flush=True)

    # ---- drop set as c4 computed it (V1) -----------------------------------
    drop_v1 = dict(QUOTE_ASSETS)
    drop_v1.update({m: "quote_asset" for m in qa})
    for mint, plist in v1.items():
        if mint in drop_v1:
            continue
        if "WSOL" not in {q for p in plist for q in (p.get("quotes") or [])}:
            drop_v1[mint] = "no_wsol_pool"
    # a corpus mint with no V1 row at all was never dropped by V1 (curve-only)

    # ---- drop set on V2 ----------------------------------------------------
    drop_v2 = dict(QUOTE_ASSETS)
    drop_v2.update({m: "quote_asset" for m in qa})
    never_graduated = set()
    wsol_by_mint = {}
    for mint, e in v2.items():
        if e["n_pools"] == 0:
            never_graduated.add(mint)
            continue
        wsol_by_mint[mint] = list(e["wsol_pools"])
        if mint in drop_v2:
            continue
        if not e["wsol_pools"]:
            drop_v2[mint] = "no_wsol_pool"
    wsol_pools_all = {p for ps in wsol_by_mint.values() for p in ps}

    counts, mints = corpus_universe(a.c3)

    # ---- delta -------------------------------------------------------------
    added = sorted(set(drop_v2) - set(drop_v1))      # newly dropped (V1 kept it)
    removed = sorted(set(drop_v1) - set(drop_v2))    # V1 dropped it, V2 proves SOL -> RECOVERED
    universe = set().union(*[mints[s] for s in SPLITS]) if SPLITS else set()
    rows_added, rows_removed = {}, {}
    for sp in SPLITS:
        rows_added[sp] = int(sum(counts[sp].get(m, 0) for m in added))
        rows_removed[sp] = int(sum(counts[sp].get(m, 0) for m in removed))

    in_corpus_v2_no_pool = sorted(m for m in universe if m in never_graduated)
    report = {
        "schema": "sol_dropset_v2",
        "rule": ("drop quote assets and mints whose pools are all non-WSOL; keep "
                 "WSOL-pooled mints and curve-only (never graduated) mints"),
        "sources": {
            "v2_pool_map": {"path": a.v2, "sha256": sha256_file(a.v2), "mints": len(v2)},
            "v1_pool_map": {"path": a.v1, "sha256": sha256_file(a.v1), "mints": len(v1)},
            "zero_pool_doc": {"path": a.zero, "sha256": sha256_file(a.zero)},
            "audit_baseline": a.c3,
        },
        "drop": drop_v2,
        "drop_counts": dict(collections.Counter(drop_v2.values())),
        "drop_total": len(drop_v2),
        "kept_mints": len(universe) - len([m for m in universe if m in drop_v2]),
        "universe": {sp: {"lines": int(sum(counts[sp].values())), "mints": len(mints[sp])}
                     for sp in SPLITS},
        "wsol_pools": {"n": len(wsol_pools_all),
                       "mints_with_wsol": len(wsol_by_mint)},
        "mint_to_wsol_pools": wsol_by_mint,
        "never_graduated_mints": sorted(never_graduated),
        "never_graduated_in_corpus": len(in_corpus_v2_no_pool),
        "v1_vs_v2": {
            "v1_drop_total": len(drop_v1),
            "v2_drop_total": len(drop_v2),
            "newly_dropped_by_v2": added[:2000],
            "newly_dropped_n": len(added),
            "recovered_minus_dropped": removed[:2000],
            "recovered_n": len(removed),
            "rows_dropped_by_v2_not_v1": rows_added,
            "rows_recovered_vs_v1": rows_removed,
        },
        "verdict": ("V2 and V1 agree on the drop set" if not added and not removed
                    else "V2 CHANGES the drop set - rebuild required"),
    }
    with open(a.out, "w", encoding="utf-8") as f:
        json.dump(report, f, indent=1)
    print(json.dumps({k: v for k, v in report.items()
                      if k not in ("drop", "mint_to_wsol_pools",
                                   "never_graduated_mints")}, indent=1))
    print("WROTE", a.out)


if __name__ == "__main__":
    main()
