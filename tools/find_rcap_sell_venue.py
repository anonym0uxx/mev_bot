#!/usr/bin/env python3
"""Decode recent RCAP transactions to find the sell venue (curve or PumpSwap pool)
and the exact accounts needed. Also get a sell quote.
"""
import json, os, urllib.request, base64, hashlib, struct, sys

HELIUS_KEY = os.environ.get("HELIUS_API_KEY", "")
if not HELIUS_KEY:
    creds = "/mnt/c/Users/Alon/.hermes/creds/pump-quant.env"
    with open(creds) as f:
        for line in f:
            k, _, v = line.strip().partition("=")
            if k == "HELIUS_API_KEY": HELIUS_KEY = v
            if k == "WALLET_ADDRESS": WALLET = v

RPC = f"https://mainnet.helius-rpc.com/?api-key={HELIUS_KEY}"
RCAP_MINT = "7ahVBvGoTaKYpPqz9G3SwvrV7VibJGCtDUFNP4hKpump"
PUMP_FUN = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMP_SWAP = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
WALLET = "7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC"

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58_decode(s):
    num = 0
    for c in s: num = num * 58 + B58.index(c)
    result = num.to_bytes(32, 'big')
    pad = 0
    for c in s:
        if c == '1': pad += 1
        else: break
    return b'\x00' * pad + result

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

def get_account_info(pubkey):
    result = rpc_call("getAccountInfo", [pubkey, {"encoding": "base64", "commitment": "confirmed"}])
    if "result" in result and result["result"]["value"]:
        return result["result"]["value"]
    return None

def fix_b64(s):
    """Fix base64 padding."""
    s = s + "=" * (4 - len(s) % 4) if len(s) % 4 != 0 else s
    return s

# Get recent transactions
print("=== Recent RCAP transactions ===")
result = rpc_call("getSignaturesForAddress", [RCAP_MINT, {"limit": 10}])
if "result" not in result or not result["result"]:
    print("No transactions found")
    sys.exit(1)

sigs = result["result"]
print(f"Found {len(sigs)} recent transactions:\n")

mint_bytes = b58_decode(RCAP_MINT)
sell_disc = hashlib.sha256(b"global:sell").digest()[:8]
buy_disc = hashlib.sha256(b"global:buy").digest()[:8]
create_disc = hashlib.sha256(b"global:create").digest()[:8]

# Check each transaction
for idx, s in enumerate(sigs):
    sig = s["signature"]
    slot = s.get("slot", "?")
    err = s.get("err", None)
    status = "FAILED" if err else "SUCCESS"
    print(f"[{idx}] slot={slot} status={status} sig={sig[:50]}...")
    
    tx_result = rpc_call("getTransaction", [sig, {"encoding": "json", "maxSupportedTransactionVersion": 0}])
    if "result" not in tx_result:
        print(f"    Error fetching tx")
        continue
    
    tx = tx_result["result"]
    meta = tx.get("meta", {})
    msg = tx.get("transaction", {}).get("message", {})
    account_keys = msg.get("accountKeys", [])
    
    # Collect all instructions (top-level + inner)
    all_ixs = []
    for inst in msg.get("instructions", []):
        all_ixs.append(("top", inst))
    for ii in meta.get("innerInstructions", []):
        for inst in ii.get("instructions", []):
            all_ixs.append(("inner", inst))
    
    # Find pump.fun sell/buy instructions
    for label, inst in all_ixs:
        prog_idx = inst.get("programIdIndex", -1)
        if prog_idx >= len(account_keys): continue
        prog_id = account_keys[prog_idx]
        data_b64 = inst.get("data", "")
        if not data_b64: continue
        
        try:
            data_bytes = base64.b64decode(fix_b64(data_b64))
        except:
            continue
        
        disc = data_bytes[:8] if len(data_bytes) >= 8 else b''
        
        if prog_id == PUMP_FUN:
            if disc == sell_disc:
                print(f"    🔴 PUMP.FUN SELL ({label})")
                # Decode sell args: discriminator(8) + tokens(8) + min_sol(8) + ...
                if len(data_bytes) >= 24:
                    tokens_in = struct.unpack('<Q', data_bytes[8:16])[0]
                    min_sol = struct.unpack('<Q', data_bytes[16:24])[0]
                    print(f"        tokens_in: {tokens_in / 1e6:.6f} ({tokens_in:,} raw)")
                    print(f"        min_sol_out: {min_sol / 1e9:.6f} SOL ({min_sol:,} lamports)")
            elif disc == buy_disc:
                print(f"    🟢 PUMP.FUN BUY ({label})")
                if len(data_bytes) >= 24:
                    max_sol = struct.unpack('<Q', data_bytes[8:16])[0]
                    min_tokens = struct.unpack('<Q', data_bytes[16:24])[0]
                    print(f"        max_sol_in: {max_sol / 1e9:.6f} SOL")
                    print(f"        min_tokens_out: {min_tokens / 1e6:.6f}")
            elif disc == create_disc:
                print(f"    ✨ PUMP.FUN CREATE ({label})")
            else:
                print(f"    ❓ PUMP.FUN other (disc={disc.hex()})")
        elif prog_id == PUMP_SWAP:
            print(f"    ⚡ PUMPSWAP instruction ({label}) disc={disc.hex()}")
        elif prog_id.startswith("ComputeBudget"):
            pass  # skip compute budget
        else:
            # Check if it's a known DEX
            if disc:
                print(f"    ❓ Unknown program={prog_id[:20]}... disc={disc.hex()}")
    
    # Find pump.fun-owned accounts in the tx (the curve account)
    if idx < 3:  # only for first 3 txs
        for i, key in enumerate(account_keys):
            if i == 0: continue  # skip signer
            acc = get_account_info(key)
            if not acc: continue
            owner = acc["owner"]
            if owner == PUMP_FUN:
                raw = base64.b64decode(fix_b64(acc["data"][0]))
                has_mint = len(raw) > 40 and raw[8:40] == mint_bytes
                print(f"    [{i}] {key} PUMP.FUN-owned size={len(raw)} {'<-- HAS RCAP MINT' if has_mint else ''}")
                if has_mint:
                    v_sol = struct.unpack('<Q', raw[40:48])[0]
                    v_tok = struct.unpack('<Q', raw[48:56])[0]
                    r_sol = struct.unpack('<Q', raw[56:64])[0]
                    r_tok = struct.unpack('<Q', raw[64:72])[0]
                    complete = raw[72] if len(raw) > 72 else 0
                    print(f"        virtual_sol={v_sol/1e9:.4f} v_tok={v_tok/1e6:.4f} r_sol={r_sol/1e9:.4f} r_tok={r_tok/1e6:.4f} complete={bool(complete)}")
                    if not complete:
                        sell = 220371635503
                        sol_out = (v_sol * sell) / (v_tok + sell)
                        print(f"        SELL QUOTE: {sol_out/1e9:.6f} SOL for {sell/1e6:.6f} RCAP")
            elif owner == PUMP_SWAP:
                raw = base64.b64decode(fix_b64(acc["data"][0]))
                print(f"    [{i}] {key} PUMPSWAP-owned size={len(raw)}")
    print()
