#!/usr/bin/env python3
"""Derive the exact pump.fun instruction discriminators from raw capture data.

Also prints sha256("global:<name>")[..8] candidates so the normalizer table can
be checked against ground truth rather than guessed.
"""
import base64
import hashlib
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path

PUMP_FUN = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'


_B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'


def b58decode(s: str) -> bytes:
    """Minimal base58 decode (no third-party dependency)."""
    n = 0
    for ch in s:
        n = n * 58 + _B58.index(ch)
    pad = len(s) - len(s.lstrip('1'))
    body = n.to_bytes((n.bit_length() + 7) // 8, 'big') if n else b''
    return b'\x00' * pad + body


def candidates():
    out = {}
    for name in ('create', 'create_v2', 'createv2', 'CreateV2',
                 'buy', 'sell', 'migrate', 'complete'):
        d = hashlib.sha256(f'global:{name}'.encode()).digest()[:8]
        out[name] = list(d)
    return out


def main():
    print('sha256("global:<name>")[..8] candidates:')
    for k, v in candidates().items():
        print(f'  {k:12s} {v}')

    f = Path(sys.argv[1])
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else 60000
    proc = subprocess.Popen(['zstd', '-dc', str(f)],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    seen = Counter()
    examples = {}
    n = 0
    for line in proc.stdout:
        if b'CreateV2' not in line and b'Instruction:' not in line:
            continue
        n += 1
        if n > limit:
            break
        o = json.loads(line)
        if o.get('record_type') != 'transaction':
            continue
        p = o.get('payload') or {}
        meta = p.get('meta') or {}
        msg = p.get('message') or {}
        logs = meta.get('log_messages') or []
        if not any('Instruction:' in str(l) for l in logs):
            continue
        keys = msg.get('account_keys_b58') or []
        for ix in (msg.get('instructions') or []):
            pi = ix.get('program_id_index')
            if pi is None or pi >= len(keys) or keys[pi] != PUMP_FUN:
                continue
            data_b58 = ix.get('data_b58') or ix.get('data')
            raw = None
            if data_b58:
                try:
                    raw = b58decode(data_b58)
                except Exception:
                    raw = None
            if not raw or len(raw) < 8:
                continue
            disc = list(raw[:8])
            tag = None
            for l in logs:
                if 'Instruction:' in l:
                    tag = l.split('Instruction:')[-1].strip()
            key = (tag, tuple(disc))
            seen[key] += 1
            examples.setdefault(key, str(data_b58)[:24])
    proc.stdout.close()
    proc.wait()

    print('\nobserved (log tag, discriminator) -> count:')
    for (tag, disc), c in seen.most_common(20):
        print(f'  {tag!s:28s} {list(disc)}  x{c}  data[:24]={examples[(tag, disc)]}')


if __name__ == '__main__':
    main()
