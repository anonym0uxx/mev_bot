"""Build candidate_sft_c4: the SOL-only SFT corpus.

FILTER (settled against three independent probes + external research):
  keep a mint iff it is a pump.fun SOL memecoin, i.e. NOT a quote asset and
  (it has a WSOL-quoted pump-swap pool OR it never graduated - a bonding-curve
  token, which is SOL-denominated by construction).

  drop: USDC / WSOL / USDT / PUMP themselves, the xStock & other quote assets,
        and memecoin mints whose only pools are non-WSOL.

LABEL INTEGRITY (the binding constraint):
  lines are copied VERBATIM from c3 - the record is never re-serialized, so
  `messages` (the labels) are byte-identical by construction, not by inspection.
  c3 is only ever opened for READING; c4 is a new directory. No padding, no
  duplication.
"""
import hashlib
import json
import os
import shutil

C3 = "/training/v2/candidate_sft_c3"
C4 = "/training/v2/candidate_sft_c4"
MANIFEST = "/training/v2/reports/SFT_C4_MANIFEST.json"
POOLS = "/training/v2/reports/AMM_POOLS_RESOLVED_V1.json"
ZERO = "/training/v2/reports/AMM_ZERO_POOL_MINTS_V1.json"

QUOTE_ASSETS = {
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v": "USDC",
    "So11111111111111111111111111111111111111112": "WSOL",
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB": "USDT",
    "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn": "PUMP",
}

pools = json.load(open(POOLS, encoding="utf-8"))["per_mint_pools"]
zero = json.load(open(ZERO, encoding="utf-8"))

drop = dict(QUOTE_ASSETS)                       # quote assets queried as mints
for m in zero.get("neither_sample", []):
    pass
# the 5 identified quote assets (xStocks etc.)
qc_path = "/training/v2/reports/AMM_ZERO_POOL_MINTS_V1.json"
z = json.load(open(qc_path, encoding="utf-8"))
for m in (z.get("quote_asset_rows") or {}):
    drop[m] = "quote_asset"

# memecoin mints with pools but NO WSOL pool -> not SOL-tradable
no_sol = []
for mint, plist in pools.items():
    if mint in drop:
        continue
    quotes = {q for p in plist for q in (p.get("quotes") or [])}
    if "WSOL" not in quotes:
        no_sol.append(mint)
        drop[mint] = "no_wsol_pool"

print(json.dumps({
    "drop_reasons": {k: sum(1 for v in drop.values() if v == k)
                     for k in set(drop.values())},
    "drop_total": len(drop),
    "drop_list": {m: r for m, r in list(drop.items())[:20]},
    "mints_no_wsol_pool": no_sol[:10],
}, indent=1), flush=True)


def mint_of(line):
    i = line.find('"mint"')
    if i < 0:
        return None
    j = line.find('"', i + 7)
    k = line.find('"', j + 1)
    return line[j + 1:k] if j >= 0 and k > j else None


os.makedirs(C4, exist_ok=True)
manifest = {"schema": "sft_corpus_v4", "rule": "SOL-only pump.fun memecoins",
            "source": C3, "drop_set_size": len(drop), "splits": {}}

for split in ("train", "validation", "examination"):
    src = os.path.join(C3, f"{split}.jsonl")
    dst = os.path.join(C4, f"{split}.jsonl")
    h_src, h_dst = hashlib.sha256(), hashlib.sha256()
    n_in = n_out = 0
    with open(src, "r", encoding="utf-8") as fi, \
            open(dst, "w", encoding="utf-8") as fo:
        for line in fi:
            n_in += 1
            h_src.update(line.encode("utf-8"))
            m = mint_of(line)
            if m in drop:
                continue
            fo.write(line)          # VERBATIM - labels untouched
            h_dst.update(line.encode("utf-8"))
            n_out += 1
    manifest["splits"][split] = {
        "rows_in": n_in, "rows_out": n_out, "rows_dropped": n_in - n_out,
        "sha256": h_dst.hexdigest(),
        "src_sha256": h_src.hexdigest(),
        "path": dst,
    }
    print(json.dumps({split: manifest["splits"][split]}), flush=True)

with open(MANIFEST, "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=1)
print("WROTE", MANIFEST)
