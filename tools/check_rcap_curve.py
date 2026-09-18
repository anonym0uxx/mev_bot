#!/usr/bin/env python3
"""Query RCAP bonding curve state via getProgramAccounts with dataSize filter."""
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

def rpc_call(method, params):
    payload = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=payload, headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=15) as resp:
        return json.loads(resp.read())

print(f"RCAP Mint: {RCAP_MINT}")
print()

# Try multiple sizes + offsets to find the curve account
for size in [212, 96, 104, 80, 74]:
    for offset in [8, 9]:
        result = rpc_call("getProgramAccounts", [
            PUMP_FUN,
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
            print(f"✅ Found at size={size} offset={offset}: {len(result['result'])} accounts")
            for acc in result["result"]:
                pubkey = acc.get("pubkey", "?")
                data_b64 = acc.get("account", {}).get("data", [None, None])[0]
                if data_b64:
                    raw = base64.b64decode(data_b64)
                    print(f"  Curve account: {pubkey}")
                    print(f"  Data length: {len(raw)} bytes")
                    print(f"  Raw hex (first 80): {raw[:80].hex()}")
                    
                    # Parse pump.fun bonding curve (Anchor format):
                    # discriminator(8) + mint(32) + virtual_sol(8) + virtual_token(8) + 
                    # real_sol(8) + real_token(8) + complete(1) + ...
                    if offset == 8:
                        # mint at offset 8, so:
                        # v_sol at 40, v_tok at 48, r_sol at 56, r_tok at 64, complete at 72
                        v_sol = struct.unpack('<Q', raw[40:48])[0]
                        v_tok = struct.unpack('<Q', raw[48:56])[0]
                        r_sol = struct.unpack('<Q', raw[56:64])[0]
                        r_tok = struct.unpack('<Q', raw[64:72])[0]
                        complete = raw[72] if len(raw) > 72 else 0
                    elif offset == 9:
                        # mint at offset 9 (after account_type), so:
                        # v_sol at 41, v_tok at 49, r_sol at 57, r_tok at 65, complete at 73
                        v_sol = struct.unpack('<Q', raw[41:49])[0]
                        v_tok = struct.unpack('<Q', raw[49:57])[0]
                        r_sol = struct.unpack('<Q', raw[57:65])[0]
                        r_tok = struct.unpack('<Q', raw[65:73])[0]
                        complete = raw[73] if len(raw) > 73 else 0
                    
                    print(f"\n  === CURVE STATE ===")
                    print(f"  virtual_sol: {v_sol / 1e9:.6f} SOL ({v_sol} lamports)")
                    print(f"  virtual_token: {v_tok / 1e6:.6f} tokens ({v_tok} raw)")
                    print(f"  real_sol: {r_sol / 1e9:.6f} SOL ({r_sol} lamports)")
                    print(f"  real_token: {r_tok / 1e6:.6f} tokens ({r_tok} raw)")
                    print(f"  complete: {bool(complete)}")
                    
                    if not complete:
                        # Sell quote: pump.fun sell formula
                        # sol_out = (v_sol * tokens_in) / (v_tok + tokens_in) * (1 - fee)
                        # pump.fun sell fee = 0% (no fee on sells, only on buys at 1%? No, 0.25%?)
                        # Actually pump.fun: buy fee = 1%, sell fee = 0% (protocol fee only on buy)
                        # Wait, actually pump.fun has a 0.25% fee on both? Let me compute multiple scenarios
                        sell = 220371635503  # raw token amount
                        sol_out_no_fee = (v_sol * sell) / (v_tok + sell)
                        sol_out_25bp = sol_out_no_fee * (1 - 0.0025)
                        sol_out_100bp = sol_out_no_fee * (1 - 0.01)
                        
                        print(f"\n  === SELL QUOTE (220,371 RCAP) ===")
                        print(f"  sol_out (no fee):  {sol_out_no_fee / 1e9:.6f} SOL ({int(sol_out_no_fee):,} lamports)")
                        print(f"  sol_out (0.25%):   {sol_out_25bp / 1e9:.6f} SOL")
                        print(f"  sol_out (1%):      {sol_out_100bp / 1e9:.6f} SOL")
                        
                        # Price
                        price = (v_sol / 1e9) / (v_tok / 1e6) if v_tok > 0 else 0
                        print(f"\n  price per token: {price:.10f} SOL")
                        print(f"  market cap: {(v_sol / 1e9) * (v_tok + v_sol) / (v_tok / 1e6) * 1e6 / 1e9:.4f} SOL (approx)")
                    else:
                        print(f"\n  ⚠️  CURVE COMPLETE - Token has migrated to PumpSwap AMM")
            raise SystemExit()  # done
        elif "error" in result:
            pass  # try next combo
        
print("No curve account found for any size/offset combination.")
print("The token may have fully migrated to PumpSwap or the curve was closed.")
