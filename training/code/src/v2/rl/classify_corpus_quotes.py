"""Classify every corpus mint by its pool's QUOTE asset - SOL vs USDC vs other.

WHY THIS DOESN'T WAIT FOR THE BACKFILL: quote asset is a property of the POOL, and a
mint's pool is stable. So one AMM signature per mint is enough to identify the pool,
and one getAccountInfo per pool identifies the quote. ~3.6k + ~2k calls, not 2.8M.

Emits /training/v2/reports/QUOTE_CLASSIFICATION_V1.json:
  - per-mint: pool, quote asset, row counts, venues
  - the SOL-only memecoin filter (the rule SFT and RL must share)
"""
import base64
import collections
import json
import os
import re
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, "/training/v2/code/src/v2/rl")
from amm_acquire_fast import pool_of                       # noqa: E402
from amm_history_acquire import load_key                    # noqa: E402

TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
OUT = "/training/v2/reports/QUOTE_CLASSIFICATION_V1.json"

WATCH = {
    "WSOL": "So11111111111111111111111111111111111111112",
    "USDC": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    "USDT": "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
    "PUMP": "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
}
ALPH = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58(s):
    n = 0
    for ch in s:
        n = n * 58 + ALPH.index(ch)
    pad = len(s) - len(s.lstrip("1"))
    return b"\x00" * pad + n.to_bytes(32, "big")


PAT = {k: b58(v) for k, v in WATCH.items()}

MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')
SIG = re.compile(r'"signature"\s*:\s*"([^"]+)"')

# NOT one signature per mint: a mint can have MORE THAN ONE pump-swap pool (observed
# mint_to_pool histogram {1: 25, 2: 1}), so a single sample can silently pick the
# WSOL pool for a mint that also has a USDC pool. Sample K signatures per mint and
# union the pools found, so multi-pool mints are DETECTED instead of guessed.
K_SIGS = 4
rows_by_mint = collections.Counter()
sigs_by_mint = collections.defaultdict(list)
for line in open(TAPE, encoding="utf-8"):
    m = MINT.search(line)
    if not m:
        continue
    mint = m.group(1)
    rows_by_mint[mint] += 1
    if len(sigs_by_mint[mint]) >= K_SIGS:
        continue
    v = VEN.search(line)
    s = SIG.search(line)
    if v and v.group(1) == "pumpswap" and s:
        sg = s.group(1)
        if sg not in sigs_by_mint[mint]:
            sigs_by_mint[mint].append(sg)

key = load_key()
URL = f"https://mainnet.helius-rpc.com/?api-key={key}"


# RATE BUDGET: this job shares the plan's rate limit with the running backfill.
# Reproduced failure: 12 workers here + 12 there => HTTP 429 and this script died,
# because it had no retry. So: few workers, and back off on 429.
_n_lock = threading.Lock()


def rpc(payload, tries=6):
    body = json.dumps(payload).encode()
    delay = 1.0
    for attempt in range(tries):
        try:
            req = urllib.request.Request(
                URL, data=body, headers={"Content-Type": "application/json"})
            return json.loads(urllib.request.urlopen(req, timeout=90).read().decode())
        except urllib.error.HTTPError as e:
            if e.code in (429, 502, 503, 504) and attempt < tries - 1:
                time.sleep(delay)
                delay = min(delay * 2, 20.0)
                continue
            raise
        except Exception:
            if attempt < tries - 1:
                time.sleep(delay)
                delay = min(delay * 2, 20.0)
                continue
            raise


def tx_pool(sig):
    r = rpc({"jsonrpc": "2.0", "id": 1, "method": "getTransaction",
             "params": [sig, {"encoding": "jsonParsed",
                              "maxSupportedTransactionVersion": 0}]})
    res = r.get("result") or {}
    return pool_of(res)


pairs = [(m, s) for m in sorted(sigs_by_mint) for s in sigs_by_mint[m]]
print(json.dumps({"mints": len(sigs_by_mint), "signatures_to_fetch": len(pairs)}))
with ThreadPoolExecutor(max_workers=4) as ex:
    got = list(ex.map(tx_pool, [s for _, s in pairs]))
mint_pools = collections.defaultdict(set)
for (m, _), p in zip(pairs, got):
    if p:
        mint_pools[m].add(p)
multi = {m: sorted(ps) for m, ps in mint_pools.items() if len(ps) > 1}
mint_pool = {m: sorted(ps)[0] for m, ps in mint_pools.items()}

pools = sorted({p for ps in mint_pools.values() for p in ps})
pool_quote = {}


def acct(p):
    r = rpc({"jsonrpc": "2.0", "id": 1, "method": "getAccountInfo",
             "params": [p, {"encoding": "base64"}]})
    v = (r.get("result") or {}).get("value")
    if not v:
        return p, "MISSING", None
    raw = base64.b64decode(v["data"][0])
    q = [k for k, pat in PAT.items() if pat in raw]
    return p, ("+".join(q) if q else "OTHER"), v.get("owner")


with ThreadPoolExecutor(max_workers=4) as ex:
    for p, q, own in ex.map(acct, pools):
        pool_quote[p] = {"quote": q, "owner": own}

per_mint = {}
for mint in rows_by_mint:
    p = mint_pool.get(mint)
    q = pool_quote.get(p, {}).get("quote") if p else None
    per_mint[mint] = {"rows": rows_by_mint[mint], "pool": p, "quote": q,
                      "is_pump_launch": mint.endswith("pump")}

sol_mints = sorted(m for m, d in per_mint.items()
                   if d["quote"] == "WSOL" and d["is_pump_launch"])
other_mints = sorted(m for m in per_mint if m not in set(sol_mints))
sol_rows = sum(rows_by_mint[m] for m in sol_mints)

doc = {
    "schema": "quote_classification_v1",
    "method": ("4 pumpswap signatures per mint -> pool_of (union) -> pool account "
               "bytes; multi-pool mints detected, not guessed"),
    "preliminary_WARNING": (
        "NOT the authoritative filter: row->pool attribution needs the per-signature "
        "backfill for multi-pool mints. Use only to size the problem."
    ),
    "signatures_per_mint_sampled": K_SIGS,
    "mints_with_multiple_pools": len(multi),
    "multi_pool_examples": {m: ps for m, ps in list(multi.items())[:5]},
    "mints_total": len(per_mint),
    "mints_pool_resolved": len(mint_pool),
    "pools": len(pools),
    "quote_counts": dict(collections.Counter(
        (d["quote"] or "UNRESOLVED") for d in per_mint.values())),
    "pool_owner_counts": dict(collections.Counter(
        (d["owner"] or "MISSING") for d in pool_quote.values())),
    "mints_sol_memecoin": len(sol_mints),
    "mints_excluded": len(other_mints),
    "rows_total": sum(rows_by_mint.values()),
    "rows_sol_memecoin": sol_rows,
    "rows_excluded": sum(rows_by_mint.values()) - sol_rows,
    "rows_excluded_frac": round(
        (sum(rows_by_mint.values()) - sol_rows) / sum(rows_by_mint.values()), 6),
    "excluded_sample": sorted(other_mints, key=lambda m: -rows_by_mint[m])[:8],
    "sol_mints": sol_mints,
    "per_mint": per_mint,
}
os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=1)

print(json.dumps({k: v for k, v in doc.items()
                  if k not in ("sol_mints", "per_mint")}, indent=1))
