#!/usr/bin/env python3
"""Find the sell venue for RCAP by examining recent transactions on the mint.
Also derive and query the pump.fun curve PDA directly.
"""
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

def b58_encode(data):
    num = int.from_bytes(data, 'big')
    res = []
    while num > 0:
        num, r = divmod(num, 58)
        res.append(B58[r])
    pad = 0
    for b in data:
        if b == 0: pad += 1
        else: break
    return '1' * pad + ''.join(reversed(res))

# Step 1: Get recent signatures for the RCAP mint
print("=== Recent transactions for RCAP mint ===")
result = rpc_call("getSignaturesForAddress", [RCAP_MINT, {"limit": 5}])
if "result" in result and result["result"]:
    sigs = result["result"]
    print(f"  Found {len(sigs)} recent signatures")
    for s in sigs[:3]:
        print(f"  sig={s.get('signature','?')[:40]}... slot={s.get('slot','?')} err={s.get('err','none')}")
    
    # Get the first transaction to find curve account
    if sigs:
        sig = sigs[0]["signature"]
        print(f"\n=== Decoding recent tx: {sig[:40]}... ===")
        tx_result = rpc_call("getTransaction", [sig, {"encoding": "json", "maxSupportedTransactionVersion": 0}])
        if "result" in tx_result and tx_result["result"]:
            tx = tx_result["result"]
            msg = tx.get("transaction", {}).get("message", {})
            account_keys = msg.get("accountKeys", [])
            print(f"  Account keys ({len(account_keys)}):")
            pump_fun_accounts = []
            for i, key in enumerate(account_keys):
                is_signer = msg.get("accountKeys", [])[i] if isinstance(msg.get("accountKeys", [])[i], str) else key
                print(f"    [{i}] {key}")
            
            # Check instructions for pump.fun program
            instructions = msg.get("instructions", [])
            print(f"\n  Instructions ({len(instructions)}):")
            for inst in instructions:
                prog_idx = inst.get("programIdIndex", -1)
                prog_id = account_keys[prog_idx] if prog_idx < len(account_keys) else "?"
                data_b64 = inst.get("data", "")
                if data_b64:
                    data_bytes = base64.b64decode(data_b64)
                    # Check if it's a pump.fun sell instruction
                    # sell discriminator = SHA256("global:sell")[:8]
                    sell_disc = hashlib.sha256(b"global:sell").digest()[:8]
                    buy_disc = hashlib.sha256(b"global:buy").digest()[:8]
                    create_disc = hashlib.sha256(b"global:create").digest()[:8]
                    
                    if data_bytes[:8] == sell_disc:
                        print(f"    PUMP.FUN SELL instruction! program={prog_id}")
                    elif data_bytes[:8] == buy_disc:
                        print(f"    PUMP.FUN BUY instruction! program={prog_id}")
                    elif data_bytes[:8] == create_disc:
                        print(f"    PUMP.FUN CREATE instruction! program={prog_id}")
                    else:
                        print(f"    Unknown instruction, program={prog_id}, disc={data_bytes[:8].hex()}")
                else:
                    print(f"    Empty instruction, program={prog_id}")
            
            # Look for the curve account in account_keys
            # It should be a PDA derived from the mint + "curve" seed
            # Check which accounts are owned by the pump.fun program
            print(f"\n  === Looking for curve account ===")
            # Get account info for each non-signer account to check owner
            for i, key in enumerate(account_keys):
                if i <= 1: continue  # skip signer + sysvar
                # Check if this account is owned by pump.fun program
                acc_info = rpc_call("getAccountInfo", [key, {"encoding": "base64", "commitment": "confirmed"}])
                if "result" in acc_info and acc_info["result"]["value"]:
                    owner = acc_info["result"]["value"]["owner"]
                    data_b64 = acc_info["result"]["value"]["data"][0]
                    if owner == PUMP_FUN:
                        raw = base64.b64decode(data_b64)
                        print(f"    [{i}] {key} - OWNED BY PUMP.FUN (size={len(raw)})")
                        print(f"      hex: {raw[:80].hex()}")
                        # Check if mint is at offset 8 or 9
                        mint_bytes = b58_decode(RCAP_MINT)
                        mint_at_8 = raw[8:40] if len(raw) > 40 else b''
                        mint_at_9 = raw[9:41] if len(raw) > 41 else b''
                        if mint_at_8 == mint_bytes:
                            print(f"      ✅ MINT MATCHES at offset 8!")
                            # Parse curve state
                            v_sol = struct.unpack('<Q', raw[40:48])[0]
                            v_tok = struct.unpack('<Q', raw[48:56])[0]
                            r_sol = struct.unpack('<Q', raw[56:64])[0]
                            r_tok = struct.unpack('<Q', raw[64:72])[0]
                            complete = raw[72] if len(raw) > 72 else 0
                            print(f"      virtual_sol: {v_sol/1e9:.6f} SOL ({v_sol} lamports)")
                            print(f"      virtual_token: {v_tok/1e6:.6f} tokens ({v_tok} raw)")
                            print(f"      real_sol: {r_sol/1e9:.6f} SOL ({r_sol} lamports)")
                            print(f"      real_token: {r_tok/1e6:.6f} tokens ({r_tok} raw)")
                            print(f"      complete: {bool(complete)}")
                            
                            if not complete:
                                sell = 220371635503
                                sol_out = (v_sol * sell) / (v_tok + sell)
                                print(f"\n      === SELL QUOTE ===")
                                print(f"      sell {sell/1e6:.6f} RCAP")
                                print(f"      sol_out (no fee): {sol_out/1e9:.6f} SOL ({int(sol_out):,} lamports)")
                        elif mint_at_9 == mint_bytes:
                            print(f"      ✅ MINT MATCHES at offset 9!")
                            v_sol = struct.unpack('<Q', raw[41:49])[0]
                            v_tok = struct.unpack('<Q', raw[49:57])[0]
                            r_sol = struct.unpack('<Q', raw[57:65])[0]
                            r_tok = struct.unpack('<Q', raw[65:73])[0]
                            complete = raw[73] if len(raw) > 73 else 0
                            print(f"      virtual_sol: {v_sol/1e9:.6f} SOL")
                            print(f"      virtual_token: {v_tok/1e6:.6f} tokens")
                            print(f"      real_sol: {r_sol/1e9:.6f} SOL")
                            print(f"      real_token: {r_tok/1e6:.6f} tokens")
                            print(f"      complete: {bool(complete)}")
                break  # only check first few
        elif "error" in tx_result:
            print(f"  Error: {tx_result['error']}")
else:
    print("  No recent transactions found for this mint")
    if "error" in result:
        print(f"  Error: {result['error']}")
