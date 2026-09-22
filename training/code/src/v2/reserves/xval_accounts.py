#!/usr/bin/env python
"""Independent validation of ABSOLUTE reserve levels.

Every pump.fun bonding-curve account write that appears in the stream is the
post-transaction on-chain state.  A transaction that both emits a TradeEvent and
rewrites the curve account must therefore agree exactly on all four reserve
levels.  This is a level check, not a price check: it cannot pass by accident
from a self-consistent but wrong reconstruction.

Usage:
  python xval_accounts.py --workers 64
"""
from __future__ import annotations

import argparse, base64, collections, glob, json, struct, subprocess, sys
from concurrent.futures import ProcessPoolExecutor

PUMPFUN = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
TRADE = bytes.fromhex("bddb7fd34ee661ee")
CURVE = bytes.fromhex("17b7f83760d8ac60")

GLOBS = [
    "/mnt/data/mev_bot-artifacts/raw/pumpfun_laserstream_raw_*_part*.ndjson.zst",
    "/mnt/data/mev_bot-artifacts/recovered_raw/*/*.ndjson.zst",
    "/mnt/data/mev_bot-artifacts/north_star/capture/*/pumpfun_laserstream_raw_*.ndjson.zst",
]


def _curve_ok(vtok, vsol, rtok, rsol):
    return (0 < vsol <= 10 ** 13 and 0 < vtok <= 10 ** 18
            and 0 <= rsol <= vsol and 0 <= rtok <= vtok)


def process(path):
    st = dict(parts=1, curve_writes=0, event_rows=0, variant_events=0,
              txs_both=0, exact_quad=0, token_only=0, sol_only=0, mismatch=0,
              variant_token_match=0, variant_account_found=0)
    events = {}
    accts = collections.defaultdict(list)
    p = subprocess.Popen(["zstd", "-dc", path], stdout=subprocess.PIPE)
    for line in p.stdout:
        try:
            o = json.loads(line)
        except Exception:
            continue
        rt = o.get("record_type")
        pl = o.get("payload") or {}
        if rt == "transaction":
            sig = pl.get("signature_b58")
            logs = (pl.get("meta") or {}).get("log_messages") or []
            for L in logs:
                if "Program data: " not in L:
                    continue
                try:
                    raw = base64.b64decode(L.split("Program data: ", 1)[1])
                except Exception:
                    continue
                if raw[:8] != TRADE or len(raw) < 129 or raw[56] > 1:
                    continue
                vsol, vtok, rsol, rtok = struct.unpack_from("<QQQQ", raw, 97)
                st["event_rows"] += 1
                if vsol == 0 and rsol == 0:
                    st["variant_events"] += 1
                    events.setdefault(("V", sig), []).append((vtok, rtok))
                elif _curve_ok(vtok, vsol, rtok, rsol):
                    events.setdefault(("E", sig), []).append((vtok, vsol, rtok, rsol))
        elif rt == "account":
            if pl.get("owner_b58") != PUMPFUN:
                continue
            try:
                raw = base64.b64decode(pl.get("data_b64") or "")
            except Exception:
                continue
            if len(raw) < 48 or raw[:8] != CURVE:
                continue
            vtok, vsol, rtok, rsol = struct.unpack_from("<QQQQ", raw, 8)
            if not _curve_ok(vtok, vsol, rtok, rsol):
                continue
            st["curve_writes"] += 1
            accts[pl.get("txn_signature_b58")].append((vtok, vsol, rtok, rsol))
    p.stdout.close()
    p.wait()
    for (kind, sig), evs in events.items():
        a = accts.get(sig)
        if not a:
            continue
        st["txs_both"] += 1
        hit = False
        for e in evs:
            for q in a:
                if kind == "E":
                    if e == q:
                        hit = True
                        st["exact_quad"] += 1
                    elif e[0] == q[0] and e[2] == q[2]:
                        st["token_only"] += 1
                        st["sol_only"] += 0
                    elif e[1] == q[1] and e[3] == q[3]:
                        st["sol_only"] += 1
                    else:
                        st["mismatch"] += 1
                else:
                    if e[0] == q[0] and e[1] == q[2]:
                        st["variant_token_match"] += 1
                        st["variant_account_found"] += 1
                        hit = True
        if kind == "V" and not hit:
            st["variant_account_found"] += 0
    return st


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workers", type=int, default=64)
    ap.add_argument("--out", default="/training/v2/reports/RESERVES_ACCOUNT_XVAL.json")
    a = ap.parse_args()
    parts = []
    for g in GLOBS:
        parts.extend(sorted(glob.glob(g)))
    seen = set()
    parts = [p for p in parts if not (p in seen or seen.add(p))]
    if not parts:
        print("FATAL: zero raw parts matched - glob failure", file=sys.stderr)
        return 2
    print("raw_parts", len(parts))
    tot = collections.Counter()
    done = 0
    with ProcessPoolExecutor(max_workers=a.workers) as ex:
        for st in ex.map(process, parts, chunksize=1):
            done += 1
            tot.update(st)
            if done % 200 == 0:
                print("  parts", done, dict(tot))
    tot["raw_parts"] = len(parts)
    print("XVAL", json.dumps(dict(tot), indent=1))
    json.dump(dict(tot), open(a.out, "w"), indent=1)
    print("wrote", a.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())