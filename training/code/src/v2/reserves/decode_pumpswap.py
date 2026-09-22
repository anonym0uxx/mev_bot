#!/usr/bin/env python
"""Decode pump-amm pool reserves from raw captures (read-only on /mnt/data).

Verified from real bytes: Pool account disc f19a6d0411b16dbc with the two mints at
43/75 (one of them wrapped SOL) and vaults at 139/171; Buy/Sell event discs
67f4521f2cf57777 / 3e2f370aa503dc2a with the pool pubkey at offset 120 and a unix-
seconds timestamp at offset 8. Reserve field offsets differ per side, so they are
resolved empirically against recorded fill prices.
"""
from __future__ import annotations

import base64
import glob
import hashlib
import json
import os
import struct
import subprocess

import numpy as np

ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
WSOL_B58 = "So11111111111111111111111111111111111111112"
PUMPAMM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
D_POOL = bytes.fromhex(hashlib.sha256(b"account:Pool").hexdigest()[:16])
D_BUY = bytes.fromhex(hashlib.sha256(b"event:BuyEvent").hexdigest()[:16])
D_SELL = bytes.fromhex(hashlib.sha256(b"event:SellEvent").hexdigest()[:16])
CAND = {"buy": [(48, 56), (32, 40), (56, 64), (40, 48)],
        "sell": [(48, 56), (56, 64), (40, 48), (32, 40)]}
OFF_POOL_PK = 120
OFF_TS = 8
U64S = (16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112)


