#!/usr/bin/env python3
"""Extract the launch (discovery) population from RAW capture part files.

Finding that motivates this tool: the normalized events file classifies almost
nothing as `create` (5-7 per 2h session) while the raw transactions plainly
contain the pump.fun create instruction. Two causes, both now handled:
  * the real instruction name is `CreateV2` (and Create / CreateFeeSharingConfig
    belong to different programs);
  * raw record fields are snake_case (`log_messages`, `post_token_balances`).

For every raw transaction whose logs contain a pump.fun create instruction,
emit: mint, creator, slot, recv_unix_ms, signature, failed.

Read-only on sources.
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

PUMP_FUN = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
CREATE_TAGS = ('Instruction: CreateV2', 'Instruction: Create')
# Mints that are ledger infrastructure, not launched tokens. A create tx that
# merely touches wSOL must not be counted as a launch.
NOT_A_LAUNCH = {
    'So11111111111111111111111111111111111111112',   # wrapped SOL
    '11111111111111111111111111111111',              # system program
    'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',   # SPL token program
    'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb',   # Token-2022 program
}


def iter_records(path: Path):
    opener = ['zstd', '-dc', str(path)] if path.suffix == '.zst' else ['cat', str(path)]
    proc = subprocess.Popen(opener, stdout=subprocess.PIPE)
    try:
        for line in proc.stdout:
            yield line
    finally:
        proc.stdout.close()
        proc.wait()


def _pk(k):
    return k.get('pubkey') if isinstance(k, dict) else k


def iter_launches(path: Path):
    for line in iter_records(path):
        if b'Create' not in line:
            continue
        try:
            o = json.loads(line)
        except Exception:
            continue
        if o.get('record_type') != 'transaction':
            continue
        p = o.get('payload') or {}
        meta = p.get('meta') or {}
        if not meta:
            continue
        logs = meta.get('log_messages') or []
        if not any(t in str(lg) for lg in logs for t in CREATE_TAGS):
            continue
        # must be the pump.fun program creating, not a fee-sharing config
        if not any(PUMP_FUN in str(lg) for lg in logs):
            continue
        msg = p.get('message') or {}
        keys = msg.get('account_keys_b58') or []
        creator = keys[0] if keys else None
        mint = None
        for tb in (meta.get('post_token_balances') or []):
            if isinstance(tb, dict) and tb.get('mint'):
                mint = tb['mint']
                break
        if not mint:
            for tb in (meta.get('pre_token_balances') or []):
                if isinstance(tb, dict) and tb.get('mint'):
                    mint = tb['mint']
                    break
        yield {
            'mint': mint,
            'creator': creator,
            'slot': o.get('slot'),
            'recv_unix_ms': o.get('recv_unix_ms'),
            'signature': p.get('signature_b58'),
            'failed': not meta.get('err_is_none', True),
        }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', type=Path, required=True)
    ap.add_argument('files', nargs='*')
    a = ap.parse_args()

    files = [Path(f) for f in a.files]
    a.out.mkdir(parents=True, exist_ok=True)
    out_jsonl = a.out / 'launches.jsonl'
    n_files = n_recs = 0
    no_mint = 0
    seen = set()
    slots = []
    with out_jsonl.open('w', encoding='utf-8') as fh:
        for f in files:
            if not f.exists():
                print(f'MISSING {f}', file=sys.stderr)
                continue
            n_files += 1
            for rec in iter_launches(f):
                if not rec['mint'] or rec['mint'] in NOT_A_LAUNCH:
                    no_mint += 1
                    continue
                if rec['mint'] in seen:
                    continue
                seen.add(rec['mint'])
                if rec['slot']:
                    slots.append(rec['slot'])
                fh.write(json.dumps(rec) + '\n')
                n_recs += 1

    summary = {
        'schema': 'north_star_discovery_launches_v1',
        'files_scanned': n_files,
        'distinct_launches': n_recs,
        'create_txs_without_mint': no_mint,
        'slot_min': min(slots) if slots else None,
        'slot_max': max(slots) if slots else None,
        'out': str(out_jsonl),
    }
    (a.out / 'DISCOVERY_SUMMARY.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
