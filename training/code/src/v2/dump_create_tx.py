#!/usr/bin/env python3
"""Dump the payload shape of a raw transaction containing a Create instruction."""
import json
import subprocess
import sys
from pathlib import Path

f = Path(sys.argv[1])
limit = int(sys.argv[2]) if len(sys.argv) > 2 else 3
proc = subprocess.Popen(['zstd', '-dc', str(f)], stdout=subprocess.PIPE)
found = 0
for line in proc.stdout:
    if b'Instruction: Create' not in line:
        continue
    o = json.loads(line)
    if o.get('record_type') != 'transaction':
        continue
    p = o.get('payload') or {}
    print('=' * 78)
    print('payload keys:', list(p))
    for k in ('signature_b58', 'slot', 'err', 'fee_lamports'):
        print(f'  {k} = {p.get(k)}')
    ak = p.get('account_keys_b58') or p.get('account_keys') or []
    print('  n account_keys:', len(ak))
    print('  accounts:', ak[:14])
    for k in ('pre_token_balances_json', 'post_token_balances_json'):
        v = p.get(k)
        if v:
            print(f'  {k}: {str(v)[:300]}')
    logs = p.get('log_messages') or []
    for lg in logs[:16]:
        print('   log:', str(lg)[:120])
    found += 1
    if found >= limit:
        break
proc.stdout.close()
proc.wait()
