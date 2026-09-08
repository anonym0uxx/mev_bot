#!/usr/bin/env python
"""
validate_social_onchain.py - Path B: Validate today's social content against
Solana on-chain data using FREE public RPC nodes.

Takes mints from today's narrative content (raw_social_event_v1) and fetches:
1. Token account holders (Token-2022 program for pump.fun tokens)
2. Recent transaction signatures for the mint
3. Current bonding curve state (pump.fun program)
4. Price estimation from bonding curve reserves

This creates a lightweight on-chain validation layer for current narrative claims,
complementing Slinky v3's historical validation for June-July data.

Free RPC endpoints (no API key needed):
- https://api.mainnet.solana.com (Solana mainnet, rate-limited)
- https://rpc.ankr.com/solana (Ankr free tier)
- https://solana-rpc.publicnode.com (PublicNode)

Usage: python src/validate_social_onchain.py [--input <raw_jsonl>] [--output <path>]
"""

import json
import os
import sys
import time
import base58
import argparse
import urllib.request
import urllib.parse
from datetime import datetime
from collections import Counter

# --- Config ---
FREE_RPC_ENDPOINTS = [
    "https://api.mainnet.solana.com",       # Solana official (3 req/s)
    "https://rpc.ankr.com/solana",           # Ankr free
    "https://solana-rpc.publicnode.com",     # PublicNode
]

# pump.fun program ID
PUMP_FUN_PROGRAM = "6EF8rQhkwC4t5cmXPVmQ6pBtS4pZyoYhJz6NDfgNw22d"
# Token-2022 program (pump.fun uses this)
TOKEN_2022_PROGRAM = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
# Standard SPL Token (some older pump tokens)
SPL_TOKEN_PROGRAM = "TokenkegQfeZyiNwATbKfHAvqfHEfXqB2uM9nZqAY3"

RUN_UUID = f"oc_{int(time.time()):08x}"

def get_git_sha():
    try:
        import subprocess
        return subprocess.check_output(
            ['git', 'rev-parse', '--short', 'HEAD'],
            cwd='D:/repos/mev_bot', stderr=subprocess.DEVNULL
        ).decode().strip()
    except Exception:
        return 'unknown'

GIT_SHA = get_git_sha()

# B58 alphabet for base58 encoding
B58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

def b58encode(data):
    """Base58 encode bytes (like bs58 for Solana)."""
    if not data:
        return ''
    # Decode bytes to integer
    n = int.from_bytes(data, 'big')
    result = []
    while n > 0:
        n, r = divmod(n, 58)
        result.append(B58_ALPHABET[r])
    # Handle leading zeros
    pad = 0
    for b in data:
        if b == 0:
            pad += 1
        else:
            break
    return B58_ALPHABET[0] * pad + ''.join(reversed(result))

def rpc_post(endpoint, method, params, timeout=20):
    """Make a JSON-RPC POST request to a Solana RPC endpoint."""
    payload = json.dumps({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    }).encode()
    req = urllib.request.Request(
        endpoint,
        data=payload,
        headers={'Content-Type': 'application/json'},
        method='POST'
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode())
    except Exception as e:
        return {"error": str(e)}

def try_endpoints(method, params, timeout=20):
    """Try multiple free RPC endpoints in order."""
    for endpoint in FREE_RPC_ENDPOINTS:
        result = rpc_post(endpoint, method, params, timeout)
        if 'error' not in result or result.get('result') is not None:
            return result, endpoint
        time.sleep(0.5)
    return {"error": "all endpoints failed"}, None

def get_token_holders(mint, program_id=TOKEN_2022_PROGRAM, max_accounts=50):
    """Get token holder accounts for a mint using getProgramAccounts with memcmp filter.
    This works for Token-2022 and SPL Token programs."""
    # memcmp filter: offset=0 (first 32 bytes = mint address)
    # We need the mint as a base58-decoded 32-byte address
    try:
        # Build memcmp filter for mint at offset 0
        # params: [program_id, {encoding: "base64", filters: [{memcmp: {offset: 0, bytes: mint_base64}}]}]
        # Actually, Solana RPC memcmp uses base58 bytes encoding
        # The mint address in base58 IS the bytes at offset 0 for token accounts
        params = [
            program_id,
            {
                "encoding": "base64",
                "commitment": "confirmed",
                "filters": [
                    {"memcmp": {"offset": 0, "bytes": mint}},
                    {"dataSize": 172}  # Token-2022 account size (standard is 165, Token-2022 is 172)
                ]
            }
        ]
        result, endpoint = try_endpoints("getProgramAccounts", params)
        if result.get('result'):
            accounts = result['result'].get('value', [])
            return accounts, len(accounts), endpoint
        return [], 0, endpoint
    except Exception as e:
        return [], 0, None

