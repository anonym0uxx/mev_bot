#!/usr/bin/env python3
"""Quality summary of the recovered launch population."""
import json
import sys
from collections import Counter
from datetime import datetime, timezone

path = sys.argv[1] if len(sys.argv) > 1 else \
    '/training/v2/canonical/discovery_raw/launches.jsonl'
rows = [json.loads(l) for l in open(path) if l.strip()]
print('launches:', len(rows))
print('distinct mints:', len({r['mint'] for r in rows}))
print('distinct creators:', len({r['creator'] for r in rows}))
print('failed txs:', sum(1 for r in rows if r['failed']))
suf = Counter('pump' if (r['mint'] or '').endswith('pump') else 'other' for r in rows)
print('mint suffix:', dict(suf))
ts = [r['recv_unix_ms'] for r in rows if r.get('recv_unix_ms')]
if ts:
    print('time span UTC:', datetime.fromtimestamp(min(ts) / 1000, timezone.utc).isoformat(),
          '->', datetime.fromtimestamp(max(ts) / 1000, timezone.utc).isoformat())
    span_h = (max(ts) - min(ts)) / 3.6e6
    print(f'span_hours: {span_h:.2f}  launches/hour: {len(rows)/span_h:.0f}')
slots = [r['slot'] for r in rows if r.get('slot')]
if slots:
    print('slot range:', min(slots), max(slots))
print('top creators:', Counter(r['creator'] for r in rows).most_common(5))
dupe_sig = Counter(r['signature'] for r in rows)
print('duplicate signatures:', sum(1 for v in dupe_sig.values() if v > 1))
