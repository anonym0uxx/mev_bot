#!/usr/bin/env python
"""
validate_slinky_parallel.py - Multi-instance parallel on-chain validation.

Uses proxy rotation + multiple RPC endpoints to validate Slinky mints
in parallel, avoiding rate limits.

Usage:
    # Launch instance 0 (processes mints 0-100)
    python src/validate_slinky_parallel.py --instance 0 --total-instances 5 --max-mints 281
    
    # Launch all instances via the wrapper script
    python src/validate_slinky_parallel.py --launch-all --instances 5 --max-mints 281
"""

import json
import os
import sys
import time
import base64
import argparse
import random
import re
from datetime import datetime

# Add parent dir to path
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from proxy_pool import ProxyRPC

TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"

SLINKY_TARGETS = "output/narrative_gold_v1/slinky_target_mints_enriched.json"
OUTPUT_DIR = "output/narrative_gold_v1/gold/onchain_validation"
OUTPUT_FILE = os.path.join(OUTPUT_DIR, "slinky_onchain_validation_parallel.jsonl")
LOCK_FILE = os.path.join(OUTPUT_DIR, ".validation_lock")


def load_targets(graduated_only=False, min_mfe=0):
    with open(SLINKY_TARGETS) as f:
        targets = json.load(f)
    # Sort by MFE desc, graduated first
    targets.sort(key=lambda x: (x.get('graduated', False), x.get('mfe_bp', 0)), reverse=True)
    if graduated_only:
        targets = [t for t in targets if t.get('graduated')]
    if min_mfe > 0:
        targets = [t for t in targets if t.get('graduated') or t.get('mfe_bp', 0) >= min_mfe]
    return targets


def load_validated():
    """Load set of already-validated mints from the output file."""
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


def append_result(record):
    """Thread-safe append to output file."""
    # Simple file locking via atomic append
    with open(OUTPUT_FILE, 'a') as f:
        f.write(json.dumps(record) + '\n')


