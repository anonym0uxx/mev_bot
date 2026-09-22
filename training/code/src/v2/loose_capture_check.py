#!/usr/bin/env python3
"""Count event types (esp. `create`) in loose capture event files."""
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path

D = Path('/mnt/data/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data')
files = sys.argv[1:] or [str(p) for p in sorted(D.glob('*.ndjson*'))]

for f in files:
    p = Path(f)
    if not p.exists():
        print(f'{p.name}: MISSING')
        continue
    opener = ['zstd', '-dc', str(p)] if p.suffix == '.zst' else ['cat', str(p)]
    proc = subprocess.Popen(opener, stdout=subprocess.PIPE)
    c = Counter()
    mints = set()
    traders = set()
    n = 0
    crea = []
    for line in proc.stdout:
        try:
            o = json.loads(line)
        except Exception:
            continue
        n += 1
        c[f"{o.get('venue')}:{o.get('event_type')}"] += 1
        if o.get('mint_b58'):
            mints.add(o['mint_b58'])
        if o.get('trader_b58'):
            traders.add(o['trader_b58'])
        if o.get('event_type') == 'create' and len(crea) < 3:
            crea.append({k: o.get(k) for k in ('mint_b58', 'creator_b58', 'slot', 'recv_unix_ms')})
    proc.stdout.close()
    proc.wait()
    print('=' * 80)
    print(f'{p.name}  records={n}  distinct_mints={len(mints)}  distinct_traders={len(traders)}')
    print('  types:', dict(c.most_common(8)))
    if crea:
        print('  sample creates:', json.dumps(crea))
