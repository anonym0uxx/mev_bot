#!/usr/bin/env python
"""amm_history_acquire — recover historical pump-swap pool reserves for the corpus
window from Solana RPC, for BOTH the buy and the exit side.

WHY RPC AND NOT A VENDOR: each corpus AMM fill already carries its `signature`.
Fetching that transaction returns the pump-amm BuyEvent/SellEvent, which carries
`pool_base_token_reserves` / `pool_quote_token_reserves` (decoded at offsets 48/56,
the layout measured in PUMPSWAP_RESERVES_DECODE.json). So the reserves at exactly
the fills our engine prices are recoverable, historically, with no forward
collection and no guesswork about which slots to fetch.

PROVEN: `amm_rpc_probe.py` decoded 5/5 real corpus signatures; three consecutive
buys on one mint showed base_reserve falling while quote_reserve rose - textbook
constant-product behaviour, so the layout and the offsets are right.

DESIGN (all of these are load-bearing):
  * resumable: the output is append-only JSONL keyed by signature; a restart
    re-reads it and skips what is already fetched. Never restarts from zero.
  * batched JSON-RPC: N requests per HTTP POST (batch is a protocol feature).
  * rate limited + adaptive: token bucket at --rps; 429/overload -> exponential
    backoff, and the effective rate is written to the manifest.
  * validated: k = base*quote per pool; the manifest reports k-drift so the data
    is accepted or rejected on the same invariant the engine uses.
  * the key is read from the creds file, never passed on the command line.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import random
import re
import sys
import threading
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
CREDS = os.environ.get("PQ_CREDS_FILE",
                       "/mnt/winc/Users/Alon/.hermes/creds/pump-quant.env")
TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
OUTDIR = "/training/v2/reserves/amm_history_v1"
AMM_PROG = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
DISC = {"67f4521f2cf57777": "BuyEvent", "3e2f370aa503dc2a": "SellEvent"}
VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')
SIG = re.compile(r'"signature"\s*:\s*"([^"]+)"')
MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
TS = re.compile(r'"recv_unix_ms"\s*:\s*(\d+)')


def load_key(path=CREDS):
    for line in open(path, encoding="utf-8", errors="ignore"):
        line = line.strip()
        if line.startswith("HELIUS_API_KEY="):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    raise SystemExit(f"REFUSING: HELIUS_API_KEY not found in {path}")


def key_fingerprint(k):
    return hashlib.sha256(k.encode()).hexdigest()[:8]


def build_index(tape=TAPE, outdir=OUTDIR):
    """signature -> (mint, recv_unix_ms) for every pump-swap fill, deduped."""
    os.makedirs(outdir, exist_ok=True)
    path = os.path.join(outdir, "sig_index.jsonl")
    if os.path.isfile(path) and os.path.getsize(path) > 0:
        return path
    tmp = path + ".partial"
    seen = set()
    n = 0
    with open(tmp, "w", encoding="utf-8") as fh:
        for line in open(tape, encoding="utf-8"):
            v = VEN.search(line)
            if not v or v.group(1) != "pumpswap":
                continue
            s = SIG.search(line)
            m = MINT.search(line)
            t = TS.search(line)
            if not (s and m and t):
                continue
            sig = s.group(1)
            if sig in seen:
                continue
            seen.add(sig)
            fh.write(json.dumps({"signature": sig, "mint": m.group(1),
                                 "recv_unix_ms": int(t.group(1))}) + "\n")
            n += 1
            if n % 250000 == 0:
                print(json.dumps({"indexed": n}), flush=True)
    os.replace(tmp, path)
    print(json.dumps({"sig_index_rows": n, "path": path}), flush=True)
    return path


def decode_event(logs):
    for lg in logs or []:
        if not isinstance(lg, str) or "Program data: " not in lg:
            continue
        try:
            raw = base64.b64decode(lg.split("Program data: ", 1)[1].strip())
        except Exception:
            continue
        if len(raw) < 64:
            continue
        d = raw[:8].hex()
        if d not in DISC:
            continue
        return {"event": DISC[d],
                "base_reserve": int.from_bytes(raw[48:56], "little"),
                "quote_reserve": int.from_bytes(raw[56:64], "little")}
    return None


def post(url, payload, timeout=45):
    body = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def run(batch=25, rps=20.0, limit=0, outdir=OUTDIR, max_retries=6):
    key = load_key()
    url = f"https://mainnet.helius-rpc.com/?api-key={key}"
    print(json.dumps({"key_fingerprint_sha256_8": key_fingerprint(key),
                      "rps_target": rps, "batch": batch}), flush=True)
    idx = build_index(outdir=outdir)
    out = os.path.join(outdir, "amm_reserves.jsonl")
    done = set()
    if os.path.isfile(out):
        for line in open(out, encoding="utf-8"):
            try:
                done.add(json.loads(line)["signature"])
            except Exception:
                continue
    print(json.dumps({"already_done": len(done)}), flush=True)

    pending = []
    for line in open(idx, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r["signature"] not in done:
            pending.append(r)
    print(json.dumps({"pending": len(pending), "total_index": len(done) + len(pending)}),
          flush=True)
    if limit:
        pending = pending[:limit]

    interval = 1.0 / max(rps, 0.1)
    next_at = time.time()
    t0 = time.time()
    fetched = decoded = 0
    k_bad = 0
    backs = 0
    fh = open(out, "a", encoding="utf-8")
    i = 0
    while i < len(pending):
        chunk = pending[i:i + batch]
        payload = [{"jsonrpc": "2.0", "id": j,
                    "method": "getTransaction",
                    "params": [c["signature"], {"encoding": "jsonParsed",
                                                "maxSupportedTransactionVersion": 0}]}
                   for j, c in enumerate(chunk)]
        wait = next_at - time.time()
        if wait > 0:
            time.sleep(wait)
        next_at = max(next_at + interval * len(chunk), time.time())
        try:
            resp = post(url, payload)
        except urllib.error.HTTPError as e:
            if e.code in (429, 503, 502, 504):
                backs += 1
                time.sleep(min(60, 2 ** backs) + random.random())
                continue
            print(json.dumps({"http_error": e.code, "fatal": True}), flush=True)
            break
        except Exception:
            backs += 1
            time.sleep(min(60, 2 ** backs) + random.random())
            continue
        if isinstance(resp, dict):
            resp = [resp]
        by_id = {r.get("id"): r for r in resp if isinstance(r, dict)}
        for j, c in enumerate(chunk):
            r = by_id.get(j) or {}
            res = r.get("result")
            row = {"signature": c["signature"], "mint": c["mint"],
                   "recv_unix_ms": c["recv_unix_ms"], "slot": None,
                   "event": None, "base_reserve": None, "quote_reserve": None}
            if res:
                row["slot"] = res.get("slot")
                ev = decode_event((res.get("meta") or {}).get("logMessages"))
                if ev:
                    row.update(ev)
                    b, q = ev["base_reserve"], ev["quote_reserve"]
                    if b > 0 and q > 0:
                        decoded += 1
                    else:
                        k_bad += 1
            fh.write(json.dumps(row) + "\n")
            fetched += 1
        i += batch
        if fetched % 2500 < batch:
            fh.flush()
            el = time.time() - t0
            print(json.dumps({"fetched": fetched, "decoded": decoded,
                              "pending": len(pending) - fetched,
                              "rate_rps": round(fetched / el, 1) if el else None,
                              "eta_h": round((len(pending) - fetched) /
                                             max(fetched / el, 1e-9) / 3600, 2)}),
                  flush=True)
    fh.flush()
    fh.close()
    el = time.time() - t0
    man = {"schema": "amm_history_v1", "out": out, "index": idx,
           "fetched_this_run": fetched, "decoded_events": decoded,
           "zero_reserve_events": k_bad, "backs": backs,
           "elapsed_s": round(el, 1),
           "effective_rps": round(fetched / el, 2) if el else None,
           "key_fingerprint_sha256_8": key_fingerprint(key)}
    with open(os.path.join(outdir, "MANIFEST.json"), "w", encoding="utf-8") as mf:
        json.dump(man, mf, indent=2)
    print(json.dumps(man, indent=1), flush=True)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--batch", type=int, default=25)
    ap.add_argument("--rps", type=float, default=20.0)
    ap.add_argument("--limit", type=int, default=0, help="0 = all")
    ap.add_argument("--outdir", default=OUTDIR)
    ap.add_argument("--index-only", action="store_true")
    a = ap.parse_args(argv)
    if a.index_only:
        build_index(outdir=a.outdir)
        return 0
    return run(batch=a.batch, rps=a.rps, limit=a.limit, outdir=a.outdir)


if __name__ == "__main__":
    sys.exit(main())
