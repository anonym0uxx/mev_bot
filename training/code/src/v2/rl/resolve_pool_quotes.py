#!/usr/bin/env python
"""Resolve the QUOTE ASSET of every pump-amm pool the backfill observes.

Why: the AMM annotation may only use WSOL-quoted pools. A USDC/xStock pool stores
6dp quote units in the field we treat as lamports (the 1000x bug), so an
UNCLASSIFIED pool must be annotated as absent rather than assumed WSOL. The earlier
classification (AMM_POOLS_RESOLVED_V1, 1,987 pools) came from 4 signatures per mint,
which under-samples the long tail; the backfill observes ~2,580+ pools.

Method: per-pool getAccountInfo, then search the ACCOUNT BYTES for known quote-asset
pubkeys. Byte search is robust to field offsets moving between program versions.
Pools are fetched in JSON-RPC BATCHES (100 per HTTP request) so this costs ~26-52
requests total and does NOT compete with the backfill's rate budget - which is the
documented failure mode (two full-concurrency RPC jobs -> HTTP 429 killed a job).

Output is append-only JSONL and resumable (skip-if-present), so it can be re-run for
the delta after the backfill completes. It also CROSS-VALIDATES every pool that the
earlier map already classified and reports agreement/disagreement.
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

AMM_PROG = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
BACKFILL = "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl"
OLD_MAP = "/training/v2/reports/AMM_POOLS_RESOLVED_V1.json"
OUT = "/training/v2/reports/POOL_QUOTES_RESOLVED_V2.jsonl"
SUMMARY = "/training/v2/reports/POOL_QUOTES_RESOLVED_V2_SUMMARY.json"

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


def post(url, payload, tries=6):
    body = json.dumps(payload).encode()
    delay = 1.0
    for a in range(tries):
        try:
            req = urllib.request.Request(url, data=body,
                                         headers={"Content-Type": "application/json"})
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--backfill", default=BACKFILL)
    ap.add_argument("--out", default=OUT)
    ap.add_argument("--batch", type=int, default=100)
    ap.add_argument("--rps", type=float, default=1.0)
    ap.add_argument("--pools-file", default="", help="explicit pool list (one per line)")
    ap.add_argument("--include-old-map", action="store_true",
                    help="also cross-validate pools from AMM_POOLS_RESOLVED_V1")
    a = ap.parse_args()

    pools = set()
    if a.pools_file:
        pools |= {l.strip() for l in open(a.pools_file) if l.strip()}
    n_bf = 0
    with open(a.backfill, encoding="utf-8") as f:
        for line in f:
            n_bf += 1
            i = line.find('"pool": "')
            if i < 0:
                continue
            j = i + 9
            k = line.find('"', j)
            pools.add(line[j:k])
    old_quotes = {}
    if os.path.exists(OLD_MAP):
        pm = json.load(open(OLD_MAP, encoding="utf-8"))["per_mint_pools"]
        for m, plist in pm.items():
            for e in plist:
                old_quotes[e["pool"]] = tuple(e.get("quotes") or ())
        if a.include_old_map:
            pools |= set(old_quotes)

    done = set()
    if os.path.exists(a.out):
        with open(a.out, encoding="utf-8") as f:
            for line in f:
                try:
                    done.add(json.loads(line)["pool"])
                except Exception:
                    pass
    todo = sorted(pools - done)
    print(json.dumps({"backfill_rows_scanned": n_bf, "pools_total": len(pools),
                      "already_done": len(done), "todo": len(todo),
                      "tracked_in_old_map": len(set(pools) & set(old_quotes))}), flush=True)

    key = load_key()
    url = f"https://mainnet.helius-rpc.com/?api-key={key}"
    interval = 1.0 / max(a.rps, 0.1)
    counts = collections.Counter()
    agree = disagree = 0
    t0 = time.time()
    with open(a.out, "a", encoding="utf-8") as fo:
        for s in range(0, len(todo), a.batch):
            chunk = todo[s:s + a.batch]
            payload = [{"jsonrpc": "2.0", "id": i, "method": "getAccountInfo",
                        "params": [p, {"encoding": "base64"}]} for i, p in enumerate(chunk)]
            try:
                res = post(url, payload)
            except Exception as e:
                print(json.dumps({"batch_failed": str(e)[:200], "at": s}), flush=True)
                counts["batch_failed"] += len(chunk)
                continue
            by_id = {r.get("id"): r for r in (res if isinstance(res, list) else [res])}
            for i, p in enumerate(chunk):
                r = by_id.get(i) or {}
                v = (r.get("result") or {}).get("value")
                if not v:
                    rec = {"pool": p, "owner": None, "len": 0, "quotes": [],
                           "note": "no_account"}
                    counts["no_account"] += 1
                else:
                    raw = base64.b64decode(v["data"][0])
                    quotes = []
                    ev = {}
                    for name, pat in PAT.items():
                        offs = [o for o in range(0, max(0, len(raw) - 31))
                                if raw[o:o + 32] == pat]
                        if offs:
                            quotes.append(name)
                            ev[name] = offs
                    rec = {"pool": p, "owner": v.get("owner"), "len": len(raw),
                           "quotes": quotes, "offsets": ev,
                           "lamports": v.get("lamports"), "slot": v.get("slot")}
                    counts["owner_" + str(v.get("owner"))] += 1
                    counts["quote_" + ("+".join(quotes) if quotes else "NONE")] += 1
                if p in old_quotes:
                    if set(old_quotes[p]) == set(rec["quotes"]):
                        agree += 1
                    else:
                        disagree += 1
                        rec["old_map_quotes"] = list(old_quotes[p])
                fo.write(json.dumps(rec) + "\n")
            fo.flush()
            elapsed = time.time() - t0
            print(json.dumps({"done": min(s + a.batch, len(todo)), "of": len(todo),
                              "elapsed_s": round(elapsed, 1),
                              "pools_per_s": round(min(s + a.batch, len(todo)) / max(elapsed, .1), 1),
                              "counts": dict(counts)}), flush=True)
            time.sleep(interval)

    summary = {"schema": "pool_quotes_resolved_v2", "pools_total": len(pools),
               "resolved_now": len(todo), "cross_validated": agree + disagree,
               "agree": agree, "disagree": disagree,
               "counts": dict(counts), "out": a.out,
               "method": "getAccountInfo batch(100) + byte scan for known quote pubkeys",
               "concurrency_note": "batched: ~%d HTTP requests total" % (len(todo) // a.batch + 1)}
    with open(SUMMARY, "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=1)
    print("SUMMARY", json.dumps(summary), flush=True)


if __name__ == "__main__":
    main()
