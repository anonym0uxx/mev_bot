#!/usr/bin/env python
"""amm_acquire_fast — concurrent fetcher for the AMM reserve backfill.

WHY THIS EXISTS: the sequential fetcher measured 15.7 signatures/s even when the
token bucket was raised to 150 rps, because throughput was bound by the ~3.2 s
round-trip of one batch POST, not by the rate limit. Serialising batches therefore
costs ~50 h for 2.81 M signatures. Concurrency is the actual fix.

Shares the proven decode with amm_history_acquire (offsets 48/56, 99.95% hit rate)
and writes the same append-only JSONL, so a run can be resumed by either tool.

Design: N worker threads each POST a JSON-RPC batch; a global token bucket throttles
REQUEST rate; on 429/5xx the batch backs off and is retried, and a batch that
exceeds --max-retries is recorded in retryable_failures.jsonl (NOT silently dropped)
so it can be picked up by a later pass.
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import random
import sys
import threading
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from amm_history_acquire import (  # noqa: E402
    AMM_PROG, OUTDIR, build_index, decode_event, key_fingerprint, load_key,
)


# Program IDs that can NEVER be the pool. Reproduced defect: taking accounts[0]
# blindly returned the System Program for 11 of 76 pools (14.5%), so pool identity
# must skip program accounts. The pool is always a writable NON-signer.
KNOWN_PROGRAMS = {
    "11111111111111111111111111111111",                              # System
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",                    # SPL Token
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",                    # Token-2022
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",                   # ATA
    "ComputeBudget111111111111111111111111111111",                    # ComputeBudget
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",                    # pump-amm
    "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",                    # pump.fun curve
}


def pool_of(res):
    """The pump-swap pool account: accounts[0] of the pump-amm instruction.

    WHY WE NEED IT: the Buy/SellEvent does NOT carry the pool address, so keying
    reserves by the tape's `mint` mixes the pools that token trades in (a token can
    be base in one pool and quote in another). That mixing is what made ~25% of
    cross-event k comparisons look inconsistent. pump-amm's buy/sell account order
    begins [pool, user, global_config, ...], so the instruction's first account IS
    the pool.
    """
    try:
        msg = (res.get("transaction") or {}).get("message") or {}
        keys = msg.get("accountKeys") or []
        meta = {k.get("pubkey"): k for k in keys if isinstance(k, dict)}
        for ix in (msg.get("instructions") or []):
            if ix.get("programId") != AMM_PROG:
                continue
            for a in (ix.get("accounts") or []):
                if isinstance(a, str):
                    pk = a
                elif isinstance(a, int) and 0 <= a < len(keys):
                    k = keys[a]
                    pk = k.get("pubkey") if isinstance(k, dict) else k
                else:
                    continue
                if not pk or pk in KNOWN_PROGRAMS:
                    continue
                info = meta.get(pk) or {}
                if info.get("signer"):
                    continue
                if info and not info.get("writable"):
                    continue
                return pk
    except Exception:
        pass
    return None


class Limiter:
    """Global request-rate limiter shared by all workers."""

    def __init__(self, rps: float):
        self.interval = 1.0 / max(rps, 0.1)
        self.lock = threading.Lock()
        self.next_at = time.time()

    def wait(self):
        with self.lock:
            now = time.time()
            if self.next_at > now:
                time.sleep(self.next_at - now)
                now = time.time()
            self.next_at = max(self.next_at + self.interval, now)
        return


def post_one(url, payload, timeout=60):
    body = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def run(workers=12, batch=50, rps=200.0, limit=0, outdir=OUTDIR, max_retries=5):
    key = load_key()
    url = f"https://mainnet.helius-rpc.com/?api-key={key}"
    print(json.dumps({"key_fingerprint_sha256_8": key_fingerprint(key),
                      "workers": workers, "batch": batch, "rps": rps}), flush=True)
    idx = build_index(outdir=outdir)
    out = os.path.join(outdir, "amm_reserves.jsonl")
    done = set()
    if os.path.isfile(out):
        for line in open(out, encoding="utf-8"):
            try:
                done.add(json.loads(line)["signature"])
            except Exception:
                continue
    pending = []
    for line in open(idx, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r["signature"] not in done:
            pending.append(r)
    if limit:
        pending = pending[:limit]
    print(json.dumps({"already_done": len(done), "to_fetch": len(pending)}), flush=True)
    if not pending:
        print(json.dumps({"status": "nothing_to_do"}), flush=True)
        return 0

    batches = [pending[i:i + batch] for i in range(0, len(pending), batch)]
    lim = Limiter(rps)
    wlock = threading.Lock()
    fh = open(out, "a", encoding="utf-8")
    fails = open(os.path.join(outdir, "retryable_failures.jsonl"), "a",
                 encoding="utf-8")
    st = collections.Counter()
    st["batches"] = len(batches)
    t0 = time.time()

    def work(bi, chunk):
        payload = [{"jsonrpc": "2.0", "id": j, "method": "getTransaction",
                    "params": [c["signature"],
                               {"encoding": "jsonParsed",
                                "maxSupportedTransactionVersion": 0}]}
                   for j, c in enumerate(chunk)]
        resp = None
        for attempt in range(max_retries + 1):
            lim.wait()
            try:
                resp = post_one(url, payload)
                break
            except urllib.error.HTTPError as e:
                if e.code in (429, 503, 502, 504) and attempt < max_retries:
                    time.sleep(min(30.0, 1.5 ** attempt) + random.random())
                    with wlock:
                        st["backs"] += 1
                    continue
                with wlock:
                    st["hard_errors"] += 1
                    for c in chunk:
                        fails.write(json.dumps({"signature": c["signature"],
                                                "reason": f"http {e.code}"}) + "\n")
                return
            except Exception:
                if attempt < max_retries:
                    time.sleep(min(30.0, 1.5 ** attempt) + random.random())
                    with wlock:
                        st["backs"] += 1
                    continue
                with wlock:
                    st["hard_errors"] += 1
                    for c in chunk:
                        fails.write(json.dumps({"signature": c["signature"],
                                                "reason": "exception"}) + "\n")
                return
        if resp is None:
            return                      # every attempt errored; batch is not written
        if isinstance(resp, dict):
            resp = [resp]
        by_id = {r.get("id"): r for r in resp if isinstance(r, dict)}
        lines = []
        dec = 0
        for j, c in enumerate(chunk):
            res = (by_id.get(j) or {}).get("result")
            row = {"signature": c["signature"], "mint": c["mint"],
                   "recv_unix_ms": c["recv_unix_ms"], "slot": None, "pool": None,
                   "event": None, "base_reserve": None, "quote_reserve": None}
            if res:
                row["slot"] = res.get("slot")
                row["pool"] = pool_of(res)
                ev = decode_event((res.get("meta") or {}).get("logMessages"))
                if ev:
                    row.update(ev)
                    dec += 1
            lines.append(json.dumps(row))
        with wlock:
            fh.write("\n".join(lines) + "\n")
            st["fetched"] += len(chunk)
            st["decoded"] += dec
            st["batches_done"] += 1
            if st["batches_done"] % 40 == 0:
                fh.flush()
                el = time.time() - t0
                rate = st["fetched"] / el if el else 0
                print(json.dumps({
                    "fetched": st["fetched"], "decoded": st["decoded"],
                    "pct": round(100.0 * st["batches_done"] / st["batches"], 2),
                    "sig_per_s": round(rate, 1), "backs": st["backs"],
                    "hard_errors": st["hard_errors"],
                    "eta_h": round((len(pending) - st["fetched"]) / rate / 3600, 2)
                    if rate else None}), flush=True)

    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(max_workers=workers) as ex:
        list(ex.map(lambda a: work(*a), enumerate(batches)))
    fh.flush()
    fh.close()
    fails.close()
    el = time.time() - t0
    man = {"schema": "amm_history_v1_fast", "out": out,
           "fetched_this_run": st["fetched"], "decoded_events": st["decoded"],
           "backs": st["backs"], "hard_errors": st["hard_errors"],
           "elapsed_s": round(el, 1),
           "sig_per_s": round(st["fetched"] / el, 1) if el else None,
           "key_fingerprint_sha256_8": key_fingerprint(key)}
    with open(os.path.join(outdir, "MANIFEST_FAST.json"), "w", encoding="utf-8") as mf:
        json.dump(man, mf, indent=2)
    print(json.dumps(man, indent=1), flush=True)
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--workers", type=int, default=12)
    ap.add_argument("--batch", type=int, default=50)
    ap.add_argument("--rps", type=float, default=200.0)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--outdir", default=OUTDIR)
    a = ap.parse_args(argv)
    return run(workers=a.workers, batch=a.batch, rps=a.rps, limit=a.limit,
               outdir=a.outdir)


if __name__ == "__main__":
    sys.exit(main())
