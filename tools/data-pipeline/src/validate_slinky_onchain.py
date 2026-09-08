#!/usr/bin/env python
"""
validate_slinky_onchain.py - Rate-limited on-chain validation for Slinky mints.

Uses multiple free RPC endpoints with long delays to avoid 429 errors.
Runs in background, accumulating validations over time.

Free RPC endpoints (rotated to avoid rate limits):
- https://api.mainnet.solana.com (3 req/s)
- https://rpc.ankr.com/solana
- https://solana-rpc.publicnode.com

Usage: python src/validate_slinky_onchain.py [--batch-size 5] [--delay 3]
"""

import json
import os
import sys
import time
import base64
import argparse
import urllib.request
from datetime import datetime
from collections import Counter

RPC_ENDPOINTS = [
    "https://api.mainnet.solana.com",
    "https://rpc.ankr.com/solana",
    "https://solana-rpc.publicnode.com",
]

TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
SPL_TOKEN = "TokenkegQfeZyiNwATbKfHAvqfHEfXqB2uM9nZqAY3"

SLINKY_TARGETS = "output/narrative_gold_v1/slinky_target_mints_enriched.json"
OUTPUT_DIR = "output/narrative_gold_v1/gold/onchain_validation"
OUTPUT_FILE = os.path.join(OUTPUT_DIR, "slinky_onchain_validation_full.jsonl")

def rpc_post(endpoint, method, params, timeout=30):
    payload = json.dumps({
        "jsonrpc": "2.0", "id": 1,
        "method": method,
        "params": params
    }).encode()
    req = urllib.request.Request(
        endpoint, data=payload,
        headers={'Content-Type': 'application/json'},
        method='POST'
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode())
    except Exception as e:
        return {"error": str(e), "_endpoint": endpoint}

def rpc_with_rotation(method, params, timeout=30):
    """Try RPC endpoints in rotation. Returns (result, endpoint_used)."""
    for endpoint in RPC_ENDPOINTS:
        result = rpc_post(endpoint, method, params, timeout)
        if result.get('result') is not None and 'error' not in result:
            return result, endpoint
        if result.get('error') and '429' in str(result.get('error', '')):
            continue  # try next endpoint
        # If it's a different error, still try next
        time.sleep(1)
    return result, None  # last result, all failed

def get_account_info(mint):
    params = [mint, {"encoding": "base64", "commitment": "confirmed"}]
    return rpc_with_rotation("getAccountInfo", params)

