#!/usr/bin/env python3
"""Tally the discriminators ACTUALLY emitted by each program in the capture.

The DISC table gives pumpswap the same buy/sell discriminators as pump.fun. If
that is wrong, side is misassigned on the majority of the stream and the sign
test rejects swaps that are perfectly resolvable.
"""
import base64
import io
import json
import subprocess
import sys
from collections import Counter

PUMP = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
SWAP = 'pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA'


def main(parts, per_file=1500):
    tally = Counter()
    for part in parts:
        p = subprocess.Popen(['zstd', '-dc', part], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL)
        n = 0
        for line in io.TextIOWrapper(p.stdout, encoding='utf-8', errors='replace'):
            n += 1
            if n > per_file:
                break
            try:
                o = json.loads(line)
            except Exception:
                continue
            if o.get('record_type') != 'transaction':
                continue
            pl = o.get('payload') or {}
            msg = pl.get('message') or {}
            meta = pl.get('meta') or {}
            keys = (list(msg.get('account_keys_b58') or [])
                    + list(meta.get('loaded_writable_addresses_b58') or [])
                    + list(meta.get('loaded_readonly_addresses_b58') or []))
            for ix in (msg.get('instructions') or []):
                pi = ix.get('program_id_index')
                if pi is None or pi >= len(keys) or keys[pi] not in (PUMP, SWAP):
                    continue
                d = ix.get('data_b64')
                if not d:
                    continue
                try:
                    raw = base64.b64decode(d)
                except Exception:
                    continue
                prog = 'pumpfun' if keys[pi] == PUMP else 'pumpswap'
                tally[(prog, tuple(raw[:8]), len(raw))] += 1

    for prog in ('pumpfun', 'pumpswap'):
        print(f'--- {prog} ---')
        rows = [(k, c) for k, c in tally.items() if k[0] == prog]
        rows.sort(key=lambda r: -r[1])
        for (pr, disc, ln), c in rows[:10]:
            print(f'  {list(disc)!s:46s} len={ln:4d}  {c:7d}')
        print(f'  distinct: {len(rows)}')


if __name__ == '__main__':
    main(sys.argv[1:])