def validate_mint(rpc_client, mint, symbol, mfe, graduated, time_str):
    """Validate a single mint on-chain using proxy rotation."""
    record = {
        'mint': mint,
        'token_symbol': symbol,
        'slinky_mfe_bp': mfe,
        'slinky_graduated': graduated,
        'slinky_time': time_str,
        'onchain_exists': False,
        'owner': None,
        'lamports': 0,
        'metadata_uri': None,
        'holder_count': None,
        'total_accounts': None,
        'total_supply': None,
        'top10_concentration': None,
        'max_holder_amount': None,
        'rpc_source': None,
        'validated_at': datetime.now().isoformat(),
    }

    # 1. Get account info
    result, source = rpc_client.post("getAccountInfo", 
        [mint, {"encoding": "base64", "commitment": "confirmed"}], timeout=15)
    
    if result and result.get('result', {}).get('value'):
        record['onchain_exists'] = True
        acct = result['result']['value']
        record['owner'] = acct.get('owner', '')
        record['lamports'] = acct.get('lamports', 0)
        record['rpc_source'] = source.get('via', 'direct')
        if source.get('proxy'):
            record['rpc_source'] += f":{source['proxy'][:20]}"
        
        # Extract metadata URI
        raw_data = acct.get('data', [None, None])[0]
        if raw_data:
            try:
                raw = base64.b64decode(raw_data)
                text = raw.decode('utf-8', errors='ignore')
                uris = re.findall(r'https?://[^\s\x00]+|ipfs://[^\s\x00]+', text)
                if uris:
                    record['metadata_uri'] = uris[0][:200]
            except:
                pass
    else:
        record['errors'] = f"account_info failed: {source.get('error', 'unknown')}"
        return record

    # 2. Get holders via getProgramAccounts
    params = [TOKEN_2022, {
        "encoding": "base64",
        "commitment": "confirmed",
        "filters": [{"memcmp": {"offset": 0, "bytes": mint}}]
    }]
    result, source = rpc_client.post("getProgramAccounts", params, timeout=60)
    
    if result and result.get('result') is not None:
        accounts = result['result']
        nonzero = 0
        total_amount = 0
        amounts = []
        
        for entry in accounts:
            if isinstance(entry, dict) and 'account' in entry:
                data_b64 = entry['account'].get('data', [None, None])[0]
                if data_b64:
                    try:
                        raw = base64.b64decode(data_b64)
                        if len(raw) >= 72:
                            amount = int.from_bytes(raw[64:72], 'little')
                            if amount > 0:
                                nonzero += 1
                                total_amount += amount
                                amounts.append(amount)
                    except:
                        pass
        
        record['holder_count'] = nonzero
        record['total_accounts'] = len(accounts)
        record['total_supply'] = total_amount
        
        if amounts and total_amount > 0:
            sorted_amt = sorted(amounts, reverse=True)
            top_n = max(1, len(sorted_amt) // 10)
            record['top10_concentration'] = round(sum(sorted_amt[:top_n]) / total_amount, 4)
            record['max_holder_amount'] = max(amounts)
    else:
        record['holder_errors'] = f"holders failed: {source.get('error', 'unknown')}"
    
    return record


def run_instance(instance_id, total_instances, max_mints, delay, graduated_only):
    """Run a single validation instance processing a subset of mints."""
    print(f"[Instance {instance_id}] Starting...")
    
    rpc = ProxyRPC(max_proxies=100)
    
    targets = load_targets(graduated_only=graduated_only, min_mfe=10000)
    validated = load_validated()
    
    # Partition targets across instances
    # Each instance processes every Nth mint (stride partitioning)
    stride = total_instances
    my_targets = [t for i, t in enumerate(targets) if i % stride == instance_id]
    my_targets = [t for t in my_targets if t['mint'] not in validated]
    
    if max_mints > 0:
        my_targets = my_targets[:max_mints]
    
    print(f"[Instance {instance_id}] Targets: {len(my_targets)} (of {len(targets)} total, stride {stride})")
    
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    
    processed = 0
    for i, t in enumerate(my_targets):
        mint = t['mint']
        symbol = t.get('token_symbol', '?')
        mfe = t.get('mfe_bp', 0)
        time_str = t.get('time_str', '')
        graduated = t.get('graduated', False)
        
        # Check if already validated (by another instance)
        current_validated = load_validated()
        if mint in current_validated:
            continue
        
        record = validate_mint(rpc, mint, symbol, mfe, graduated, time_str)
        append_result(record)
        processed += 1
        
        verified = "✓" if record.get('onchain_exists') else "✗"
        h = record.get('holder_count', '?')
        c = record.get('top10_concentration', 0)
        src = record.get('rpc_source', '?')
        label = "GRAD" if graduated else "SURV"
        
        print(f"[Instance {instance_id}] [{processed}] ${symbol:12s} [{label}] "
              f"{verified} holders={h} top10={c} mfe={mfe}bp src={src}")
        
        # Delay between mints (stagger across instances)
        time.sleep(delay + random.uniform(0, delay))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--instance', type=int, default=0, help='Instance ID (0-based)')
    parser.add_argument('--total-instances', type=int, default=1, help='Total instances')
    parser.add_argument('--max-mints', type=int, default=0, help='Max mints per instance')
    parser.add_argument('--delay', type=float, default=2.0, help='Base delay between mints (seconds)')
    parser.add_argument('--graduated-only', action='store_true', help='Only graduated mints')
    parser.add_argument('--launch-all', action='store_true', help='Launch all instances in parallel')
    parser.add_argument('--instances', type=int, default=5, help='Number of parallel instances (with --launch-all)')
    args = parser.parse_args()

    if args.launch_all:
        import subprocess
        print(f"Launching {args.instances} parallel instances...")
        procs = []
        for i in range(args.instances):
            cmd = (f"python src/validate_slinky_parallel.py "
                   f"--instance {i} --total-instances {args.instances} "
                   f"--max-mints {args.max_mints} --delay {args.delay} "
                   f"{'--graduated-only' if args.graduated_only else ''}")
            log_file = f"output/onchain_validation_instance{i}.log"
            p = subprocess.Popen(
                ['python', 'src/validate_slinky_parallel.py',
                 '--instance', str(i), '--total-instances', str(args.instances),
                 '--max-mints', str(args.max_mints), '--delay', str(args.delay)]
                + (['--graduated-only'] if args.graduated_only else []),
                stdout=open(f"D:/repos/mev_bot/tools/data-pipeline/{log_file}", 'w'),
                stderr=subprocess.STDOUT,
                cwd="D:/repos/mev_bot/tools/data-pipeline"
            )
            procs.append(p)
            print(f"  Instance {i}: PID {p.pid}")
            time.sleep(2)  # stagger start
        
        print(f"\nLaunched {len(procs)} instances. Monitoring...")
        for p in procs:
            p.wait()
        print("All instances done.")
        
        # Show summary
        validated = load_validated()
        print(f"\n  Total validated: {len(validated)}")
    else:
        run_instance(args.instance, args.total_instances, args.max_mints, args.delay, args.graduated_only)


if __name__ == '__main__':
    main()
