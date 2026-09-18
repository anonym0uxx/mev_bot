#!/usr/bin/env python3
"""Check if RCAP has migrated to PumpSwap AMM pool.
Also try alternate curve account sizes (pump.fun changed layout over time).
"""
import json, os, urllib.request, base64, struct

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
PUMP_SWAP = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

# Check pump.fun curve with ALL possible sizes
print("=== Checking pump.fun curve (all sizes) ===")
# The standard pump.fun bonding curve account is 212 bytes, but let me also try other sizes
for size in [212, 96, 104, 80, 74, 128, 144, 160, 200, 240, 100, 84, 90, 120, 180, 220]:
    result = rpc_call("getProgramAccounts", [
        PUMP_FUN,
        {
            "encoding": "base64",
            "commitment": "confirmed",
            "filters": [
                {"dataSize": size},
                {"memcmp": {"offset": 8, "bytes": RCAP_MINT}},
            ]
        }
    ])
    if "result" in result and result["result"] and len(result["result"]) > 0:
        print(f"  ✅ size={size}: {len(result['result'])} accounts")
        for acc in result["result"]:
            pubkey = acc.get("pubkey", "?")
            data_b64 = acc.get("account", {}).get("data", [None, None])[0]
            if data_b64:
                raw = base64.b64decode(data_b64)
                print(f"    pubkey={pubkey} len={len(raw)}")
                print(f"    hex: {raw[:100].hex()}")
                # Parse standard pump.fun curve
                v_sol = struct.unpack('<Q', raw[40:48])[0]
                v_tok = struct.unpack('<Q', raw[48:56])[0]
                r_sol = struct.unpack('<Q', raw[56:64])[0]
                r_tok = struct.unpack('<Q', raw[64:72])[0]
                complete = raw[72] if len(raw) > 72 else 0
                print(f"    v_sol={v_sol/1e9:.4f} v_tok={v_tok/1e6:.4f} r_sol={r_sol/1e9:.4f} r_tok={r_tok/1e6:.4f} complete={complete}")
        break

# Check PumpSwap pool
print("\n=== Checking PumpSwap AMM pool ===")
# PumpSwap pool layout: discriminator(8) + pool(32) + ...
# The mint might be at different offsets
for size in [212, 128, 160, 200, 240, 176, 100, 80]:
    for offset in [8, 40, 16]:
        result = rpc_call("getProgramAccounts", [
            PUMP_SWAP,
            {
                "encoding": "base64",
                "commitment": "confirmed",
                "filters": [
                    {"dataSize": size},
                    {"memcmp": {"offset": offset, "bytes": RCAP_MINT}},
                ]
            }
        ])
        if "result" in result and result["result"] and len(result["result"]) > 0:
            print(f"  ✅ PumpSwap pool found! size={size} offset={offset}: {len(result['result'])} accounts")
            for acc in result["result"]:
                pubkey = acc.get("pubkey", "?")
                data_b64 = acc.get("account", {}).get("data", [None, None])[0]
                if data_b64:
                    raw = base64.b64decode(data_b64)
                    print(f"    pubkey={pubkey} len={len(raw)}")
                    print(f"    hex: {raw[:100].hex()}")
            break
    else:
        continue
    break
else:
    print("  No PumpSwap pool found for RCAP")

# Also check: is there an associated token account for our wallet + this mint?
print("\n=== Checking our token account for RCAP ===")
# ATA = PDA(wallet, mint) for Token-2022
# Let me just check if the token account we found earlier is still valid
WALLET = "7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC"
result = rpc_call("getTokenAccountsByOwner", [
    WALLET,
    {"mint": RCAP_MINT},
    {"encoding": "jsonParsed", "commitment": "confirmed"}
])
if "result" in result:
    accounts = result["result"]["value"]
    print(f"  Token accounts for RCAP: {len(accounts)}")
    for acc in accounts:
        pubkey = acc.get("pubkey", "?")
        info = acc.get("account", {}).get("data", {}).get("parsed", {}).get("info", {})
        ui_amount = info.get("tokenAmount", {}).get("uiAmount", 0)
        raw = info.get("tokenAmount", {}).get("amount", "0")
        decimals = info.get("tokenAmount", {}).get("decimals", 0)
        print(f"  ATA: {pubkey}")
        print(f"  Amount: {ui_amount} (raw={raw}, decimals={decimals})")
elif "error" in result:
    print(f"  Error: {result['error']}")
