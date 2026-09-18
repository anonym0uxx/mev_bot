#!/usr/bin/env python3
"""Find which token in our wallet is RCAP by querying DAS getAsset for each mint."""
import json, os, urllib.request, time, sys

HELIUS_KEY = os.environ.get("HELIUS_API_KEY", "")
if not HELIUS_KEY:
    creds = "/mnt/c/Users/Alon/.hermes/creds/pump-quant.env"
    with open(creds) as f:
        for line in f:
            if line.startswith("HELIUS_API_KEY="):
                HELIUS_KEY = line.strip().split("=",1)[1]
            elif line.startswith("WALLET_ADDRESS="):
                WALLET = line.strip().split("=",1)[1]

RPC = f"https://mainnet.helius-rpc.com/?api-key={HELIUS_KEY}"

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

# First get all token accounts with balances
result = rpc_call("getTokenAccountsByOwner", [
    "7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC",
    {"programId": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"},
    {"encoding": "jsonParsed", "commitment": "confirmed"}
])

holdings = []
if "result" in result:
    for acc in result["result"]["value"]:
        info = acc.get("account", {}).get("data", {}).get("parsed", {}).get("info", {})
        mint = info.get("mint", "?")
        ui_amount = info.get("tokenAmount", {}).get("uiAmount", 0)
        raw_amount = info.get("tokenAmount", {}).get("amount", "0")
        decimals = info.get("tokenAmount", {}).get("decimals", 0)
        if ui_amount and ui_amount > 0:
            holdings.append((mint, ui_amount, raw_amount, decimals))

print(f"Non-zero holdings: {len(holdings)}")
print()

# Query DAS getAsset for each to find RCAP
print("Querying DAS for token symbols...")
rcap_found = []
for i, (mint, ui_amount, raw, decimals) in enumerate(holdings):
    try:
        res = rpc_call("getAsset", [mint, {"displayOptions": {"showFungible": True}}])
        if "result" in res:
            asset = res["result"]
            symbol = asset.get("content", {}).get("metadata", {}).get("symbol", "?")
            name = asset.get("content", {}).get("metadata", {}).get("name", "?")
            print(f"  [{i+1}] {symbol:12s} | {name:25s} | mint={mint} | amount={ui_amount}")
            if symbol.upper() == "RCAP":
                rcap_found.append((mint, ui_amount, raw, decimals, symbol, name))
        elif "error" in res:
            print(f"  [{i+1}] ERROR for mint={mint}: {res['error'].get('message','')[:80]}")
    except Exception as e:
        print(f"  [{i+1}] Exception for mint={mint}: {e}")
    time.sleep(0.15)  # rate limit

print()
if rcap_found:
    print("=== RCAP FOUND ===")
    for mint, ui_amount, raw, decimals, symbol, name in rcap_found:
        print(f"  Symbol: {symbol}")
        print(f"  Name: {name}")
        print(f"  Mint: {mint}")
        print(f"  Amount: {ui_amount} (raw: {raw}, decimals: {decimals})")
else:
    print("=== RCAP NOT FOUND in wallet holdings ===")
    print("None of the held tokens have symbol 'RCAP'")
