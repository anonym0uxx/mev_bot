#!/usr/bin/env python3
"""Check our wallet's on-chain token holdings via Helius RPC."""
import json, os, sys, urllib.request

HELIUS_KEY = os.environ.get("HELIUS_API_KEY", "")
WALLET = os.environ.get("WALLET_ADDRESS", "")
if not HELIUS_KEY or not WALLET:
    # try loading from creds file
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

# Also try DAS API for metadata
def das_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

print(f"Wallet: {WALLET}")
print()

# Query standard SPL Token program
for prog_name, prog_id in [("SPL Token", "TokenkegQfeIB1r0wzEJy4Kf5pUxJ8sB7W2w7Z"),
                             ("Token-2022", "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")]:
    print(f"=== {prog_name} ({prog_id}) ===")
    try:
        result = rpc_call("getTokenAccountsByOwner", [
            WALLET,
            {"programId": prog_id},
            {"encoding": "jsonParsed", "commitment": "confirmed"}
        ])
        if "result" in result:
            accounts = result["result"]["value"]
            print(f"Token accounts: {len(accounts)}")
            holdings = []
            for acc in accounts:
                info = acc.get("account", {}).get("data", {}).get("parsed", {}).get("info", {})
                mint = info.get("mint", "?")
                ui_amount = info.get("tokenAmount", {}).get("uiAmount", 0)
                raw_amount = info.get("tokenAmount", {}).get("amount", "0")
                decimals = info.get("tokenAmount", {}).get("decimals", 0)
                if ui_amount and ui_amount > 0:
                    holdings.append((mint, ui_amount, raw_amount, decimals))
                    print(f"  mint={mint}  amount={ui_amount}  raw={raw_amount}  decimals={decimals}")
            if not holdings:
                print("  (no non-zero balances)")
        elif "error" in result:
            print(f"  Error: {result['error']}")
    except Exception as e:
        print(f"  Exception: {e}")
    print()

# Also try getAssetsByOwner via DAS API
print("=== DAS: getAssetsByOwner ===")
try:
    result = das_call("getAssetsByOwner", [WALLET, {"displayOptions": {"showFungible": True}}])
    if "result" in result:
        assets = result["result"].get("items", [])
        print(f"Assets: {len(assets)}")
        for asset in assets[:30]:
            sym = asset.get("content", {}).get("metadata", {}).get("symbol", "?")
            name = asset.get("content", {}).get("metadata", {}).get("name", "?")
            mint = asset.get("id", "?")
            amt = asset.get("token_info", {}).get("balance", 0)
            ui_amt = asset.get("token_info", {}).get("ui_amount", 0)
            if ui_amt and ui_amt > 0:
                print(f"  {sym:10s} {name:20s} mint={mint}  amount={ui_amt}")
    elif "error" in result:
        print(f"  Error: {result['error']}")
except Exception as e:
    print(f"  Exception: {e}")
