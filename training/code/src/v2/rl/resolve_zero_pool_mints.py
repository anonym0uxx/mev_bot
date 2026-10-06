"""Resolve the 289 zero-pool AMM mints: are they QUOTE assets rather than memecoins?

A mint that appears in pump-swap rows but is the BASE of no pool is almost certainly
the QUOTE of some pool - i.e. a quote asset (stablecoin, xStock, protocol token),
not a memecoin. Test: getProgramAccounts(memcmp at the QUOTE offset 75 = mint).
If pools come back, the mint is a quote asset and is excluded from a SOL-memecoin
corpus. If nothing comes back either way, it is a genuine anomaly to report.

Offsets are the ones established empirically: base_mint 43, quote_mint 75.
"""
import base64
import collections
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, "/training/v2/code/src/v2/rl")
from amm_history_acquire import load_key  # noqa: E402

SRC = "/training/v2/reports/AMM_POOLS_RESOLVED_V1.json"
OUT = "/training/v2/reports/AMM_ZERO_POOL_MINTS_V1.json"
AMM_PROG = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
OFF_BASE, OFF_QUOTE = 43, 75

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

src = json.load(open(SRC, encoding="utf-8"))
resolved = set(src["per_mint_pools"])
rows_by_mint = src["rows_by_mint"]

# AMM mints = every mint the resolver was asked about
amm_mints = set(src["per_mint_pools"]) | {
    m for m in rows_by_mint
}  # rows_by_mint is tape-wide; narrow using the resolver's own count
# The resolver was given exactly the mints with >=1 pumpswap row; recover them from
# its failure/empty set = mints asked about. Rebuild from the tape to be exact.
import re  # noqa: E402
TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')
amm = set()
for line in open(TAPE, encoding="utf-8"):
    m, v = MINT.search(line), VEN.search(line)
    if m and v and v.group(1) == "pumpswap":
        amm.add(m.group(1))

zero = sorted(amm - resolved)
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


def probe(mint):
    try:
        r = rpc({"jsonrpc": "2.0", "id": 1, "method": "getProgramAccounts",
                 "params": [AMM_PROG, {
                     "encoding": "base64",
                     "filters": [{"memcmp": {"offset": OFF_QUOTE,
                                             "bytes": mint}}]}]})
    except Exception as e:
        return mint, None, str(e)[:80]
    vals = r.get("result") or []
    bases = []
    for it in vals:
        raw = base64.b64decode(it["account"]["data"][0])
        bases.append({
            "pool": it["pubkey"],
            "base_mint_b58": _b58e(raw[OFF_BASE:OFF_BASE + 32]),
            "base_quotes": [k for k, p in PAT.items() if p in raw],
            "owner": it["account"].get("owner"),
        })
    return mint, bases, None


_B58A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def _b58e(b):
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = _B58A[r] + s
    pad = len(b) - len(b.lstrip(b"\x00"))
    return "1" * pad + (s or "")


out, errs = {}, {}
with ThreadPoolExecutor(max_workers=4) as ex:
    for mint, bases, err in ex.map(probe, zero):
        if err:
            errs[mint] = err
        else:
            out[mint] = bases

is_quote = {m: v for m, v in out.items() if v}
neither = sorted(m for m in out if not out[m])

doc = {
    "schema": "amm_zero_pool_mints_v1",
    "method": "getProgramAccounts(memcmp at QUOTE offset 75) for mints with no base pool",
    "amm_mints_total": len(amm),
    "resolved_as_base": len(resolved),
    "zero_pool_tested": len(zero),
    "identified_as_QUOTE": len(is_quote),
    "neither_base_nor_quote": len(neither),
    "errors": len(errs),
    "quote_asset_rows": {m: rows_by_mint.get(m, 0) for m in is_quote},
    "rows_removed_if_excluded": sum(rows_by_mint.get(m, 0) for m in is_quote),
    "neither_sample": neither[:10],
    "detail": out,
}
os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=1)
print(json.dumps({k: v for k, v in doc.items() if k != "detail"}, indent=1))
