#!/usr/bin/env python3
"""Find RCAP bonding curve account by decoding recent pump.fun transactions."""
import json, os, urllib.request, base64, hashlib, struct

HELIUS_KEY = os.environ.get("HELIUS_API_KEY", "")
if not HELIUS_KEY:
    creds = "/mnt/c/Users/Alon/.hermes/creds/pump-quant.env"
    with open(creds) as f:
        for line in f:
            if line.startswith("HELIUS_API_KEY="):
                HELIUS_KEY = line.strip().split("=",1)[1]

RPC = f"https://mainnet.helius-rpc.com/?api-key={HELIUS_KEY}"
RCAP_MINT = "7ahVBvGoTaKYpPqz9G3SwvrV7VibJGCtDUFNP4hKpump"
PUMP_FUN = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
WALLET = "7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC"

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58_decode(s):
    num = 0
    for c in s:
        num = num * 58 + B58.index(c)
    result = num.to_bytes(32, 'big')
    pad = 0
    for c in s:
        if c == '1': pad += 1
        else: break
    return b'\x00' * pad + result

# Get the most recent transaction and find the curve account
result = rpc_call("getSignaturesForAddress", [RCAP_MINT, {"limit": 3}])
if "result" in result and result["result"]:
    sig = result["result"][0]["signature"]
    print(f"Latest tx: {sig[:50]}...")
    tx_result = rpc_call("getTransaction", [sig, {"encoding": "json", "maxSupportedTransactionVersion": 0}])
    if "result" in tx_result and tx_result["result"]:
        tx = tx_result["result"]
        meta = tx.get("meta", {})
        msg = tx.get("transaction", {}).get("message", {})
        account_keys = msg.get("accountKeys", [])
        
        print(f"Account keys: {len(account_keys)}")
        print(f"TX status: {'FAILED' if meta.get('err') else 'SUCCESS'}")
        print()
        
        # Check inner instructions for pump.fun sell/buy
        inner = meta.get("innerInstructions", [])
        all_ixs = []
        # Top-level instructions
        for inst in msg.get("instructions", []):
            all_ixs.append(("top", inst))
        # Inner instructions
        for ii in inner:
            for inst in ii.get("instructions", []):
                all_ixs.append(("inner", inst))
        
        sell_disc = hashlib.sha256(b"global:sell").digest()[:8]
        buy_disc = hashlib.sha256(b"global:buy").digest()[:8]
        
        # Find pump.fun accounts in the transaction
        print("=== Pump.fun accounts in this tx ===")
        # Query account info for accounts that look like pump.fun PDAs (end in "pump")
        pump_accounts = []
        for i, key in enumerate(account_keys):
            if key.endswith("pump") or key == PUMP_FUN:
                pump_accounts.append((i, key))
        
        print(f"Pump.fun-related accounts: {len(pump_accounts)}")
        for i, key in pump_accounts:
            print(f"  [{i}] {key}")
        
        # Query each pump.fun PDA account to find the curve account
        print("\n=== Querying pump.fun accounts ===")
        for i, key in pump_accounts:
            if key == PUMP_FUN: continue
            acc_info = rpc_call("getAccountInfo", [key, {"encoding": "base64", "commitment": "confirmed"}])
            if "result" in acc_info and acc_info["result"]["value"]:
                owner = acc_info["result"]["value"]["owner"]
                data_b64 = acc_info["result"]["value"]["data"][0]
                raw = base64.b64decode(data_b64)
                print(f"\n  [{i}] {key}")
                print(f"    owner: {owner}")
                print(f"    size: {len(raw)} bytes")
                print(f"    hex: {raw[:80].hex()}")
                
                if owner == PUMP_FUN:
                    mint_bytes = b58_decode(RCAP_MINT)
                    # Check if RCAP mint is at offset 8
                    if len(raw) > 40 and raw[8:40] == mint_bytes:
                        print(f"    ✅ RCAP MINT at offset 8 — THIS IS THE CURVE ACCOUNT!")
                        v_sol = struct.unpack('<Q', raw[40:48])[0]
                        v_tok = struct.unpack('<Q', raw[48:56])[0]
                        r_sol = struct.unpack('<Q', raw[56:64])[0]
                        r_tok = struct.unpack('<Q', raw[64:72])[0]
                        complete = raw[72] if len(raw) > 72 else 0
                        print(f"    virtual_sol: {v_sol/1e9:.6f} SOL ({v_sol:,} lamports)")
                        print(f"    virtual_token: {v_tok/1e6:.6f} tokens ({v_tok:,} raw)")
                        print(f"    real_sol: {r_sol/1e9:.6f} SOL ({r_sol:,} lamports)")
                        print(f"    real_token: {r_tok/1e6:.6f} tokens ({r_tok:,} raw)")
                        print(f"    complete: {bool(complete)}")
                        
                        if not complete:
                            sell = 220371635503
                            sol_out = (v_sol * sell) / (v_tok + sell)
                            print(f"\n    === SELL QUOTE (220,371.6 RCAP) ===")
                            print(f"    sol_out (no fee):  {sol_out/1e9:.6f} SOL ({int(sol_out):,} lamports)")
                            # pump.fun sell fee is 0% (fee only on buy side)
                            print(f"    sol_out (0% fee):  {sol_out/1e9:.6f} SOL")
                    else:
                        print(f"    Different mint or non-curve account")
            else:
                print(f"  [{i}] {key} — account not found or empty")
