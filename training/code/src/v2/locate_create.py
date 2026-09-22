#!/usr/bin/env python3
"""Locate where 'Instruction: Create' appears inside a raw transaction record."""
import json
import subprocess
import sys
from pathlib import Path

f = Path(sys.argv[1])
proc = subprocess.Popen(['zstd', '-dc', str(f)], stdout=subprocess.PIPE)
shown = 0
for line in proc.stdout:
    if b'Instruction: Create' not in line:
        continue
    o = json.loads(line)
    p = o.get('payload') or {}
    print('=' * 76)
    print('record_type:', o.get('record_type'), '| slot:', o.get('slot'))
    print('payload top keys:', list(p))
    for k in ('is_vote', 'tx_index'):
        print(f'  {k} = {p.get(k)}')
    msg = p.get('message')
    meta = p.get('meta')
    print('  message type:', type(msg).__name__, '| meta type:', type(meta).__name__)
    if isinstance(msg, dict):
        print('  message keys:', list(msg))
        keys = msg.get('accountKeys')
        print('  n accountKeys:', len(keys) if isinstance(keys, list) else keys)
        if isinstance(keys, list) and keys:
            print('  key[0]:', json.dumps(keys[0])[:200])
    if isinstance(meta, dict):
        print('  meta keys:', list(meta))
        logs = meta.get('logMessages')
        print('  n logMessages:', len(logs) if isinstance(logs, list) else logs)
        pbt = meta.get('postTokenBalances')
        print('  postTokenBalances:', json.dumps(pbt)[:400] if pbt else pbt)
    # where does the string actually live?
    blob = json.dumps(o)
    i = blob.find('Instruction: Create')
    print('  context:', blob[max(0, i - 220):i + 60])
    shown += 1
    if shown >= 2:
        break
proc.stdout.close()
proc.wait()
