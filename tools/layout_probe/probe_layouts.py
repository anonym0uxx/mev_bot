#!/usr/bin/env python3
"""Deeper pump.fun layout probe: paginate, collect the distinct (ix, count,
token program) combos with a REAL verifying signature + slot for each.

Emits /tmp/layout_matrix.json — the evidence the §41 registry is built from.
"""
import base64, hashlib, json, sys, time, urllib.request

key = None
for l in open("/mnt/data/tmp/pump-quant.env"):
    if l.startswith("HELIUS_API_KEY="):
        key = l.split("=", 1)[1].strip()
RPC = f"https://mainnet.helius-rpc.com/?api-key={key}"
PUMP = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
TOKEN_SPL = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
PAGES, PER = 4, 100
B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58d(s):
    n = 0
    for ch in s:
        n = n * 58 + B58.index(ch)
    out = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    return b"\x00" * (len(s) - len(s.lstrip("1"))) + out


def rpc(method, params, tries=5):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    last = None
    for i in range(tries):
        try:
            req = urllib.request.Request(RPC, data=body, headers={"Content-Type": "application/json"})
            r = json.loads(urllib.request.urlopen(req, timeout=45).read())
            if r.get("error"):
                last = r["error"]
            else:
                return r.get("result")
        except Exception as e:  # noqa: BLE001
            last = e
        time.sleep(1.2 * (i + 1))
    print(f"  ! {method}: {last}", file=sys.stderr)
    return None


NAMES = ["buy", "sell", "buy_v2", "sell_v2", "buy_exact_sol_in", "sell_exact_sol_in"]
D = {hashlib.sha256(f"global:{n}".encode()).digest()[:8]: n for n in NAMES}

matrix, stat = {}, {"tx": 0, "fail": 0, "pump_ix": 0, "matched": 0, "unmatched": {}}
before = None
for page in range(PAGES):
    params = [PUMP, {"limit": PER, "commitment": "confirmed"}]
    if before:
        params[1]["before"] = before
    sigs = rpc("getSignaturesForAddress", params) or []
    print(f"page {page}: {len(sigs)} sigs", file=sys.stderr)
    if not sigs:
        break
    before = sigs[-1]["signature"]
    for s in sigs:
        sg = s["signature"]
        tx = rpc("getTransaction", [sg, {"maxSupportedTransactionVersion": 1,
                                         "encoding": "json", "commitment": "confirmed"}])
        if not tx:
            stat["fail"] += 1
            continue
        stat["tx"] += 1
        msg = tx["transaction"]["message"]
        static = [k["pubkey"] if isinstance(k, dict) else k for k in msg["accountKeys"]]
        la = (tx.get("meta", {}) or {}).get("loadedAddresses") or {}
        keys = static + list(la.get("writable") or []) + list(la.get("readonly") or [])
        ixs = list(msg.get("instructions", []))
        for g in (tx.get("meta", {}).get("innerInstructions") or []):
            ixs.extend(g.get("instructions", []))
        for ix in ixs:
            pidx = ix.get("programIdIndex")
            pid = ix.get("programId")
            if pid is None and isinstance(pidx, int) and 0 <= pidx < len(keys):
                pid = keys[pidx]
            if pid != PUMP:
                continue
            stat["pump_ix"] += 1
            raw = b""
            try:
                raw = base64.b64decode(ix["data"]) if ix.get("encoding") == "base64" else b58d(ix.get("data", ""))
            except Exception:  # noqa: BLE001
                continue
            if len(raw) < 8:
                continue
            name = D.get(raw[:8])
            if not name:
                h = raw[:8].hex()
                stat["unmatched"][h] = stat["unmatched"].get(h, 0) + 1
                continue
            stat["matched"] += 1
            accs = ix.get("accounts", []) or []
            progs = {keys[a] for a in accs if isinstance(a, int) and 0 <= a < len(keys)}
            tok = "2022" if TOKEN_2022 in progs else ("spl" if TOKEN_SPL in progs else "?")
            k = f"{name}|{len(accs)}|{tok}"
            m = matrix.setdefault(k, {"ix": name, "n": len(accs), "token": tok,
                                      "count": 0, "example_sig": sg, "example_slot": tx.get("slot")})
            m["count"] += 1

matrix = dict(sorted(matrix.items()))
json.dump({"stats": stat, "matrix": matrix}, open("/tmp/layout_matrix.json", "w"), indent=1)
print(json.dumps({"stats": stat, "matrix": matrix}, indent=1)[:4500])