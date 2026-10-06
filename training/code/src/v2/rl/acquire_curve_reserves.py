"""acquire_curve_reserves — on-chain pump.fun curve reserves for the WALL window.

WHY THIS EXISTS
---------------
The forward wall (RL eval set) runs 09-09 13:04 -> 21:04. The curve capture stream
(s0909a/s0909b) ENDS 09-09 12:39, so 354 of the wall's 475 decision mints (74%,
all curve-venue / never-graduated) had no curve reserve state inside their own
evaluation window. Tape-based integration was tried and REFUTED: the tape is the
renormalized corpus-only tape (1,035 of 10,701 captured mints appear in it), and
even count-matched mints reproduced 0/131 states exactly. Integrating it would
have silently mis-priced the 75%.

This fetches the transactions by SIGNATURE (the tape already carries them) and
decodes pump.fun's TradeEvent directly - the same proven pattern as the AMM
backfill, but for the curve program.

DECODED LAYOUT (validated 100% on a 60-signature probe; see
curve_event_decode_check.py):
    disc bddb7fd34ee661ee | mint[8:40] | sol_amount@40 | token_amount@48
    is_buy@56 | virtual_sol@97 | virtual_token@105 | real_sol@113 | real_token@121
Structural invariants enforced on EVERY event before it is written:
    virtual_sol - real_sol == 30_000_000_000
    virtual_token - real_token == 279_900_000_000_000
    decoded mint == the mint the signature was indexed under
Append-only and resumable by signature, so an interrupted run costs nothing.
"""
from __future__ import annotations

import argparse
import base64
import collections
import json
import os
import random
import re
import struct
import sys
import threading
import time
import urllib.error
import urllib.request

sys.path.insert(0, "/training/v2/code/src/v2/rl")
from amm_history_acquire import key_fingerprint, load_key  # noqa: E402

DISC = bytes.fromhex("bddb7fd34ee661ee")
VSOL_OFF = 30_000_000_000
VTOK_OFF = 279_900_000_000_000
TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
OUTDIR = "/training/v2/reserves/curve_wall_v1"
MIN_TS = 1788968000000            # ~2 h before the wall: gives the oracle continuity
ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"

_MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
_SIG = re.compile(r'"signature"\s*:\s*"([^"]+)"')
_TS = re.compile(r'"recv_unix_ms"\s*:\s*(\d+)')
_SIDE = re.compile(r'"side"\s*:\s*"(\w+)"')
_VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')


def b58(b: bytes) -> str:
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = ALPHA[r] + s
    for c in b:
        if c:
            break
        s = "1" + s
    return s


def decode_event(logs):
    for lg in logs or []:
        if not isinstance(lg, str) or "Program data: " not in lg:
            continue
        try:
            raw = base64.b64decode(lg.split("Program data: ", 1)[1].strip())
        except Exception:
            continue
        if len(raw) < 129 or raw[:8] != DISC:
            continue
        vsol, vtok, rsol, rtok = struct.unpack_from("<QQQQ", raw, 97)
        if vsol - rsol != VSOL_OFF or vtok - rtok != VTOK_OFF:
            return {"__invariant__": True}
        return {"mint": b58(raw[8:40]),
                "sol_amount": struct.unpack_from("<Q", raw, 40)[0],
                "token_amount": struct.unpack_from("<Q", raw, 48)[0],
                "is_buy": bool(raw[56]),
                "virtual_sol": vsol, "virtual_token": vtok,
                "real_sol": rsol, "real_token": rtok}
    return None


def build_worklist(mints_file):
    want = set(open(mints_file, encoding="utf-8").read().split())
    out, seen = [], set()
    with open(TAPE, encoding="utf-8") as fh:
        for line in fh:
            m = _MINT.search(line)
            if not m or m.group(1) not in want:
                continue
            v = _VEN.search(line)
            if not v or v.group(1) != "pumpfun":
                continue
            s, t, sd = _SIG.search(line), _TS.search(line), _SIDE.search(line)
            if not (s and t and sd):
                continue
            sig = s.group(1)
            if sig in seen or int(t.group(1)) < MIN_TS:
                continue
            seen.add(sig)
            out.append({"signature": sig, "mint": m.group(1),
                        "recv_unix_ms": int(t.group(1)), "side": sd.group(1)})
    return out


class Limiter:
    def __init__(self, rps):
        self.gap = 1.0 / float(rps)
        self.lock = threading.Lock()
        self.next = 0.0

    def wait(self):
        with self.lock:
            now = time.time()
            if now < self.next:
                time.sleep(self.next - now)
                now = time.time()
            self.next = max(now, self.next) + self.gap