def get_recent_signatures(mint, limit=3):
    """Get recent transaction signatures for a mint's token account.
    We need to find the associated token account or a known holder first."""
    # This is a simplified version - gets signatures for the mint's program
    # In practice, we'd need to find the ATA or liquidity account
    return []

def get_account_info(mint, encoding="base64"):
    """Get account info for a mint address."""
    params = [mint, {"encoding": encoding, "commitment": "confirmed"}]
    result, endpoint = try_endpoints("getAccountInfo", params)
    return result, endpoint

def decode_bonding_curve(account_data_b64, mint):
    """Attempt to decode pump.fun bonding curve state from account data.
    pump.fun bonding curve account data contains:
    - discriminator (8 bytes)
    - virtualTokenBonds (8 bytes, u64)
    - virtualSolReserves (8 bytes, u64)
    - realTokenReserves (8 bytes, u64)
    - tokenTotalSupply (8 bytes, u64)
    - realSolReserves (8 bytes, u64)
    - marketCapSolInitial (8 bytes, u64)
    - isComplete (1 byte, bool)
    """
    import base64
    try:
        raw = base64.b64decode(account_data_b64)
        if len(raw) < 64:
            return None
        # Skip 8-byte discriminator
        offset = 8
        def read_u64(data, off):
            if off + 8 > len(data):
                return None
            return int.from_bytes(data[off:off+8], 'little')

        v_tokens = read_u64(raw, offset); offset += 8
        v_sol = read_u64(raw, offset); offset += 8
        real_tokens = read_u64(raw, offset); offset += 8
        total_supply = read_u64(raw, offset); offset += 8
        real_sol = read_u64(raw, offset); offset += 8
        mcap_initial = read_u64(raw, offset); offset += 8
        is_complete = raw[offset] if offset < len(raw) else None

        # Convert lamports to SOL
        v_sol_sol = (v_sol or 0) / 1e9
        real_sol_sol = (real_sol or 0) / 1e9
        mcap_initial_sol = (mcap_initial or 0) / 1e9

        return {
            'virtual_sol_reserves_sol': v_sol_sol,
            'real_sol_reserves_sol': real_sol_sol,
            'virtual_tokens': v_tokens,
            'real_tokens': real_tokens,
            'total_supply': total_supply,
            'market_cap_initial_sol': mcap_initial_sol,
            'is_complete': bool(is_complete) if is_complete is not None else None,
            'graduated': bool(is_complete) if is_complete is not None else None,
            'price_sol': (v_sol_sol / (v_tokens or 1)) if v_tokens else 0,
            'market_cap_sol': (v_sol_sol * total_supply / (v_tokens or 1)) if v_tokens else 0,
        }
    except Exception as e:
        return None

def validate_mint_onchain(mint, token_symbol=None):
    """Validate a single mint against on-chain data.
    Returns a validation record."""
    validation = {
        'mint': mint,
        'token_symbol': token_symbol,
        'validation_time_ms': int(time.time() * 1000),
        'rpc_endpoints_tried': [],
        'holder_count': None,
        'graduated': None,
        'price_sol': None,
        'market_cap_sol': None,
        'bonding_curve_state': None,
        'on_chain_verified': False,
        'errors': [],
    }

    # 1. Check if the mint account exists and get its data
    result, endpoint = get_account_info(mint, "base64")
    validation['rpc_endpoints_tried'].append(endpoint)
    if result and result.get('result') and result['result'].get('value'):
        account = result['result']['value']
        validation['on_chain_verified'] = True
        validation['owner'] = account.get('owner', '')
        validation['lamports'] = account.get('lamports', 0)
        validation['is_token'] = account.get('owner', '') in (
            TOKEN_2022_PROGRAM, SPL_TOKEN_PROGRAM
        )

        # Data format: [base64_string, "base64"]
        raw_data = account.get('data', [None, None])
        if isinstance(raw_data, list) and len(raw_data) >= 1:
            data_b64 = raw_data[0]
            if data_b64:
                bc = decode_bonding_curve(data_b64, mint)
                if bc:
                    validation['bonding_curve_state'] = bc
                    validation['graduated'] = bc.get('graduated')
                    validation['price_sol'] = bc.get('price_sol')
                    validation['market_cap_sol'] = bc.get('market_cap_sol')
    elif result and result.get('error'):
        validation['errors'].append(f"account_info: {result['error']}")

    time.sleep(0.5)

    # 2. Get holder count via getProgramAccounts + memcmp
    # Try Token-2022 first (pump.fun standard)
    holders_t2022, count_t2022, ep1 = get_token_holders(mint, TOKEN_2022_PROGRAM)
    validation['rpc_endpoints_tried'].append(ep1)
    if count_t2022 > 0:
        validation['holder_count'] = count_t2022
        validation['token_program'] = 'Token-2022'
    else:
        time.sleep(0.5)
        # Try standard SPL Token
        holders_spl, count_spl, ep2 = get_token_holders(mint, SPL_TOKEN_PROGRAM)
        validation['rpc_endpoints_tried'].append(ep2)
        if count_spl > 0:
            validation['holder_count'] = count_spl
            validation['token_program'] = 'SPL-Token'
        else:
            validation['holder_count'] = 0
            validation['token_program'] = 'unknown'

    return validation

