#!/usr/bin/env python3
"""Find RCAP curve account by checking ALL recent transactions + derive PDA."""
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
TOKEN2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"

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

# Derive the pump.fun bonding curve PDA for this mint
# pump.fun uses seeds: [mint_bytes, b"curve"] with pump.fun program
# Actually, pump.fun uses: seeds = [&mint, b"curve"] - but let me also try [b"curve", &mint]
# The standard pump.fun derivation: seeds = [mint_pubkey, "curve"] with bump

mint_bytes = b58_decode(RCAP_MINT)
pump_bytes = b58_decode(PUMP_FUN)

print(f"RCAP mint bytes: {mint_bytes.hex()}")
print(f"Pump.fun program bytes: {pump_bytes.hex()}")
print()

# Try deriving PDA with seeds [mint, b"curve"]
# Solana PDA: hash(seeds + bump_seed + program_id) and check off-curve
# The correct algorithm:
# for bump in 255..0:
#   hash = sha256(seeds + [bump] + program_id)
#   if hash is NOT on the ed25519 curve: return (pda, bump)
# We need the ed25519 curve check, which is complex.
# Instead, let's derive using a known method.

# Actually, for pump.fun the curve PDA seeds are known: [mint_pubkey, b"curve"]
# Let me just try querying with getProgramAccounts using the dataSize + memcmp approach
# but this time search for the mint at the correct offset

# First, let me check what the pump.fun bonding curve discriminator is
# pump.fun is an Anchor program. The bonding curve account discriminator is:
# SHA256("account:BondingCurve")[:8] or similar Anchor account discriminator
# Actually for pump.fun: the discriminator for the curve account
# In Anchor, account discriminator = SHA256("<module>:<Account>")[:8]
# But pump.fun might use a different convention

# Let me check multiple transactions to find a pump.fun-owned account
print("=== Checking last 10 transactions for RCAP ===")
result = rpc_call("getSignaturesForAddress", [RCAP_MINT, {"limit": 10}])
if "result" in result and result["result"]:
    sigs = result["result"]
    print(f"Found {len(sigs)} signatures")
    
    for idx, s in enumerate(sigs):
        sig = s["signature"]
        slot = s.get("slot", "?")
        err = s.get("err", None)
        memo = s.get("memo", "")
        print(f"\n  [{idx}] sig={sig[:40]}... slot={slot} err={err}")
        
        tx_result = rpc_call("getTransaction", [sig, {"encoding": "json", "maxSupportedTransactionVersion": 0}])
        if "result" not in tx_result:
            continue
        tx = tx_result["result"]
        meta = tx.get("meta", {})
        msg = tx.get("transaction", {}).get("message", {})
        account_keys = msg.get("accountKeys", [])
        
        # Find accounts owned by pump.fun program
        for i, key in enumerate(account_keys):
            acc_info = rpc_call("getAccountInfo", [key, {"encoding": "base64", "commitment": "confirmed"}])
            if "result" in acc_info and acc_info["result"]["value"]:
                owner = acc_info["result"]["value"]["owner"]
                if owner == PUMP_FUN:
                    data_b64 = acc_info["result"]["value"]["data"][0]
                    raw = base64.b64decode(data_b64)
                    print(f"    [{i}] {key} OWNED BY PUMP.FUN (size={len(raw)})")
                    # Check if RCAP mint is in this account
                    if len(raw) > 40:
                        mint_at_8 = raw[8:40]
                        if mint_at_8 == mint_bytes:
                            print(f"      ✅ RCAP CURVE ACCOUNT FOUND!")
                            v_sol = struct.unpack('<Q', raw[40:48])[0]
                            v_tok = struct.unpack('<Q', raw[48:56])[0]
                            r_sol = struct.unpack('<Q', raw[56:64])[0]
                            r_tok = struct.unpack('<Q', raw[64:72])[0]
                            complete = raw[72] if len(raw) > 72 else 0
                            print(f"      virtual_sol: {v_sol/1e9:.6f} SOL ({v_sol:,} lamports)")
                            print(f"      virtual_token: {v_tok/1e6:.6f} tokens")
                            print(f"      real_sol: {r_sol/1e9:.6f} SOL")
                            print(f"      real_token: {r_tok/1e6:.6f} tokens")
                            print(f"      complete: {bool(complete)}")
                            
                            if not complete:
                                sell = 220371635503
                                sol_out = (v_sol * sell) / (v_tok + sell)
                                print(f"      SELL QUOTE: {sol_out/1e9:.6f} SOL for {sell/1e6:.6f} RCAP")
                            sys.exit(0)
                    elif len(raw) > 41:
                        mint_at_9 = raw[9:41]
                        if mint_at_9 == mint_bytes:
                            print(f"      ✅ RCAP CURVE ACCOUNT FOUND (offset 9)!")
                            sys.exit(0)
                elif owner == "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA":
                    print(f"    [{i}] {key} OWNED BY PUMPSWAP")
            break  # only check first non-signer account to save time
        
        if idx >= 2:  # only check first 3 transactions
            break
else:
    print("No transactions found")
