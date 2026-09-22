#!/usr/bin/env python3
"""Enumerate every LaserStream capture session reachable on the box.

Read-only. Reports duration, event counts, venue breakdown and -- the field that
decides whether a session could support a *discovery* population -- the count of
`create` events (new token launches).
"""
import json
import os
from pathlib import Path

ROOTS = [
    Path('/mnt/data/mev_bot-artifacts/north_star/capture'),
    Path('/mnt/data/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data'),
]

rows = []
for root in ROOTS:
    if not root.exists():
        continue
    for p in sorted(root.iterdir()):
        if p.is_file():
            # loose event file, no manifest beside it
            if 'events_v1_' in p.name and p.name.endswith(('.ndjson', '.zst')):
                rows.append({'root': str(root), 'session': p.name, 'bytes': p.stat().st_size,
                             'manifest': None})
            continue
        if not p.is_dir():
            continue
        mans = list(p.glob('*manifest_v1_*.json'))
        rec = {'root': str(root), 'session': p.name,
               'bytes': sum(f.stat().st_size for f in p.rglob('*') if f.is_file()),
               'manifest': None}
        if mans:
            try:
                m = json.loads(mans[0].read_text())
                rec['manifest'] = {
                    'duration_minutes': m.get('duration_minutes'),
                    'total_events': m.get('total_events'),
                    'total_raw_records': m.get('total_raw_records'),
                    'start_unix_ms': m.get('start_unix_ms'),
                    'end_unix_ms': m.get('end_unix_ms'),
                    'counts': m.get('counts'),
                    'quality': {k: v for k, v in (m.get('quality') or {}).items()
                                if k in ('duplicates', 'decode_failures', 'unknown_events')},
                }
            except Exception as e:                                  # noqa: BLE001
                rec['manifest'] = {'error': str(e)}
        rows.append(rec)

for r in rows:
    m = r['manifest']
    print('=' * 88)
    print(f"{r['session']}  ({r['bytes']/1e9:.2f} GB)")
    if not m:
        print('   no manifest (loose file)')
        continue
    if 'error' in m:
        print('   manifest error:', m['error'])
        continue
    c = m.get('counts') or {}
    print(f"   minutes={m.get('duration_minutes')} events={m.get('total_events')} "
          f"raw={m.get('total_raw_records')}")
    print(f"   CREATE(pumpfun)={c.get('creates')} migrate={c.get('migrations')} "
          f"pump_buys={c.get('pump_buys')} pump_sells={c.get('pump_sells')} "
          f"ps_buys={c.get('pumpswap_buys')} ps_sells={c.get('pumpswap_sells')} "
          f"ps_create_pools={c.get('pumpswap_create_pools')}")

tot_create = sum((r['manifest'] or {}).get('counts', {}).get('creates', 0) or 0
                 for r in rows if r['manifest'] and 'counts' in r['manifest'])
print('=' * 88)
print('TOTAL pumpfun create events across all sessions:', tot_create)