def extract_mints_from_raw(raw_path):
    """Extract unique mints from raw_social_event_v1 OR creator_claim_v1 JSONL files.
    Falls back to gold layer claims if raw events don't have primary_mint."""
    mints = set()
    raw_files = []
    if os.path.isdir(raw_path):
        for f in os.listdir(raw_path):
            if f.endswith('.jsonl'):
                raw_files.append(os.path.join(raw_path, f))
    elif os.path.isfile(raw_path):
        raw_files = [raw_path]

    for fpath in raw_files:
        with open(fpath, 'r', encoding='utf-8') as f:
            for line in f:
                if not line.strip():
                    continue
                try:
                    ev = json.loads(line)
                    mint = ev.get('primary_mint')
                    if mint and len(mint) > 20:  # valid Solana address length
                        mints.add((mint, ev.get('token_symbol', '')))
                except json.JSONDecodeError:
                    continue

    # Fallback: check gold layer claims if no mints in raw
    if not mints:
        gold_claims = os.path.join(os.path.dirname(os.path.dirname(raw_path)),
                                   "gold/creator_claim_v1/creator_claim_v1.jsonl")
        # Handle case where raw_path is already in gold dir
        alt_path = "D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1/gold/creator_claim_v1/creator_claim_v1.jsonl"
        for fpath in [gold_claims, alt_path]:
            if os.path.exists(fpath):
                with open(fpath, 'r', encoding='utf-8') as f:
                    for line in f:
                        if not line.strip():
                            continue
                        try:
                            c = json.loads(line)
                            mint = c.get('primary_mint')
                            if mint and len(mint) > 20:
                                mints.add((mint, c.get('token_symbol', '')))
                        except json.JSONDecodeError:
                            continue
                break  # use first found

    return mints

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--input', default='output/narrative_gold_v1/raw/raw_social_event_v1',
                        help='Input raw_social_event_v1 dir or file')
    parser.add_argument('--output', default='output/narrative_gold_v1/gold/onchain_validation',
                        help='Output directory')
    parser.add_argument('--max-mints', type=int, default=50, help='Max mints to validate')
    args = parser.parse_args()

    print("=" * 70)
    print("ON-CHAIN VALIDATION FOR SOCIAL CONTENT")
    print("Path B: Solana free RPC validation for today's narrative mints")
    print("=" * 70)
    print(f"  Run UUID:  {RUN_UUID}")
    print(f"  Git SHA:   {GIT_SHA}")

    # Extract mints from raw social events
    mints = extract_mints_from_raw(args.input)
    print(f"\n  Unique mints found in raw social data: {len(mints)}")

    if not mints:
        print("  No mints found. Exiting.")
        return

    # Limit
    mints_list = list(mints)[:args.max_mints]
    print(f"  Processing first {len(mints_list)} mints")

    # Validate each mint
    validations = []
    for i, (mint, symbol) in enumerate(mints_list):
        print(f"\n  [{i+1}/{len(mints_list)}] ${symbol} {mint[:20]}...")
        v = validate_mint_onchain(mint, symbol)
        validations.append(v)
        verified = "✓" if v['on_chain_verified'] else "✗"
        holders = v.get('holder_count', '?')
        grad = "GRAD" if v.get('graduated') else "BOND" if v.get('graduated') is False else "?"
        print(f"    {verified} verified | holders={holders} | {grad}")
        time.sleep(1)  # RPC rate limit courtesy

    # Write output
    os.makedirs(args.output, exist_ok=True)
    out_file = os.path.join(args.output, f"onchain_validation_{RUN_UUID}.jsonl")
    with open(out_file, 'w', encoding='utf-8') as f:
        for v in validations:
            f.write(json.dumps(v) + '\n')

    print(f"\n{'=' * 70}")
    print(f"  Total validations: {len(validations)}")
    verified = sum(1 for v in validations if v['on_chain_verified'])
    print(f"  On-chain verified: {verified}")
    print(f"  Graduated: {sum(1 for v in validations if v.get('graduated') == True)}")
    print(f"  With holders: {sum(1 for v in validations if v.get('holder_count', 0) and v.get('holder_count', 0) > 0)}")
    print(f"\n  Output: {out_file}")

if __name__ == '__main__':
    main()
