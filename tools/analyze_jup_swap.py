#!/usr/bin/env python3
"""Decode the Jupiter swap transactions for RCAP to find:
1. The actual DEX/AMM pool providing liquidity
2. The price (SOL per token)
3. Whether we can sell 220K tokens
Also query Jupiter API for a quote.
"""
import json, os, urllib.request, base64, struct, sys, time

HELIUS_KEY = os.environ.get("HELIUS_API_KEY", "")
if not HELIUS_KEY:
    creds = "/mnt/c/Users/Alon/.hermes/creds/pump-quant.env"
    with open(creds) as f:
        for line in f:
            k, _, v = line.strip().partition("=")
            if k == "HELIUS_API_KEY": HELIUS_KEY = v

RPC = f"https://mainnet.helius-rpc.com/?api-key={HELIUS_KEY}"
RCAP_MINT = "7ahVBvGoTaKYpPqz9G3SwvrV7VibJGCtDUFNP4hKpump"
SOL_MINT = "So11111111111111111111111111111111111111112"
WALLET = "7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC"
JUP_V6 = "JUP6LkbZbjS1jKKwapdHBvb4fvYsLIBUJSJp63ZkYG"
ATA = "HyaWdx3XFyQceR9q9feRavw9ksivJ1GSB3sFTmRVk2ZJ"

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

def fix_b64(s):
    if not s: return ""
    return s + "=" * (4 - len(s) % 4) if len(s) % 4 != 0 else s

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

def get_account_info(pubkey):
    result = rpc_call("getAccountInfo", [pubkey, {"encoding": "base64", "commitment": "confirmed"}])
    if "result" in result and result["result"]["value"]:
        return result["result"]["value"]
    return None

# Step 1: Get the Jupiter swap transaction and decode account list
print("=== Decoding Jupiter swap tx for RCAP ===")
result = rpc_call("getSignaturesForAddress", [RCAP_MINT, {"limit": 10}])
sigs = result.get("result", [])

# Find the Jupiter tx (tx[1] from earlier = slot 441162904)
jup_sig = None
for s in sigs:
    tx_result = rpc_call("getTransaction", [s["signature"], {"encoding": "json", "maxSupportedTransactionVersion": 0}])
    if "result" not in tx_result: continue
    tx = tx_result["result"]
    msg = tx.get("transaction", {}).get("message", {})
    account_keys = msg.get("accountKeys", [])
    for key in account_keys:
        if key.startswith("JUP6"):
            jup_sig = s["signature"]
            print(f"Found Jupiter tx: {jup_sig[:60]}...")
            break
    if jup_sig: break

if jup_sig:
    tx_result = rpc_call("getTransaction", [jup_sig, {"encoding": "json", "maxSupportedTransactionVersion": 0}])
    tx = tx_result["result"]
    meta = tx.get("meta", {})
    msg = tx.get("transaction", {}).get("message", {})
    account_keys = msg.get("accountKeys", [])
    pre_balances = tx.get("meta", {}).get("preTokenBalances", [])
    post_balances = tx.get("meta", {}).get("postTokenBalances", [])
    pre_sol = tx.get("meta", {}).get("preBalances", [])
    post_sol = tx.get("meta", {}).get("postBalances", [])
    
    print(f"\nAccount keys ({len(account_keys)}):")
    for i, key in enumerate(account_keys):
        # Check owner of each account
        acc = get_account_info(key)
        owner = acc["owner"] if acc else "?"
        owner_short = owner[:16] + "..." if len(owner) > 16 else owner
        print(f"  [{i}] {key} owner={owner_short}")
    
    # Show token balance changes to identify buyer/seller and amounts
    print(f"\n=== Token balance changes ===")
    for pre in pre_balances:
        idx = pre.get("accountIndex", -1)
        mint_idx = pre.get("mint", "")
        amt = pre.get("uiTokenAmount", {}).get("uiAmount", 0)
        print(f"  PRE  [{idx}] mint={mint_idx[:20]}... amt={amt}")
    for post in post_balances:
        idx = post.get("accountIndex", -1)
        mint_idx = post.get("mint", "")
        amt = post.get("uiTokenAmount", {}).get("uiAmount", 0)
        print(f"  POST [{idx}] mint={mint_idx[:20]}... amt={amt}")
    
    # SOL balance changes for the signer
    print(f"\n=== SOL balance changes ===")
    for i in range(min(5, len(pre_sol))):
        delta = (post_sol[i] if i < len(post_sol) else 0) - (pre_sol[i] if i < len(pre_sol) else 0)
        if delta != 0:
            print(f"  [{i}] {account_keys[i][:20]}... delta={delta/1e9:.6f} SOL")

