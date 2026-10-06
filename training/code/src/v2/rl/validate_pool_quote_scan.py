#!/usr/bin/env python
"""Validate the pool-account quote scan against ground truth before it is trusted.

The batch scan searches pool account bytes for known quote-asset pubkeys. That is
necessary but not sufficient: the account contains BOTH the base mint and the quote
mint, so a hit does not by itself say which role the asset plays, and a miss does not
prove the pool is non-WSOL.

This probe answers, on a sample stratified by the scan's own verdict:
  1. is the pool's BASE mint (the mint the backfill recorded for that pool) actually
     present in the account, and at what offset? (if absent, the account is not the
     pool we think it is -> the whole label is invalid)
  2. where does each known quote asset sit relative to the base mint offset?
  3. for pools labelled NONE, what quote asset is actually there? (unknown xStock, or
     a scan failure)
"""
from __future__ import annotations

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
from resolve_pool_quotes import PAT, b58d, post  # noqa: E402

BACKFILL = "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl"
RESOLVED = "/training/v2/reports/POOL_QUOTES_RESOLVED_V2.jsonl"
N_PER_CLASS = 40


def b58e(b):
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"[r] + s
    pad = len(b) - len(b.lstrip(b"\x00"))
    return "1" * pad + (s or "1")


def main():
    pool_mint = {}
    with open(BACKFILL, encoding="utf-8") as f:
        for line in f:
            i = line.find('"pool": "')
            m = line.find('"mint": "')
            if i < 0 or m < 0:
                continue
            j, k = i + 9, i + 9
            k = line.find('"', j)
            pool = line[j:k]
            mj = m + 9
            mk = line.find('"', mj)
            if pool not in pool_mint:
                pool_mint[pool] = line[mj:mk]
    print(json.dumps({"pools_with_mint": len(pool_mint)}), flush=True)

    by_class = collections.defaultdict(list)
    with open(RESOLVED, encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            cls = "+".join(r["quotes"]) if r["quotes"] else "NONE"
            if len(by_class[cls]) < N_PER_CLASS:
                by_class[cls].append(r["pool"])
    sample = []
    for cls, pools in sorted(by_class.items()):
        for p in pools:
            sample.append((cls, p))
    print(json.dumps({k: len(v) for k, v in by_class.items()}), flush=True)

    key = load_key()
    url = f"https://mainnet.helius-rpc.com/?api-key={key}"
    out = []
    B = 50
    for s in range(0, len(sample), B):
        chunk = sample[s:s + B]
        payload = [{"jsonrpc": "2.0", "id": i, "method": "getAccountInfo",
                    "params": [p, {"encoding": "base64"}]}
                   for i, (_c, p) in enumerate(chunk)]
        try:
            res = post(url, payload)
        except Exception as e:
            print("batch failed", e)
            continue
        by_id = {r.get("id"): r for r in (res if isinstance(res, list) else [res])}
        for i, (cls, pool) in enumerate(chunk):
            r = by_id.get(i) or {}
            v = (r.get("result") or {}).get("value")
            rec = {"scan_class": cls, "pool": pool, "mint": pool_mint.get(pool)}
            if not v:
                rec["status"] = "no_account"
                out.append(rec)
                continue
            raw = base64.b64decode(v["data"][0])
            rec["len"] = len(raw)
            rec["owner"] = v.get("owner")
            mb = b58d(rec["mint"]) if rec["mint"] else None
            rec["base_offsets"] = ([o for o in range(0, len(raw) - 31)
                                    if raw[o:o + 32] == mb] if mb else [])
            rec["quote_offsets"] = {name: [o for o in range(0, len(raw) - 31)
                                           if raw[o:o + 32] == pat]
                                    for name, pat in PAT.items()}
            # every 32-byte window that looks like a pubkey, to find the unknown quote
            rec["unknown_32byte_aligned_at_96_128_160"] = [
                b58e(raw[off:off + 32])
                for off in (96, 128, 160) if off + 32 <= len(raw)]
            out.append(rec)
        time.sleep(0.5)

    with open("/training/v2/reports/POOL_QUOTE_VALIDATION.json", "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1)

    agg = collections.Counter()
    for r in out:
        if r.get("status") == "no_account":
            agg[(r["scan_class"], "no_account")] += 1
            continue
        agg[(r["scan_class"], "base_present" if r["base_offsets"] else "base_ABSENT")] += 1
        if r["scan_class"] == "NONE":
            found = [k for k, v in r["quote_offsets"].items() if v]
            agg[("NONE", "scan_found_" + ("+".join(found) if found else "nothing"))] += 1
    print(json.dumps({"+".join(map(str, k)): v for k, v in sorted(agg.items())}, indent=1))
    for r in out:
        if r.get("scan_class") == "NONE" and not r.get("base_offsets"):
            print("NONE/absent-base example:", json.dumps(r)[:600])
            break


if __name__ == "__main__":
    main()
