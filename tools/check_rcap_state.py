#!/usr/bin/env python3
"""Check RCAP token's current bonding curve state and expected sell output.
Queries pump.fun curve account + PumpSwap pool to determine venue and price.
"""
import json, os, urllib.request, base64, struct, sys

HELIUS_KEY = os.environ.get("HELIUS_API_KEY", "")
if not HELIUS_KEY:
    creds = "/mnt/c/Users/Alon/.hermes/creds/pump-quant.env"
    with open(creds) as f:
        for line in f:
            if line.startswith("HELIUS_API_KEY="):
                HELIUS_KEY = line.strip().split("=",1)[1]

RPC = f"https://mainnet.helius-rpc.com/?api-key={HELIUS_KEY}"
WALLET = "7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC"
RCAP_MINT = "7ahVBvGoTaKYpPqz9G3SwvrV7VibJGCtDUFNP4hKpump"
PUMP_FUN = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

def b58_decode(s):
    """Base58 decode (Bitcoin alphabet)."""
    import hashlib
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    num = 0
    for c in s:
        num = num * 58 + alphabet.index(c)
    # encode num as 32 bytes
    result = num.to_bytes(32, 'big')
    # leading zeros
    pad = 0
    for c in s:
        if c == '1': pad += 1
        else: break
    return b'\x00' * pad + result

def derive_curve(mint):
    """Derive pump.fun curve account (PDA) for a mint."""
    # PDA seeds: [b"account", b"curve", b"coins", b"1", &mint, &b""] - actually pump.fun uses specific seeds
    # The curve account is a PDA: seeds = [mint, b"curve"] or similar
    # Actually pump.fun bonding curve account derivation:
    # seeds = [b"curve", mint] with pump.fun program ID
    # But the correct derivation is via getProgramAccounts with memcmp for the mint
    return None

print(f"RCAP Mint: {RCAP_MINT}")
print(f"Wallet: {WALLET}")
print()

# 1. Find the curve account for this mint via getProgramAccounts
print("=== Finding pump.fun curve account ===")
# memcmp filter: offset 8 (after discriminator+account_type), 32 bytes = mint pubkey
mint_bytes = b58_decode(RCAP_MINT)
mint_b64 = base64.b64encode(mint_bytes).decode()

result = rpc_call("getProgramAccounts", [
    PUMP_FUN,
    {
        "encoding": "base64",
        "commitment": "confirmed",
        "filters": [
            {"memcmp": {"offset": 8, "bytes": base64.b58encode(mint_bytes).decode() if hasattr(base64, 'b58encode') else ""}},
            {"dataSize": 212}
        ]
    }
])

# Actually base64.b58encode doesn't exist in stdlib. Let me use the base58 encoding manually.
# The memcmp "bytes" field expects base58-encoded bytes.
# Let me try a different approach - use the mint directly as base58 for the memcmp

result = rpc_call("getProgramAccounts", [
    PUMP_FUN,
    {
        "encoding": "jsonParsed",
        "commitment": "confirmed",
        "filters": [
            {"memcmp": {"offset": 8, "bytes": RCAP_MINT}},
            {"dataSize": 212}
        ]
    }
])

if "result" in result and result["result"]:
    accounts = result["result"]
    print(f"Curve accounts found: {len(accounts)}")
    for acc in accounts:
        pubkey = acc.get("pubkey", "?")
        data = acc.get("account", {}).get("data", [None, None])
        print(f"  Curve account: {pubkey}")
        if data and data[0]:
            raw = base64.b64decode(data[0])
            # Parse curve state: discriminator(8) + account_type(1) + mint(32) + 
            # virtual_sol(8) + virtual_token(8) + real_sol(8) + real_token(8) + 
            # complete(1) + ...
            if len(raw) >= 85:
                # Skip discriminator(8) + account_type(1) + mint(32) = offset 41
                v_sol = struct.unpack('<Q', raw[41:49])[0]
                v_tok = struct.unpack('<Q', raw[49:57])[0]
                r_sol = struct.unpack('<Q', raw[57:65])[0]
                r_tok = struct.unpack('<Q', raw[65:73])[0]
                complete = raw[73] if len(raw) > 73 else 0
                print(f"    virtual_sol: {v_sol / 1e9:.6f} SOL ({v_sol} lamports)")
                print(f"    virtual_token: {v_tok / 1e6:.6f} tokens ({v_tok} raw)")
                print(f"    real_sol: {r_sol / 1e9:.6f} SOL ({r_sol} lamports)")
                print(f"    real_token: {r_tok / 1e6:.6f} tokens ({r_tok} raw)")
                print(f"    curve_complete: {bool(complete)}")
                
                # Calculate sell output using pump.fun bonding curve formula
                # Sell formula: out = (sol_reserves * tokens_in) / (tokens_reserves + tokens_in) * 1 - fee
                # fee = 0.25% (25 bps) for sells... actually pump.fun has 0% fee on sell? No, it's complex.
                # Pump.fun sell: out = real_sol * tokens_in / (real_token + tokens_in) * (1 - fee)
                # But actually the curve uses virtual reserves:
                # buy: out = (v_tok * sol_in) / (v_sol + sol_in) 
                # sell: out = (v_sol * tok_in) / (v_tok + tok_in)
                # fee on sell = 0% (pump.fun charges fee on buy only? Or both?)
                # Actually pump.fun: buy fee = 1% (100 bps... no, the fee_bps in instruction is different)
                # The protocol fee is 0.25% I think. Let me just compute both.
                
                sell_amount = 220371635503  # our raw token amount
                # Sell formula (pump.fun): sol_out = (v_sol * tok_in) / (v_tok + tok_in) * (1 - fee_bps/10000)
                # Standard fee on sell = 100 bps (1%)? Actually pump.fun doesn't charge on sell I think.
                # Let's compute without fee first:
                sol_out_no_fee = (v_sol * sell_amount) / (v_tok + sell_amount)
                print(f"\n    === SELL QUOTE (no fee) ===")
                print(f"    sell_amount: {sell_amount / 1e6:.6f} tokens ({sell_amount} raw)")
                print(f"    sol_out: {sol_out_no_fee / 1e9:.6f} SOL ({int(sol_out_no_fee)} lamports)")
                
                # With 1% fee (100 bps):
                sol_out_1pct = sol_out_no_fee * 0.99
                print(f"    sol_out (1% fee): {sol_out_1pct / 1e9:.6f} SOL")
                
                # Token price per token
                price_per_token = (v_sol / 1e9) / (v_tok / 1e6) if v_tok > 0 else 0
                print(f"    price per token: {price_per_token:.10f} SOL")
                print(f"    total value (no fee): {sell_amount / 1e6 * price_per_token:.6f} SOL")
else:
    print(f"No curve accounts found. Token may have graduated to PumpSwap.")
    if "error" in result:
        print(f"Error: {result['error']}")
    # Try PumpSwap pool
    print("\n=== Checking PumpSwap pool ===")
    PUMP_SWAP = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
    result2 = rpc_call("getProgramAccounts", [
        PUMP_SWAP,
        {
            "encoding": "jsonParsed",
            "commitment": "confirmed",
            "filters": [
                {"memcmp": {"offset": 8, "bytes": RCAP_MINT}},
            ]
        }
    ])
    if "result" in result2 and result2["result"]:
        print(f"PumpSwap pools found: {len(result2['result'])}")
    else:
        print("No PumpSwap pool found either. Token may be fully migrated.")
        if "error" in result2:
            print(f"Error: {result2['error']}")