def get_holders(mint):
    """Get holder count and distribution for a mint via getProgramAccounts."""
    payload_params = [TOKEN_2022, {
        "encoding": "base64",
        "commitment": "confirmed",
        "filters": [{"memcmp": {"offset": 0, "bytes": mint}}]
    }]
    result, endpoint = rpc_with_rotation("getProgramAccounts", payload_params, timeout=60)
    if not result or not result.get('result'):
        return None, endpoint

    accounts = result['result']
    nonzero = 0
    total_amount = 0
    amounts = []

    for entry in accounts:
        if isinstance(entry, dict) and 'account' in entry:
            data_b64 = entry['account'].get('data', [None, None])[0]
            if data_b64:
                raw = base64.b64decode(data_b64)
                if len(raw) >= 72:
                    amount = int.from_bytes(raw[64:72], 'little')
                    if amount > 0:
                        nonzero += 1
                        total_amount += amount
                        amounts.append(amount)

    top10_pct = 0
    if amounts and total_amount > 0:
        sorted_amt = sorted(amounts, reverse=True)
        top_n = max(1, len(sorted_amt) // 10)
        top10_pct = sum(sorted_amt[:top_n]) / total_amount

    return {
        'holder_count': nonzero,
        'total_accounts': len(accounts),
        'total_supply': total_amount,
        'top10_concentration': round(top10_pct, 4),
        'max_holder_amount': max(amounts) if amounts else 0,
    }, endpoint

def load_existing():
    """Load already-validated mints to skip them."""
    validated = set()
    if os.path.exists(OUTPUT_FILE):
        with open(OUTPUT_FILE, 'r') as f:
            for line in f:
                if line.strip():
                    try:
                        v = json.loads(line)
                        validated.add(v['mint'])
                    except json.JSONDecodeError:
                        pass
    return validated

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--batch-size', type=int, default=5, help='Mints per batch before pause')
    parser.add_argument('--delay', type=float, default=3.0, help='Seconds between mints')
    parser.add_argument('--batch-pause', type=float, default=15.0, help='Pause between batches')
    parser.add_argument('--max-mints', type=int, default=0, help='Max mints to process (0=all)')
    parser.add_argument('--graduated-only', action='store_true', help='Only graduated mints')
    args = parser.parse_args()

    print("=" * 70)
    print("SLINKY ON-CHAIN VALIDATION (rate-limited, multi-endpoint)")
    print("=" * 70)

    with open(SLINKY_TARGETS) as f:
        targets = json.load(f)

    # Sort by MFE desc, graduated first
    targets.sort(key=lambda x: (x.get('graduated', False), x.get('mfe_bp', 0)), reverse=True)

    if args.graduated_only:
        targets = [t for t in targets if t.get('graduated')]

    # Filter to high-MFE or graduated
    targets = [t for t in targets if t.get('graduated') or t.get('mfe_bp', 0) >= 10000]

    # Skip already validated
    existing = load_existing()
    targets = [t for t in targets if t['mint'] not in existing]
    print(f"  Already validated: {len(existing)}")
    print(f"  Remaining: {len(targets)}")

    if args.max_mints > 0:
        targets = targets[:args.max_mints]
        print(f"  Processing: {len(targets)}")

    os.makedirs(OUTPUT_DIR, exist_ok=True)

    processed = 0
    for i, t in enumerate(targets):
        mint = t['mint']
        symbol = t.get('token_symbol', '?')
        mfe = t.get('mfe_bp', 0)
        time_str = t.get('time_str', '')
        graduated = t.get('graduated', False)

        # Get account info
        acct_result, ep1 = get_account_info(mint)
        onchain_exists = False
        owner = None
        lamports = 0
        metadata_uri = None

        if acct_result and acct_result.get('result', {}).get('value'):
            onchain_exists = True
            acct = acct_result['result']['value']
            owner = acct.get('owner', '')
            lamports = acct.get('lamports', 0)
            raw_data = acct.get('data', [None, None])[0]
            if raw_data:
                raw = base64.b64decode(raw_data)
                text = raw.decode('utf-8', errors='ignore')
                import re
                uris = re.findall(r'https?://[^\s\x00]+|ipfs://[^\s\x00]+', text)
                if uris:
                    metadata_uri = uris[0][:200]

        time.sleep(args.delay)

        # Get holders
        holders, ep2 = get_holders(mint)
        if holders is None:
            holders = {'holder_count': None, 'total_accounts': None,
                       'total_supply': None, 'top10_concentration': None,
                       'max_holder_amount': None}

        record = {
            'mint': mint,
            'token_symbol': symbol,
            'slinky_mfe_bp': mfe,
            'slinky_graduated': graduated,
            'slinky_time': time_str,
            'onchain_exists': onchain_exists,
            'owner': owner,
            'lamports': lamports,
            'metadata_uri': metadata_uri,
            **holders,
            'rpc_endpoints': [e for e in [ep1, ep2] if e],
            'validated_at': datetime.now().isoformat(),
        }

        with open(OUTPUT_FILE, 'a') as f:
            f.write(json.dumps(record) + '\n')
        processed += 1

        label = "GRAD" if graduated else "SURV"
        h = holders.get('holder_count', '?')
        c = holders.get('top10_concentration', 0)
        verified = "✓" if onchain_exists else "✗"
        print(f"  [{i+1}] ${symbol:12s} [{label}] {verified} holders={h} top10={c} mfe={mfe}bp")

        # Batch pause
        if (processed % args.batch_size == 0):
            print(f"  --- Batch pause ({args.batch_pause}s) ---")
            time.sleep(args.batch_pause)
        else:
            time.sleep(args.delay)

    print(f"\n  Processed: {processed}")
    print(f"  Total validated: {len(existing) + processed}")

if __name__ == '__main__':
    main()
