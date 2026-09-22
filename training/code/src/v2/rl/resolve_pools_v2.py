#!/usr/bin/env python
"""AUTHORITATIVE pool -> quote resolution per AMM mint (fixes the accounts[0] mess).

Why this replaces the first attempt: the backfill's pool address came from
`accounts[0]` of the pump-amm instruction. Validating it showed that ~half of those
addresses are NOT pump-amm Pool accounts at all (137-byte foreign accounts, closed
accounts), and a byte scan that merely looks for a known quote asset cannot tell a
pool's BASE from its QUOTE. Neither is good enough to put a price in a prompt.

The authoritative method: getProgramAccounts(pump-amm, memcmp(base_mint @43) = mint)
returns exactly the pools whose BASE is that mint - the role is pinned by the filter,
not guessed - and dataSlice[43:107] then gives base+quote directly. Every returned
pool is self-verified: bytes[0:32] must equal the mint we asked for.

Layout (measured, 301-byte accounts, discr f19a6d0411b16dbc):
    [0:8] discriminator | [8] bump | [9:11] index | [11:43] creator
    [43:75] base_mint   | [75:107] quote_mint | [107:139] lp_mint | ...

Output: /training/v2/reports/AMM_POOLS_RESOLVED_V2.jsonl (one row per pool, resumable)
        /training/v2/reports/AMM_POOLS_RESOLVED_V2_SUMMARY.json
Cost: ~1 JSON-RPC batch per ~20 mints -> ~40 HTTP requests for the whole AMM set.
"""
from __future__ import annotations

import argparse
import base64
import collections
import json
import os
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from amm_acquire_fast import load_key  # noqa: E402
from validate_pool_quote_scan import b58e  # noqa: E402

AMM_PROG = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
BACKFILL = "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl"
OUT = "/training/v2/reports/AMM_POOLS_RESOLVED_V2.jsonl"
SUMMARY = "/training/v2/reports/AMM_POOLS_RESOLVED_V2_SUMMARY.json"

WATCH = {
    "WSOL": "So11111111111111111111111111111111111111112",
    "USDC": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
    "USDT": "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
    "PUMP": "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
    "XSTOCK_1": "XsoCS1TfEyfFhfvj8EtZ528L3CaKBDBRqRapnBbDF2W",
    "XSTOCK_2": "XspzcW1PRtgf6Wj92HCiZdjzKCyFekVD8P5Ueh3dRMX",
    "XSTOCK_3": "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh",
}

ALPH = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58d(s):
    n = 0
    for ch in s:
        n = n * 58 + ALPH.index(ch)
    pad = len(s) - len(s.lstrip("1"))
    return b"\x00" * pad + n.to_bytes(32, "big")


def post(url, payload, tries=6):
    body = json.dumps(payload).encode()
    delay = 1.0
    for a in range(tries):
        try:
            req = urllib.request.Request(url, data=body,
                                         headers={"Content-Type": "application/json"})
            return json.loads(urllib.request.urlopen(req, timeout=120).read().decode())
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


def amm_mints_from_backfill(path):
    mints = set()
    with open(path, encoding="utf-8") as f:
        for line in f:
            i = line.find('"mint": "')
            if i < 0:
                continue
            j = i + 9
            mints.add(line[j:line.find('"', j)])
    return mints


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--backfill", default=BACKFILL)
    ap.add_argument("--out", default=OUT)
    ap.add_argument("--batch", type=int, default=20)
    ap.add_argument("--pause", type=float, default=0.4)
    ap.add_argument("--mints-file", default="")
    a = ap.parse_args()

    if a.mints_file:
        mints = [l.strip() for l in open(a.mints_file) if l.strip()]
    else:
        mints = sorted(amm_mints_from_backfill(a.backfill))
    done = set()
    if os.path.exists(a.out):
        with open(a.out, encoding="utf-8") as f:
            for line in f:
                try:
                    r = json.loads(line)
                    if r.get("mint"):
                        done.add(r["mint"])
                except Exception:
                    pass
    todo = [m for m in mints if m not in done]
    print(json.dumps({"amm_mints": len(mints), "already_done": len(done),
                      "todo": len(todo),
                      "batches": (len(todo) + a.batch - 1) // a.batch}), flush=True)

    url = "https://mainnet.helius-rpc.com/?api-key=" + load_key()
    counts = collections.Counter()
    t0 = time.time()
    with open(a.out, "a", encoding="utf-8") as fo:
        for s in range(0, len(todo), a.batch):
            chunk = todo[s:s + a.batch]
            payload = []
            for i, m in enumerate(chunk):
                payload.append({
                    "jsonrpc": "2.0", "id": i, "method": "getProgramAccounts",
                    "params": [AMM_PROG, {
                        "encoding": "base64",
                        "filters": [{"memcmp": {"offset": 43, "bytes": m}}],
                        "dataSlice": {"offset": 43, "length": 64}}]})
            try:
                res = post(url, payload)
            except Exception as e:
                print(json.dumps({"batch_failed": str(e)[:160], "at": s}), flush=True)
                counts["batch_failed"] += len(chunk)
                continue
            by_id = {r.get("id"): r for r in (res if isinstance(res, list) else [res])}
            for i, m in enumerate(chunk):
                r = by_id.get(i) or {}
                if r.get("error"):
                    rec = {"mint": m, "pools": [], "error": str(r["error"])[:160]}
                    counts["rpc_error"] += 1
                else:
                    mb = b58d(m)
                    pools = []
                    for it in (r.get("result") or []):
                        try:
                            d = base64.b64decode(it["account"]["data"][0])
                        except Exception:
                            counts["undecodable"] += 1
                            continue
                        if len(d) < 64 or d[0:32] != mb:
                            counts["base_mismatch"] += 1
                            continue          # self-check: base role pinned
                        qb = d[32:64]
                        quote = "OTHER"
                        for name, addr in WATCH.items():
                            if qb == b58d(addr):
                                quote = name
                                break
                        pools.append({"pool": it["pubkey"],
                                      "owner": it["account"].get("owner"),
                                      "quote": quote})
                        counts["quote_" + quote] += 1
                    pools.sort(key=lambda p: (p["quote"] != "WSOL", p["pool"]))
                    rec = {"mint": m, "pools": pools,
                           "n_pools": len(pools),
                           "wsol_pools": [p["pool"] for p in pools if p["quote"] == "WSOL"]}
                    counts["mints_with_wsol" if rec["wsol_pools"] else
                           ("mints_no_pool" if not pools else "mints_no_wsol")] += 1
                fo.write(json.dumps(rec) + "\n")
            fo.flush()
            el = time.time() - t0
            print(json.dumps({"done": min(s + a.batch, len(todo)), "of": len(todo),
                              "elapsed_s": round(el, 1), "counts": dict(counts)}), flush=True)
            time.sleep(a.pause)

    summary = {"schema": "amm_pools_resolved_v2",
               "method": "getProgramAccounts(pump-amm, memcmp base_mint@43) + "
                         "dataSlice[43:107] + self-check bytes[0:32]==mint",
               "amm_mints": len(mints), "resolved_now": len(todo),
               "counts": dict(counts), "out": a.out}
    with open(SUMMARY, "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=1)
    print("SUMMARY", json.dumps(summary), flush=True)


if __name__ == "__main__":
    main()
