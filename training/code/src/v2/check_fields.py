#!/usr/bin/env python3
"""Inspect field value distributions in a capture event sample."""
import json
import sys
from collections import Counter

path = sys.argv[1] if len(sys.argv) > 1 else '/tmp/ev_sample.ndjson'
c = Counter()
amt = Counter()
sides = Counter()
n = 0
for line in open(path):
    try:
        o = json.loads(line)
    except Exception:
        continue
    n += 1
    sides[str(o.get('trade_side'))] += 1
    ai, ao = o.get('amount_in'), o.get('amount_out')
    ok = ai is not None and ao is not None
    c[ok] += 1
    amt[(str(o.get('venue')), str(o.get('event_type')), ok)] += 1
print('records:', n)
print('trade_side values:', dict(sides.most_common(10)))
print('both amounts present:', dict(c))
print('(venue,event_type,has_both):', dict(amt.most_common(10)))
