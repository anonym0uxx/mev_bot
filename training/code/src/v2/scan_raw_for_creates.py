#!/usr/bin/env python3
"""Scan RAW capture part files for pump.fun Create instructions and mint breadth.

Decides whether the discovery population exists in raw form even though the
normalized events file classified almost nothing as `create`.
"""
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path

PUMP_FUN = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'

files = [Path(p) for p in sys.argv[1:]]
for f in files:
    opener = ['zstd', '-dc', str(f)] if f.suffix == '.zst' else ['cat', str(f)]
    proc = subprocess.Popen(opener, stdout=subprocess.PIPE)
    types = Counter()
    instr = Counter()
    mints = set()
    creators = set()
    n = 0
    tx_n = 0
    for line in proc.stdout:
        try:
            o = json.loads(line)
        except Exception:
            continue
        n += 1
        rt = o.get('record_type')
        types[rt] += 1
        if rt != 'transaction':
            continue
        tx_n += 1
        p = o.get('payload') or {}
        blob = json.dumps(p)
        for tag in ('Instruction: Create', 'Instruction: Buy', 'Instruction: Sell',
                    'Instruction: CreatePool', 'Instruction: Migrate'):
            if tag in blob:
                instr[tag] += 1
        # collect pubkeys mentioned
        for key in ('account_keys_b58', 'accounts_b58', 'pubkeys_b58'):
            v = p.get(key)
            if isinstance(v, list):
                for a in v:
                    if isinstance(a, str) and a.endswith('pump') and len(a) > 30:
                        mints.add(a)
    proc.stdout.close()
    proc.wait()
    print('=' * 80)
    print(f'{f.name}: records={n} transactions={tx_n}')
    print('  record types:', dict(types.most_common(6)))
    print('  instruction tags:', dict(instr.most_common(10)))
    print('  mint-like pubkeys collected:', len(mints))