# Step 2: Check our ATA balance
print(f"\n=== Our RCAP ATA balance ===")
acc = get_account_info(ATA)
if acc:
    raw = base64.b64decode(fix_b64(acc["data"][0]))
    if len(raw) >= 72:
        amount = struct.unpack('<Q', raw[64:72])[0]
        decimals = raw[72] if len(raw) > 72 else 6
        print(f"  ATA: {ATA}")
        print(f"  Balance: {amount / 10**decimals:.6f} RCAP ({amount:,} raw)")
        print(f"  Decimals: {decimals}")

# Step 3: Query Jupiter API for a sell quote
print(f"\n=== Jupiter quote: sell 220,371 RCAP for SOL ===")
amount_raw = 220371635503  # 220,371.635503 * 10^6
jup_quote_url = f"https://quote-api.jup.ag/v6/quote?inputMint={RCAP_MINT}&outputMint={SOL_MINT}&amount={amount_raw}&slippageBps=500"
try:
    req = urllib.request.Request(jup_quote_url, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        quote = json.loads(resp.read())
    if quote and "data" in quote and quote["data"]:
        best = quote["data"][0] if isinstance(quote["data"], list) else quote
        if isinstance(quote, list) and quote:
            best = quote[0]
        
        # Handle different API response formats
        if "outAmount" in quote:
            out_amount = int(quote["outAmount"])
            print(f"  ✅ QUOTE RECEIVED!")
            print(f"  Input: {amount_raw / 1e6:.6f} RCAP")
            print(f"  Output: {out_amount / 1e9:.6f} SOL ({out_amount:,} lamports)")
            print(f"  Price: {out_amount / amount_raw * 1e3:.12f} SOL per RCAP")
            print(f"  Slippage: 5% (500 bps)")
            if "swapUsd" in quote:
                print(f"  USD value: ${quote.get('swapUsd', '?')}")
            print(f"  Routes: {json.dumps(quote.get('routePlan', [])[:3], indent=2)[:200]}")
        elif isinstance(quote, list) and len(quote) > 0 and "outAmount" in quote[0]:
            best = quote[0]
            out_amount = int(best["outAmount"])
            print(f"  ✅ QUOTE RECEIVED!")
            print(f"  Input: {amount_raw / 1e6:.6f} RCAP")
            print(f"  Output: {out_amount / 1e9:.6f} SOL ({out_amount:,} lamports)")
            print(f"  Price: {out_amount / amount_raw * 1e3:.12f} SOL per RCAP")
        else:
            print(f"  Raw quote response: {json.dumps(quote)[:500]}")
    elif "error" in quote:
        print(f"  ❌ Jupiter error: {quote['error']}")
    else:
        print(f"  No quotes returned. Raw: {json.dumps(quote)[:300]}")
except urllib.error.HTTPError as e:
    body = e.read().decode()
    print(f"  ❌ HTTP {e.code}: {body[:300]}")
except Exception as e:
    print(f"  ❌ Error: {e}")

# Step 4: Try smaller amount to check if liquidity exists
print(f"\n=== Jupiter quote: sell 10,000 RCAP (smaller test) ===")
small_amount = 10000 * 10**6
jup_quote_url2 = f"https://quote-api.jup.ag/v6/quote?inputMint={RCAP_MINT}&outputMint={SOL_MINT}&amount={small_amount}&slippageBps=500"
try:
    req = urllib.request.Request(jup_quote_url2, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        quote2 = json.loads(resp.read())
    if quote2 and "outAmount" in quote2:
        out2 = int(quote2["outAmount"])
        print(f"  ✅ Quote for 10K RCAP: {out2/1e9:.6f} SOL")
        print(f"  Implied price: {out2 / small_amount * 1e3:.12f} SOL/RCAP")
        print(f"  Implied full-bag value: {out2 / small_amount * amount_raw / 1e9:.6f} SOL (linear, ignores slippage)")
    elif isinstance(quote2, list) and quote2 and "outAmount" in quote2[0]:
        out2 = int(quote2[0]["outAmount"])
        print(f"  ✅ Quote for 10K RCAP: {out2/1e9:.6f} SOL")
    else:
        print(f"  No quote for small amount. Raw: {json.dumps(quote2)[:300]}")
except urllib.error.HTTPError as e:
    body = e.read().decode()
    print(f"  ❌ HTTP {e.code}: {body[:300]}")
except Exception as e:
    print(f"  ❌ Error: {e}")