def b58enc(b: bytes) -> str:
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = ALPHA[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s


def b58dec(s: str) -> int:
    n = 0
    for ch in s:
        n = n * 58 + ALPHA.index(ch)
    return n


def decode_pool_account(raw: bytes):
    if len(raw) < 240 or raw[:8] != D_POOL:
        return None
    m1, m2 = b58enc(raw[43:75]), b58enc(raw[75:107])
    if WSOL_B58 not in (m1, m2):
        return None
    wsol_is_base = (m1 == WSOL_B58)
    return {"mint": (m2 if wsol_is_base else m1), "base_mint": m1, "quote_mint": m2,
            "wsol_is_base": wsol_is_base,
            "base_vault": b58enc(raw[139:171]), "quote_vault": b58enc(raw[171:203])}


def decode_swap_event(raw: bytes):
    if len(raw) < 152:
        return None
    if raw[:8] == D_BUY:
        side = "buy"
    elif raw[:8] == D_SELL:
        side = "sell"
    else:
        return None
    ts = struct.unpack_from("<q", raw, OFF_TS)[0]
    return {"side": side, "ts_ms": int(ts) * 1000 if ts < 10 ** 12 else int(ts),
            "pool": b58enc(raw[OFF_POOL_PK:OFF_POOL_PK + 32]),
            "u": {o: struct.unpack_from("<Q", raw, o)[0] for o in U64S},
            "fees": {"lp": struct.unpack_from("<Q", raw, 80)[0],
                     "protocol": struct.unpack_from("<Q", raw, 96)[0]}}


def scan_parts(parts, max_lines=0):
    """-> (pools, events). Read-only; stderr from zstd is ignored."""
    pools, events = {}, []
    seen = 0
    for path in parts:
        pr = subprocess.Popen(["zstd", "-dc", path], stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL)
        for line in pr.stdout:
            seen += 1
            if max_lines and seen > max_lines:
                pr.kill()
                break
            try:
                o = json.loads(line)
            except Exception:
                continue
            rt, pl = o.get("record_type"), (o.get("payload") or {})
            if rt == "account" and pl.get("owner_b58") == PUMPAMM:
                info = decode_pool_account(base64.b64decode(pl.get("data_b64") or ""))
                if info:
                    info["pubkey"] = pl.get("pubkey_b58")
                    pools[info["pubkey"]] = info
            elif rt == "transaction":
                sig = pl.get("signature_b58")
                for L in (pl.get("meta") or {}).get("log_messages") or []:
                    if "Program data: " not in L:
                        continue
                    try:
                        raw = base64.b64decode(L.split("Program data: ", 1)[1])
                    except Exception:
                        continue
                    ev = decode_swap_event(raw)
                    if ev:
                        ev["signature"] = sig
                        events.append(ev)
        pr.wait()
        if max_lines and seen > max_lines:
            break
    return pools, events


def pick_layout(events, pools, price_by_sig):
    """Choose (base_off, quote_off) per side by agreement with recorded prices."""
    out = {}
    for side in ("buy", "sell"):
        best = None
        for bo, qo in CAND[side]:
            ratios = []
            for ev in events:
                if ev["side"] != side or ev["signature"] not in price_by_sig:
                    continue
                info = pools.get(ev["pool"])
                if not info:
                    continue
                sol = ev["u"][bo] if info["wsol_is_base"] else ev["u"][qo]
                tok = ev["u"][qo] if info["wsol_is_base"] else ev["u"][bo]
                if sol > 0 and tok > 0:
                    ratios.append(abs(np.log((sol / tok) / price_by_sig[ev["signature"]])))
            if ratios:
                med = float(np.median(ratios))
                if best is None or med < best[0]:
                    best = (med, bo, qo, len(ratios))
        if best:
            out[side] = {"median_abs_log_ratio": round(best[0], 6), "base_off": best[1],
                         "quote_off": best[2], "n_scored": best[3]}
    return out


def reserve_pair(ev, info, layout):
    bo = layout[ev["side"]]["base_off"]
    qo = layout[ev["side"]]["quote_off"]
    base, quote = ev["u"][bo], ev["u"][qo]
    sol, tok = (base, quote) if info["wsol_is_base"] else (quote, base)
    return sol, tok


def load_tape_prices(tape_path, venues=("pumpswap",)):
    prices, meta = {}, {}
    if not tape_path or not os.path.exists(tape_path):
        return prices, meta
    with open(tape_path) as fh:
        for line in fh:
            r = json.loads(line)
            if venues and r.get("venue") not in venues:
                continue
            tok = abs(int(r.get("tokens_raw") or 0))
            if not tok:
                continue
            prices[r["signature"]] = abs(int(r["sol_lamports"])) / tok
            meta[r["signature"]] = r["mint"]
    return prices, meta


def main():
    import argparse
    import pyarrow as pa
    import pyarrow.parquet as pq
    ap = argparse.ArgumentParser()
    ap.add_argument("--parts", nargs="+", required=True)
    ap.add_argument("--tape", default="/training/v2/canonical/renorm_pooltest/trades.jsonl")
    ap.add_argument("--out", default="/training/v2/reserves/forward")
    ap.add_argument("--tag", default="pumpswap_pool_reserves_v1")
    ap.add_argument("--report", default="/training/v2/reports/PUMPSWAP_RESERVES_DECODE.json")
    ap.add_argument("--max-lines", type=int, default=0)
    a = ap.parse_args()

    files = []
    for p in a.parts:
        files.extend(sorted(glob.glob(p)) if any(c in p for c in "*?[") else [p])
    prices, sig2mint = load_tape_prices(a.tape)
    pools, events = scan_parts(files, max_lines=a.max_lines)
    layout = pick_layout(events, pools, prices)

    rows, skipped_unknown_pool, skipped_zero = [], 0, 0
    for ev in events:
        info = pools.get(ev["pool"])
        if not info:
            skipped_unknown_pool += 1
            continue
        sol, tok = reserve_pair(ev, info, layout)
        if sol <= 0 or tok <= 0:
            skipped_zero += 1
            continue
        rows.append({"mint": info["mint"], "pool": ev["pool"],
                     "event_time_unix_ms": int(ev["ts_ms"]),
                     "pool_sol_lamports": int(sol), "pool_token_reserves_raw": int(tok),
                     "venue": "pumpswap", "slot": None,
                     "signature": ev["signature"],
                     "lp_fee_lamports": int(ev["fees"]["lp"]),
                     "protocol_fee_lamports": int(ev["fees"]["protocol"]),
                     "coin_creator_fee_lamports": 0, "source": "event_cpi_log"})
    rows.sort(key=lambda r: (r["event_time_unix_ms"], r["signature"] or ""))

    rep = {"parts": [os.path.basename(f) for f in files], "events": len(events),
           "pools_decoded": len(pools), "layout": layout,
           "rows": len(rows), "skipped_unknown_pool": skipped_unknown_pool,
           "skipped_zero_reserve": skipped_zero,
           "distinct_mints": len({r["mint"] for r in rows}),
           "k_checked": 0, "k_rel_drift_p50": None, "k_rel_drift_p90": None}
    if rows:
        rep["ts_min_ms"] = rows[0]["event_time_unix_ms"]
        rep["ts_max_ms"] = rows[-1]["event_time_unix_ms"]
        # k stability across consecutive events of the same pool (fee-inclusive
        # constant product: k must only grow by the LP fee, never jump)
        bypool = {}
        for r in rows:
            bypool.setdefault(r["pool"], []).append(r)
        drifts = []
        for pl, rs in bypool.items():
            ks = [r["pool_sol_lamports"] * r["pool_token_reserves_raw"] for r in rs]
            for i in range(1, len(ks)):
                if ks[i - 1]:
                    drifts.append(abs(ks[i] - ks[i - 1]) / ks[i - 1])
        drifts.sort()
        if drifts:
            rep["k_checked"] = len(drifts)
            rep["k_rel_drift_p50"] = drifts[len(drifts) // 2]
            rep["k_rel_drift_p90"] = drifts[int(0.9 * (len(drifts) - 1))]
        # price agreement with the recorded tape (independent check)
        ratios = []
        for r in rows:
            p = prices.get(r["signature"])
            if p and r["pool_token_reserves_raw"] > 0:
                ratios.append(abs(np.log((r["pool_sol_lamports"] / r["pool_token_reserves_raw"]) / p)))
        if ratios:
            rep["price_abs_log_ratio_vs_recorded_p50"] = float(np.median(ratios))
            rep["price_within_1pct_frac"] = float(np.mean([x < 0.01 for x in ratios]))
        os.makedirs(a.out, exist_ok=True)
        t = pa.table({k: pa.array([r.get(k) for r in rows]) for k in
                      ("mint", "pool", "event_time_unix_ms", "pool_sol_lamports",
                       "pool_token_reserves_raw", "venue", "slot", "signature",
                       "lp_fee_lamports", "protocol_fee_lamports",
                       "coin_creator_fee_lamports", "source")},
                     schema=pa.schema([("mint", pa.string()), ("pool", pa.string()),
                                       ("event_time_unix_ms", pa.int64()),
                                       ("pool_sol_lamports", pa.int64()),
                                       ("pool_token_reserves_raw", pa.int64()),
                                       ("venue", pa.string()), ("slot", pa.int64()),
                                       ("signature", pa.string()),
                                       ("lp_fee_lamports", pa.int64()),
                                       ("protocol_fee_lamports", pa.int64()),
                                       ("coin_creator_fee_lamports", pa.int64()),
                                       ("source", pa.string())]))
        out = os.path.join(a.out, a.tag + "_part0000.parquet")
        pq.write_table(t, out, compression="zstd")
        rep["parquet"] = out
    os.makedirs(os.path.dirname(a.report), exist_ok=True)
    json.dump(rep, open(a.report, "w"), indent=1)
    print(json.dumps(rep, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


