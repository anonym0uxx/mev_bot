#!/usr/bin/env python3
"""Print the real pump.fun instruction discriminators seen in a CreateV2 tx."""
import base64
import json
import subprocess
import sys
from pathlib import Path

PUMP_FUN = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
_B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'


def b58decode(s):
    n = 0
    for ch in s:
        n = n * 58 + _B58.index(ch)
    pad = len(s) - len(s.lstrip('1'))
    body = n.to_bytes((n.bit_length() + 7) // 8, 'big') if n else b''
    return b'\x00' * pad + body


f = Path(sys.argv[1])
proc = subprocess.Popen(['zstd', '-dc', str(f)],
                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
shown = 0
for line in proc.stdout:
    if b'CreateV2' not in line:
        continue
    o = json.loads(line)
    p = o.get('payload') or {}
    msg = p.get('message') or {}
    meta = p.get('meta') or {}
    keys = list(msg.get('account_keys_b58') or [])
    keys += list(meta.get('loaded_writable_addresses_b58') or [])
    keys += list(meta.get('loaded_readonly_addresses_b58') or [])
    print('--- tx', p.get('signature_b58', '')[:20], 'keys:', len(keys))
    ixs = msg.get('instructions') or []
    print('    n_instructions:', len(ixs))
    if ixs:
        print('    instruction[0] keys:', list(ixs[0].keys()))
    for ix in ixs:
        pi = ix.get('program_id_index')
        if pi is None or pi >= len(keys) or keys[pi] != PUMP_FUN:
            continue
        raw = None
        d = ix.get('data_b64') or ix.get('data_b58') or ix.get('data')
        if d:
            try:
                raw = base64.b64decode(d)
            except Exception:
                try:
                    raw = b58decode(d)
                except Exception:
                    raw = None
        if not raw or len(raw) < 8:
            continue
        tag = [l.split('Instruction:')[-1].strip()
               for l in (meta.get('log_messages') or []) if 'Instruction:' in l]
        print('    pumpfun ix disc:', list(raw[:8]), 'len:', len(raw), 'first_tags:', tag[:3])
    shown += 1
    if shown >= 3:
        break
proc.stdout.close()
proc.wait()
