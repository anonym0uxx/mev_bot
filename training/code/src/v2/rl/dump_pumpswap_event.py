#!/usr/bin/env python
"""Dump the first real pump-amm swap events with every candidate field, so the
reserve offsets are established from the bytes instead of from memory."""
from __future__ import annotations

import base64
import hashlib
import json
import struct
import subprocess

PART = ("/mnt/data/mev_bot-artifacts/raw/"
        "pumpfun_laserstream_raw_v1_20260824_053543_000288_part0000.ndjson.zst")
BUY = hashlib.sha256(b"event:BuyEvent").hexdigest()[:16]
SELL = hashlib.sha256(b"event:SellEvent").hexdigest()[:16]
print("BuyEvent disc", BUY, "SellEvent disc", SELL)

names = ["timestamp@8", "u@16", "u@24", "u@32", "u@40", "u@48", "u@56", "u@64",
         "u@72", "u@80", "u@88", "u@96", "u@104", "u@112", "u@120", "u@128"]
shown = 0
p = subprocess.Popen(["zstd", "-dc", PART], stdout=subprocess.PIPE)
for line in p.stdout:
    if shown >= 3:
        break
    try:
        o = json.loads(line)
    except Exception:
        continue
    if o.get("record_type") != "transaction":
        continue
    pl = o.get("payload") or {}
    for L in (pl.get("meta") or {}).get("log_messages") or []:
        if "Program data: " not in L:
            continue
        try:
            raw = base64.b64decode(L.split("Program data: ", 1)[1])
        except Exception:
            continue
        if raw[:8].hex() not in (BUY, SELL) or len(raw) < 200:
            continue
        vals = struct.unpack_from("<q" + "Q" * 15, raw, 8)
        print("\n=== %s len=%d" % ("BUY" if raw[:8].hex() == BUY else "SELL", len(raw)))
        for n, v in zip(names, vals):
            print("  %-10s %d" % (n, v))
        # trailing pubkey candidates
        print("  pubkeys@:", [raw[o:o + 32].hex()[:12] for o in (120, 128, 136, 144)])
        print("  raw tail hex:", raw[104:170].hex())
        shown += 1
        break
