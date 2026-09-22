#!/usr/bin/env python3
"""Inspect the shape of RAW capture records vs normalized events."""
import json
import sys
from collections import Counter

path = sys.argv[1] if len(sys.argv) > 1 else '/tmp/raw_sample.ndjson'
rows = [json.loads(l) for l in open(path) if l.strip()]
print('records:', len(rows))
print('top-level keys seen:', Counter(tuple(sorted(r.keys())) for r in rows).most_common(3))
kinds = Counter()
for r in rows:
    kinds[str(r.get('kind') or r.get('type') or r.get('record_type'))] += 1
print('kind/type distribution:', dict(kinds.most_common(12)))
print()
print('--- one full sample record (truncated) ---')
print(json.dumps(rows[0])[:1400])
