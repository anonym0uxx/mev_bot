"""Validate the pump.fun TradeEvent layout at scale.

Invariants that must hold for EVERY decoded event (they are structural, not
statistical):
    virtual_sol - real_sol == 30_000_000_000        (VIRTUAL_SOL_OFFSET)
    virtual_tok - real_tok == 279_900_000_000_000   (VIRTUAL_TOK_OFFSET)
    is_buy agrees with the tape's `side` for the same signature
"""
import base64
import collections
import json
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
ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


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
        mint = b58(raw[8:40])
        sol_amt, tok_amt = struct.unpack_from("<QQ", raw, 40)
        is_buy = bool(raw[56])
        vsol, vtok, rsol, rtok = struct.unpack_from("<QQQQ", raw, 97)
        return {"mint": mint, "sol_amount": sol_amt, "token_amount": tok_amt,
                "is_buy": is_buy, "virtual_sol": vsol, "virtual_token": vtok,
                "real_sol": rsol, "real_token": rtok}
    return None


MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
SIG = re.compile(r'"signature"\s*:\s*"([^"]+)"')
SIDE = re.compile(r'"side"\s*:\s*"(\w+)"')
VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')

want = set(open("/training/v2/reports/WALL_MINTS_475.txt").read().split())
probe = []
for line in open(TAPE, encoding="utf-8"):
    m = MINT.search(line)
    if not m or m.group(1) not in want:
        continue
    v, s, sd = VEN.search(line), SIG.search(line), SIDE.search(line)
    if v and v.group(1) == "pumpfun" and s and sd:
        probe.append((m.group(1), s.group(1), sd.group(1)))
        if len(probe) >= 60:
            break
print("probe signatures:", len(probe))

key = load_key()
print("key fp:", key_fingerprint(key))
url = "https://mainnet.helius-rpc.com/?api-key=" + key
lock = threading.Lock()
st = collections.Counter()
recs = []
payloads = []


def fetch(chunk):
    body = json.dumps([{"jsonrpc": "2.0", "id": j, "method": "getTransaction",
                        "params": [c[1], {"encoding": "jsonParsed",
                                          "maxSupportedTransactionVersion": 0}]}
                       for j, c in enumerate(chunk)]).encode()
    for attempt in range(5):
        try:
            req = urllib.request.Request(url, data=body,
                                         headers={"Content-Type": "application/json"})
            return json.loads(urllib.request.urlopen(req, timeout=90).read())
        except urllib.error.HTTPError as e:
            if e.code in (429, 503, 502, 504) and attempt < 4:
                time.sleep(1.5 ** attempt)
                continue
            raise
    return None


for i in range(0, len(probe), 20):
    chunk = probe[i:i + 20]
    out = fetch(chunk)
    if not out:
        st["batch_failed"] += 1
        continue
    for j, item in enumerate(out if isinstance(out, list) else [out]):
        res = item.get("result")
        if not res:
            st["no_result"] += 1
            continue
        ev = decode_event((res.get("meta") or {}).get("logMessages"))
        if not ev:
            st["no_event_decoded"] += 1
            continue
        exp_mint, exp_sig, exp_side = chunk[j]
        ok_mint = ev["mint"] == exp_mint
        ok_off = (ev["virtual_sol"] - ev["real_sol"] == VSOL_OFF
                  and ev["virtual_token"] - ev["real_token"] == VTOK_OFF)
        ok_side = ev["is_buy"] == (exp_side == "buy")
        st["decoded"] += 1
        st["mint_match"] += int(ok_mint)
        st["offset_invariant"] += int(ok_off)
        st["side_match"] += int(ok_side)
        if ok_mint and ok_off:
            recs.append(ev)

print(json.dumps(dict(st), indent=1))
print(json.dumps({"sample": recs[:2]}, indent=1))
n = max(st["decoded"], 1)
print("VERDICT: %s (mint %.1f%%, offsets %.1f%%, side %.1f%%)"
      % ("PASS" if st["mint_match"] == st["decoded"] and st["offset_invariant"] == st["decoded"]
         and st["side_match"] == st["decoded"] else "FAIL",
         100.0 * st["mint_match"] / n, 100.0 * st["offset_invariant"] / n,
         100.0 * st["side_match"] / n))