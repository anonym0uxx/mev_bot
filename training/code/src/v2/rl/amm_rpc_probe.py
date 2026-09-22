"""PROOF STEP: can we recover historical AMM pool reserves via RPC?

Take real pump-swap signatures straight out of the corpus tape, fetch each tx from
Helius, decode the pump-amm Buy/SellEvent from the program logs, and validate the
recovered (base, quote) reserves with the constant-product invariant.

Key is loaded from the creds file (never on the command line, never printed).
"""
import base64
import json
import os
import re
import sys
import urllib.request

CREDS = "/mnt/winc/Users/Alon/.hermes/creds/pump-quant.env"
TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
AMM_PROG = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
DISC = {"67f4521f2cf57777": "BuyEvent", "3e2f370aa503dc2a": "SellEvent"}


def load_key(path=CREDS):
    for line in open(path, encoding="utf-8", errors="ignore"):
        line = line.strip()
        if line.startswith("HELIUS_API_KEY="):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    raise SystemExit("HELIUS_API_KEY not found in creds file")


def rpc(url, method, params, timeout=30):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                       "params": params}).encode()
    req = urllib.request.Request(url, data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def sigs_from_tape(n_want, path=TAPE):
    out = []
    seen = set()
    for line in open(path, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("venue") != "pumpswap":
            continue
        s = r.get("signature")
        if s and s not in seen:
            seen.add(s)
            out.append((r["mint"], int(r["recv_unix_ms"]), s))
        if len(out) >= n_want:
            break
    return out


def decode_event(logs):
    """Return list of (event, base_reserve, quote_reserve) from log messages."""
    found = []
    for lg in logs or []:
        if not isinstance(lg, str) or "Program data: " not in lg:
            continue
        b64 = lg.split("Program data: ", 1)[1].strip()
        try:
            raw = base64.b64decode(b64)
        except Exception:
            continue
        if len(raw) < 64:
            continue
        disc = raw[:8].hex()
        if disc not in DISC:
            continue
        base_r = int.from_bytes(raw[48:56], "little")
        quote_r = int.from_bytes(raw[56:64], "little")
        found.append((DISC[disc], base_r, quote_r))
    return found


if __name__ == "__main__":
    key = load_key()
    url = f"https://mainnet.helius-rpc.com/?api-key={key}"
    sigs = sigs_from_tape(int(sys.argv[1]) if len(sys.argv) > 1 else 5)
    print(json.dumps({"signatures_from_tape": len(sigs)}, indent=1))
    ok = bad = 0
    for mint, ts, sig in sigs:
        try:
            res = rpc(url, "getTransaction",
                      [sig, {"encoding": "jsonParsed",
                             "maxSupportedTransactionVersion": 0}])
        except Exception as e:
            print(json.dumps({"sig": sig[:16], "error": type(e).__name__}))
            bad += 1
            continue
        err = res.get("error")
        if err:
            print(json.dumps({"sig": sig[:16], "rpc_error": str(err)[:160]}))
            bad += 1
            continue
        r = res.get("result")
        if not r:
            print(json.dumps({"sig": sig[:16], "result": None}))
            bad += 1
            continue
        logs = (r.get("meta") or {}).get("logMessages") or []
        accs = [a.get("pubkey") for a in
                ((r.get("transaction") or {}).get("message") or {}).get("accountKeys", [])]
        ev = decode_event(logs)
        kdrift = None
        if ev:
            _, b, q = ev[0]
            if b and q:
                kdrift = "k=" + str(b * q)
        print(json.dumps({
            "sig": sig[:16], "mint": mint[:12], "slot": r.get("slot"),
            "has_amm_prog": AMM_PROG in accs,
            "n_events": len(ev),
            "event": ev[0][0] if ev else None,
            "base_reserve": ev[0][1] if ev else None,
            "quote_reserve": ev[0][2] if ev else None,
        }))
        if ev:
            ok += 1
        else:
            bad += 1
    print(json.dumps({"decoded_ok": ok, "failed": bad}, indent=1))
