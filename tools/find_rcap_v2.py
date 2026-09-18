#!/usr/bin/env python3
"""Find RCAP token in our wallet via Helius DAS API (getAssetsByOwner) + fallback token metadata."""
import json, os, urllib.request, time

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

WALLET = "7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC"

# DAS getAssetsByOwner - correct params: [owner, sortBy, limit, page, displayOptions, readMethods]
print("=== DAS: getAssetsByOwner (fungible) ===")
try:
    result = rpc_call("getAssetsByOwner", [
        WALLET,
        {"sortBy": "created", "limit": 100, "page": 1},
        {"displayOptions": {"showFungible": True}},
    ])
    if "result" in result:
        assets = result["result"].get("items", [])
        print(f"Assets: {len(assets)}")
        for asset in assets:
            sym = asset.get("content", {}).get("metadata", {}).get("symbol", "?")
            name = asset.get("content", {}).get("metadata", {}).get("name", "?")
            mint = asset.get("id", asset.get("mint", "?"))
            ti = asset.get("token_info", {})
            ui_amt = ti.get("ui_amount", ti.get("balance", 0))
            decimals = ti.get("decimals", 0)
            symbol_upper = sym.upper()
            marker = " <<<< RCAP!" if symbol_upper == "RCAP" else ""
            if ui_amt and ui_amt > 0:
                print(f"  {sym:12s} | {name:25s} | mint={mint} | amount={ui_amt}{marker}")
    elif "error" in result:
        print(f"Error: {json.dumps(result['error'])}")
except Exception as e:
    print(f"Exception: {e}")

print()
print("=== Fallback: Query each held mint via getTokenMetadata ===")
# Get holdings first
result = rpc_call("getTokenAccountsByOwner", [
    WALLET,
    {"programId": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"},
    {"encoding": "jsonParsed", "commitment": "confirmed"}
])

holdings = []
if "result" in result:
    for acc in result["result"]["value"]:
        info = acc.get("account", {}).get("data", {}).get("parsed", {}).get("info", {})
        mint = info.get("mint", "?")
        ui_amount = info.get("tokenAmount", {}).get("uiAmount", 0)
        raw = info.get("tokenAmount", {}).get("amount", "0")
        decimals = info.get("tokenAmount", {}).get("decimals", 0)
        if ui_amount and ui_amount > 0:
            holdings.append((mint, ui_amount, raw, decimals))

# Query metadata for each
for mint, ui_amount, raw, decimals in holdings:
    try:
        # getAsset with correct displayOptions format (booleans, not objects)
        res = rpc_call("getAsset", [mint])
        if "result" in res:
            asset = res["result"]
            sym = asset.get("content", {}).get("metadata", {}).get("symbol", "?")
            name = asset.get("content", {}).get("metadata", {}).get("name", "?")
            marker = " <<<< RCAP!" if sym.upper() == "RCAP" else ""
            print(f"  {sym:12s} | {name:25s} | mint={mint[:20]}... | amount={ui_amount}{marker}")
        elif "error" in res:
            # try fetching metadata via metaplex
            print(f"  ???? | mint={mint[:20]}... | amount={ui_amount} (DAS error: {res['error'].get('message','')[:60]})")
    except Exception as e:
        print(f"  ???? | mint={mint[:20]}... | amount={ui_amount} (ex: {e})")
    time.sleep(0.12)
