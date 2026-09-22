"""Authoritatively resolve EVERY AMM mint's pool(s) via getProgramAccounts + memcmp.

WHY THIS BEATS SAMPLING SIGNATURES: sampling 4 signatures per mint missed ~49% of
mints because per-signature pool extraction fails ~16% of the time ((0.84)^4 ~ 0.5).
getProgramAccounts with a memcmp filter on the pool's base_mint returns ALL pools for
a base mint in ONE call - no sampling, no miss rate, and multi-pool mints are found
by construction instead of being inferred.

Validates the account layout against known (mint, pool) pairs BEFORE trusting the
memcmp offset, then decodes each pool's quote asset from its own bytes.
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
from amm_history_acquire import load_key  # noqa: E402

TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
QC = "/training/v2/reports/QUOTE_CLASSIFICATION_V1.json"
OUT = "/training/v2/reports/AMM_POOLS_RESOLVED_V1.json"
AMM_PROG = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"

WATCH = {
    "WSOL": "So11111111111111111111111111111111111111112",
    "USDC": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    "USDT": "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
    "PUMP": "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
}
ALPH = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58d(s):
    n = 0
    for ch in s:
        n = n * 58 + ALPH.index(ch)
    pad = len(s) - len(s.lstrip("1"))
    return b"\x00" * pad + n.to_bytes(32, "big")


PAT = {k: b58d(v) for k, v in WATCH.items()}
MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')

# AMM mints = mints with >=1 pumpswap row (verified 766 against the tape)
amm_mints = set()
rows_by_mint = collections.Counter()
for line in open(TAPE, encoding="utf-8"):
    m = MINT.search(line)
    if not m:
        continue
    mint = m.group(1)
    rows_by_mint[mint] += 1
    v = VEN.search(line)
    if v and v.group(1) == "pumpswap":
        amm_mints.add(mint)

key = load_key()
URL = f"https://mainnet.helius-rpc.com/?api-key={key}"


def rpc(payload, tries=6):
    body = json.dumps(payload).encode()
    delay = 1.0
    for a in range(tries):
        try:
            req = urllib.request.Request(
                URL, data=body, headers={"Content-Type": "application/json"})
            return json.loads(urllib.request.urlopen(req, timeout=90).read().decode())
        except urllib.error.HTTPError as e:
            if e.code in (429, 502, 503, 504) and a < tries - 1:
                time.sleep(delay)
                delay = min(delay * 2, 20.0)
                continue
            raise
        except Exception:
            if a < tries - 1:
                time.sleep(delay)
                delay = min(delay * 2, 20.0)
                continue
            raise


# --- 1. VALIDATE the layout on known (mint, pool) pairs from the classification ---
qc = json.load(open(QC, encoding="utf-8"))
known = [(m, d["pool"], d["quote"]) for m, d in qc["per_mint"].items()
         if d.get("pool") and d.get("quote") == "WSOL"][:3]
layout = []
for mint, pool, _q in known:
    r = rpc({"jsonrpc": "2.0", "id": 1, "method": "getAccountInfo",
             "params": [pool, {"encoding": "base64"}]})
    v = (r.get("result") or {}).get("value")
    if not v:
        layout.append({"pool": pool, "error": "no account"})
        continue
    raw = base64.b64decode(v["data"][0])
    mb, wb = b58d(mint), PAT["WSOL"]
    offs = [i for i in range(0, len(raw) - 32)
            if raw[i:i + 32] == mb]
    w_offs = [i for i in range(0, len(raw) - 32)
              if raw[i:i + 32] == wb]
    layout.append({"pool": pool[:12], "len": len(raw),
                   "base_mint_offsets": offs, "wsol_offsets": w_offs})

print(json.dumps({"layout_probe": layout}, indent=1), flush=True)

# derive the offset from the probe, falling back to the documented layout
BASE_OFF = None
for L in layout:
    if L.get("base_mint_offsets"):
        BASE_OFF = L["base_mint_offsets"][0]
        break
print(json.dumps({"base_mint_offset_used": BASE_OFF}), flush=True)
if BASE_OFF is None:
    raise SystemExit("could not establish base_mint offset")


# --- 2. one getProgramAccounts per AMM mint -> ALL its pools ---
def pools_for(mint):
    try:
        r = rpc({"jsonrpc": "2.0", "id": 1, "method": "getProgramAccounts",
                 "params": [AMM_PROG, {
                     "encoding": "base64",
                     "filters": [{"memcmp": {"offset": BASE_OFF,
                                             "bytes": mint}}]}]})
    except Exception as e:
        return mint, None, str(e)[:80]
    vals = r.get("result") or []
    out = []
    for it in vals:
        raw = base64.b64decode(it["account"]["data"][0])
        q = [k for k, p in PAT.items() if p in raw]
        out.append({"pool": it["pubkey"],
                    "owner": it["account"].get("owner"),
                    "quotes": q})
    return mint, out, None


results, errors = {}, {}
with ThreadPoolExecutor(max_workers=4) as ex:
    for mint, pools, err in ex.map(pools_for, sorted(amm_mints)):
        if err:
            errors[mint] = err
        else:
            results[mint] = pools

resolved = {m: p for m, p in results.items() if p}
pools_all = collections.Counter()
for p in resolved.values():
    for d in p:
        pools_all[d["pool"]] += 1

doc = {
    "schema": "amm_pools_resolved_v1",
    "method": "getProgramAccounts(pump-amm, memcmp base_mint) per AMM mint",
    "amm_mints": len(amm_mints),
    "mints_resolved": len(resolved),
    "mints_failed": len(errors),
    "distinct_pools": len(pools_all),
    "mints_with_multiple_pools": sum(1 for p in resolved.values() if len(p) > 1),
    "quote_counts": dict(collections.Counter(
        "+".join(d["quotes"]) if d["quotes"] else "NONE"
        for p in resolved.values() for d in p)),
    "owner_counts": dict(collections.Counter(
        d["owner"] for p in resolved.values() for d in p)),
    "rows_by_mint": dict(rows_by_mint),
    "per_mint_pools": resolved,
    "errors": errors,
}
os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=1)
print(json.dumps({k: v for k, v in doc.items()
                  if k not in ("per_mint_pools", "errors", "rows_by_mint")},
                 indent=1), flush=True)
