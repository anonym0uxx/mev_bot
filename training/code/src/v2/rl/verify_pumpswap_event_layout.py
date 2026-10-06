#!/usr/bin/env python
"""VERIFY the pumpswap (pump-amm) Anchor event layout against REAL captured bytes.

Derives the discriminators from the IDL naming convention (sha256('event:Name')[:8],
the same derivation that reproduces pump.fun's known event:TradeEvent =
bddb7fd34ee661ee and account:BondingCurve = 17b7f83760d8ac60), then scans a real
LaserStream capture part for pump-amm swap events and checks that the candidate
field offsets decode to plausible pool reserves with a constant product
(sol*tok) that survives across consecutive events of the same pool.

Read-only on /mnt/data. Prints a JSON verdict.
"""
from __future__ import annotations

import collections
import hashlib
import json
import struct
import subprocess
import sys

PARTS = [
    "/mnt/data/mev_bot-artifacts/raw/pumpfun_laserstream_raw_v1_20260824_053543_000288_part0000.ndjson.zst",
    "/mnt/data/mev_bot-artifacts/raw/pumpfun_laserstream_raw_v1_20260824_053543_000288_part0001.ndjson.zst",
]


def disc(name, kind):
    return hashlib.sha256(f"{kind}:{name}".encode()).hexdigest()[:16]


def main():
    out = {"derived": {k: disc(n, k) for k, n in (
        ("event", "TradeEvent"), ("account", "BondingCurve"),
        ("event", "BuyEvent"), ("event", "SellEvent"),
        ("event", "CreatePoolEvent"), ("account", "Pool"))}}
    out["known_reference"] = {"pumpfun_TradeEvent": "bddb7fd34ee661ee",
                              "pumpfun_BondingCurve_account": "17b7f83760d8ac60"}
    buy = bytes.fromhex(disc("BuyEvent", "event"))
    sell = bytes.fromhex(disc("SellEvent", "event"))
    pool = bytes.fromhex(disc("Pool", "account"))

    hits = collections.Counter()
    samples = []
    pool_events = collections.defaultdict(list)
    acct_pool = 0
    files_scanned = 0
    for path in PARTS:
        files_scanned += 1
        p = subprocess.Popen(["zstd", "-dc", path], stdout=subprocess.PIPE)
        for line in p.stdout:
            try:
                o = json.loads(line)
            except Exception:
                continue
            rt = o.get("record_type")
            pl = o.get("payload") or {}
            if rt == "transaction":
                logs = (pl.get("meta") or {}).get("log_messages") or []
                for L in logs:
                    if "Program data: " not in L:
                        continue
                    try:
                        import base64
                        raw = base64.b64decode(L.split("Program data: ", 1)[1])
                    except Exception:
                        continue
                    if len(raw) < 200:
                        continue
                    d = raw[:8].hex()
                    if d not in (buy.hex(), sell.hex()):
                        continue
                    hits[("buy" if d == buy.hex() else "sell")] += 1
                    # candidate layout, offsets from byte 8, all u64 unless noted
                    f = struct.unpack_from("<qQQQQQQQQQQQQ", raw, 8)
                    (ts, base_out, max_quote_in, user_base_res, user_quote_res,
                     pool_base_res, pool_quote_res, quote_in, lp_bps, lp_fee,
                     proto_bps, proto_fee, quote_in_lp) = f
                    ok = (0 < pool_quote_res < 10 ** 14 and 10 ** 9 < pool_base_res < 10 ** 20
                          and ts > 1_700_000_000_000)
                    if ok and len(samples) < 5:
                        samples.append({
                            "disc": d, "ts": ts, "pool_base_token_reserves": pool_base_res,
                            "pool_quote_token_reserves": pool_quote_res,
                            "lp_fee_basis_points": lp_bps, "lp_fee": lp_fee,
                            "protocol_fee_basis_points": proto_bps, "protocol_fee": proto_fee,
                            "quote_amount_in": quote_in,
                            "quote_amount_in_with_lp_fee": quote_in_lp,
                            "k": pool_base_res * pool_quote_res})
                    # pool pubkey sits 32 bytes after the last u64 we can trust
                    try:
                        pool_pk = raw[8 + 14 * 8: 8 + 14 * 8 + 32]
                        pool_events[pool_pk.hex()].append((pool_quote_res, pool_base_res))
                    except Exception:
                        pass
            elif rt == "account":
                raw_b64 = pl.get("data_b64")
                if not raw_b64:
                    continue
                try:
                    import base64
                    raw = base64.b64decode(raw_b64)
                except Exception:
                    continue
                if len(raw) >= 8 and raw[:8] == pool:
                    acct_pool += 1

    out["events_seen"] = dict(hits)
    out["sample_decodes"] = samples
    out["pool_accounts_seen_in_stream"] = acct_pool
    # constant product stability over consecutive events of the same pool
    ratios = []
    for pk, evs in pool_events.items():
        if len(evs) < 3:
            continue
        evs = evs[:20]
        ks = [a * b for a, b in evs if a > 0 and b > 0]
        for i in range(1, len(ks)):
            if ks[i - 1]:
                ratios.append(abs(ks[i] - ks[i - 1]) / ks[i - 1])
    ratios.sort()
    out["k_rel_drift_on_consecutive_same_pool_events"] = {
        "n": len(ratios),
        "p50": ratios[len(ratios) // 2] if ratios else None,
        "p90": ratios[int(0.9 * (len(ratios) - 1))] if ratios else None,
        "max": ratios[-1] if ratios else None,
        "note": "k should drift by only the LP fee between consecutive swaps",
    }
    out["files_scanned"] = files_scanned
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
