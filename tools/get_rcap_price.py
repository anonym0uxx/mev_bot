#!/usr/bin/env python3
"""Get RCAP token price and Jupiter quote via Helius DAS API."""
import json, os, urllib.request

# Load creds
with open('/mnt/c/Users/Alon/.hermes/creds/pump-quant.env') as f:
    creds = {}
    for line in f:
        k, _, v = line.strip().partition('=')
        creds[k] = v

HELIUS_KEY = creds.get('HELIUS_API_KEY', '')
RCAP_MINT = '7ahVBvGoTaKYpPqz9G3SwvrV7VibJGCtDUFNP4hKpump'
WALLET = creds.get('WALLET_ADDRESS', '')
RPC = f'https://mainnet.helius-rpc.com/?api-key={HELIUS_KEY}'

def rpc(method, params):
    payload = json.dumps({'jsonrpc':'2.0','id':1,'method':method,'params':params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

# 1. Get asset metadata
print("=== RCAP Asset Info ===")
result = rpc('getAsset', {'id': RCAP_MINT, 'displayOptions': {'showFungibility': True}})
asset = result.get('result', {})
print(f"  Name: {asset.get('name', '?')}")
print(f"  Symbol: {asset.get('symbol', '?')}")
print(f"  Supply: {asset.get('supply', '?')}")
print(f"  Decimals: {asset.get('decimals', '?')}")
fung = asset.get('fungibility', {})
print(f"  Fungibility: {json.dumps(fung)[:300]}")

# 2. Get token price via DAS
print("\n=== RCAP Price ===")
try:
    price_result = rpc('getAsset', {'id': RCAP_MINT, 'displayOptions': {'showFungibility': True}})
    # Try getSignaturesForAddress to find latest trade price
    sigs = rpc('getSignaturesForAddress', [RCAP_MINT, {'limit': 5}])
    for s in sigs.get('result', [])[:3]:
        print(f"  Recent tx: {s['signature'][:40]}... slot={s.get('slot','?')}")
except Exception as e:
    print(f"  Error: {e}")

# 3. Check our wallet's token accounts
print("\n=== Our Holdings ===")
ata = 'HyaWdx3XFyQceR9q9feRavw9ksivJ1GSB3sFTmRVk2ZJ'
acc = rpc('getAccountInfo', [ata, {'encoding': 'base64', 'commitment': 'confirmed'}])
val = acc.get('result', {}).get('value')
if val:
    import base64, struct
    raw = base64.b64decode(val['data'][0])
    if len(raw) >= 72:
        amount = struct.unpack('<Q', raw[64:72])[0]
        # Read mint decimals from the ATA data (offset 72)
        mint_pk = raw[0:32]
        owner_pk = raw[32:64]
        # Decimals are in the mint account, not ATA. But we can get it from DAS
        decimals = asset.get('decimals', 6)
        print(f"  ATA: {ata}")
        print(f"  Raw amount: {amount:,}")
        print(f"  Decimals: {decimals}")
        if decimals and decimals > 0:
            print(f"  UI amount: {amount / 10**decimals:.6f} RCAP")
        else:
            print(f"  UI amount: {amount:,} RCAP (0 decimals)")
        print(f"  Owner: {owner_pk.hex()[:16]}...")

# 4. Try to find liquidity pools via getProgramAccounts
# Check Raydium AMM v4 - look for pools containing RCAP mint
print("\n=== Searching for AMM pools with RCAP ===")
# Raydium AMM V4 program
RAYDIUM_AMM = '675KFP9oo8SEEs3onDPfUTRfxV2h3pZvNpQm6kYqfopR'
# Orca Whirlpool
ORCA_WHIRL = 'whirLw6XncHf5s5f1yQg7GaZp2p5Zp2p5Zp2p5Zp2p5Zp2'
# PumpSwap
PUMP_SWAP = 'pAMBPPrQqZvLqAA64t7ex5ZWp3t7p3t7p3t7p3t7p3t7p3t7p3'

# Use Helius DAS getAssetsByOwner to check for pool accounts
# Actually, let's try to find the pool by searching for token accounts of the mint
# that are owned by known DEX programs

# Let's use a simpler approach - get the recent Jupiter swap tx and find the pool
print("\n=== Decoding recent swap tx for price discovery ===")
sigs = rpc('getSignaturesForAddress', [RCAP_MINT, {'limit': 5}])
for s in sigs.get('result', [])[:2]:
    sig = s['signature']
    print(f"\n  Tx: {sig[:50]}...")
    tx = rpc('getTransaction', [sig, {'encoding': 'json', 'maxSupportedTransactionVersion': 0}])
    meta = tx.get('result', {}).get('meta', {})
    msg = tx.get('result', {}).get('transaction', {}).get('message', {})
    keys = msg.get('accountKeys', [])
    pre_token = meta.get('preTokenBalances', [])
    post_token = meta.get('postTokenBalances', [])
    
    # Find RCAP and SOL balance changes
    for i, pt in enumerate(post_token):
        idx = pt.get('accountIndex', 0)
        mint = pt.get('mint', '')
        pre_amt = None
        for pr in pre_token:
            if pr.get('accountIndex', 0) == idx:
                pre_amt = pr.get('uiTokenAmount', {}).get('uiAmount')
                break
        post_amt = pt.get('uiTokenAmount', {}).get('uiAmount')
        if pre_amt is not None or post_amt is not None:
            symbol = 'RCAP' if 'pump' in mint else ('SOL' if 'So1' in mint else mint[:8])
            print(f"    [{idx}] {symbol}: {pre_amt} -> {post_amt}")

# 5. Try Jupiter swap simulator via Helius
print("\n=== Simulating Jupiter swap (sell 220K RCAP) ===")
# Use Helius' simulateTransaction or getTransaction to estimate
# Actually, let's calculate from the recent swap data
print("  (Need swap data from recent transactions to estimate price)")