def run(mints_file, workers=12, batch=50, rps=200.0, limit=0, outdir=OUTDIR,
        max_retries=5):
    key = load_key()
    print(json.dumps({"key_fingerprint_sha256_8": key_fingerprint(key),
                      "workers": workers, "batch": batch, "rps": rps}), flush=True)
    url = "https://mainnet.helius-rpc.com/?api-key=" + key
    os.makedirs(outdir, exist_ok=True)
    out = os.path.join(outdir, "curve_reserves.jsonl")
    wl = build_worklist(mints_file)
    done = set()
    if os.path.isfile(out):
        with open(out, encoding="utf-8") as fh:
            for line in fh:
                try:
                    done.add(json.loads(line)["signature"])
                except Exception:
                    continue
    pending = [w for w in wl if w["signature"] not in done]
    if limit:
        pending = pending[:limit]
    print(json.dumps({"worklist": len(wl), "already_done": len(done),
                      "to_fetch": len(pending)}), flush=True)
    if not pending:
        print(json.dumps({"status": "nothing_to_do"}), flush=True)
        return 0

    batches = [pending[i:i + batch] for i in range(0, len(pending), batch)]
    lim, wlock, st = Limiter(rps), threading.Lock(), collections.Counter()
    st["batches"] = len(batches)
    fh = open(out, "a", encoding="utf-8")
    fails = open(os.path.join(outdir, "retryable_failures.jsonl"), "a", encoding="utf-8")
    t0 = time.time()

    def work(chunk):
        body = json.dumps([{"jsonrpc": "2.0", "id": j, "method": "getTransaction",
                            "params": [c["signature"],
                                       {"encoding": "jsonParsed",
                                        "maxSupportedTransactionVersion": 0}]}
                           for j, c in enumerate(chunk)]).encode()
        res = None
        for attempt in range(max_retries + 1):
            lim.wait()
            try:
                req = urllib.request.Request(
                    url, data=body, headers={"Content-Type": "application/json"})
                res = json.loads(urllib.request.urlopen(req, timeout=90).read())
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
                        fails.write(json.dumps({**c, "error": str(e)[:200]}) + "\n")
                return
            except Exception as e:                       # noqa: BLE001
                with wlock:
                    st["hard_errors"] += 1
                    for c in chunk:
                        fails.write(json.dumps({**c, "error": str(e)[:200]}) + "\n")
                return
        if res is None:
            return
        items = res if isinstance(res, list) else [res]
        lines = []
        for j, item in enumerate(items):
            c = chunk[j] if j < len(chunk) else None
            if c is None:
                continue
            r = item.get("result")
            if not r:
                with wlock:
                    st["no_result"] += 1
                    fails.write(json.dumps({**c, "error": "no_result"}) + "\n")
                continue
            ev = decode_event((r.get("meta") or {}).get("logMessages"))
            if not ev:
                with wlock:
                    st["no_event"] += 1
                    fails.write(json.dumps({**c, "error": "no_curve_event"}) + "\n")
                continue
            if ev.get("__invariant__"):
                with wlock:
                    st["invariant_violation"] += 1
                    fails.write(json.dumps({**c, "error": "invariant_violation"}) + "\n")
                continue
            if ev["mint"] != c["mint"]:
                with wlock:
                    st["mint_mismatch"] += 1
                    fails.write(json.dumps({**c, "error": "mint_mismatch",
                                            "decoded": ev["mint"]}) + "\n")
                continue
            if ev["is_buy"] != (c["side"] == "buy"):
                with wlock:
                    st["side_mismatch"] += 1
                    fails.write(json.dumps({**c, "error": "side_mismatch"}) + "\n")
                continue
            lines.append(json.dumps({**c, **ev, "slot": r.get("slot"),
                                     "source": "helius_curve_backfill"}))
            with wlock:
                st["written"] += 1
        if lines:
            with wlock:
                fh.write("\n".join(lines) + "\n")
                fh.flush()

    idx = 0
    while idx < len(batches):
        chunk_threads = []
        for _ in range(min(workers, len(batches) - idx)):
            th = threading.Thread(target=work, args=(batches[idx],))
            th.start()
            chunk_threads.append(th)
            idx += 1
        for th in chunk_threads:
            th.join()
        if (idx // max(workers, 1)) % 20 == 0:
            print(json.dumps({"progress": idx, "of": len(batches),
                              "written": st["written"],
                              "elapsed_s": round(time.time() - t0, 1)}), flush=True)
    fh.close()
    fails.close()
    st["elapsed_s"] = round(time.time() - t0, 1)
    st["out"] = out
    print("SUMMARY " + json.dumps(dict(st)), flush=True)
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--mints-file", default="/training/v2/reports/WALL_MINTS_475.txt")
    ap.add_argument("--workers", type=int, default=12)
    ap.add_argument("--batch", type=int, default=50)
    ap.add_argument("--rps", type=float, default=200.0)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--outdir", default=OUTDIR)
    a = ap.parse_args(argv)
    return run(a.mints_file, workers=a.workers, batch=a.batch, rps=a.rps,
               limit=a.limit, outdir=a.outdir)


if __name__ == "__main__":
    raise SystemExit(main())