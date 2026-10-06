#!/usr/bin/env python3
"""Census of a raw pump.fun LaserStream capture.

Decides whether a capture is a *universe* capture (many mints observed shallowly,
usable to sample an as-of discoverable universe and therefore untraded
opportunities) or a *mint-scoped* capture (a few mints observed deeply, which
cannot supply negatives).

Read-only: streams the .zst, never writes to the source.

  python3 capture_census.py --session /mnt/data/.../capture/<session> \
      --out /training/v2/reports
"""
import argparse
import json
import subprocess
from collections import Counter, defaultdict
from pathlib import Path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--session', type=Path, required=True)
    ap.add_argument('--out', type=Path, default=Path('/training/v2/reports'))
    ap.add_argument('--max-events', type=int, default=0, help='0 = all')
    a = ap.parse_args()

    events_file = next(a.session.glob('*events_v1_*.ndjson.zst'))
    manifest = json.loads(next(a.session.glob('*manifest_v1_*.json')).read_text())

    by_type = Counter()
    by_venue = Counter()
    by_type_venue = Counter()
    mints = Counter()
    traders = set()
    creators = set()
    pool_mints = set()
    curve_mints = set()
    t_min, t_max = None, None
    n = 0

    proc = subprocess.Popen(['zstd', '-dc', str(events_file)], stdout=subprocess.PIPE)
    for line in proc.stdout:
        line = line.strip()
        if not line:
            continue
        try:
            o = json.loads(line)
        except json.JSONDecodeError:
            continue
        n += 1
        et = o.get('event_type')
        vn = o.get('venue')
        by_type[et] += 1
        by_venue[vn] += 1
        by_type_venue[f'{vn}:{et}'] += 1
        m = o.get('mint_b58')
        if m:
            mints[m] += 1
        tr = o.get('trader_b58')
        if tr:
            traders.add(tr)
        cr = o.get('creator_b58')
        if cr:
            creators.add(cr)
        if o.get('pool_account_b58'):
            pool_mints.add(m)
        if o.get('curve_account_b58'):
            curve_mints.add(m)
        ts = o.get('recv_unix_ms')
        if ts:
            t_min = ts if t_min is None else min(t_min, ts)
            t_max = ts if t_max is None else max(t_max, ts)
        if a.max_events and n >= a.max_events:
            break
    proc.stdout.close()
    proc.wait()

    counts = sorted(mints.values(), reverse=True)
    total_mint_events = sum(counts)

    def q(p):
        if not counts:
            return None
        return counts[min(len(counts) - 1, int(len(counts) * p))]

    report = {
        'schema': 'north_star_capture_census_v1',
        'session': a.session.name,
        'events_file': events_file.name,
        'events_read': n,
        'manifest_total_events': manifest.get('total_events'),
        'programs': manifest.get('programs'),
        'duration_minutes': manifest.get('duration_minutes'),
        'by_event_type': dict(by_type.most_common()),
        'by_venue': dict(by_venue.most_common()),
        'by_type_venue': dict(by_type_venue.most_common(30)),
        'distinct_mints': len(mints),
        'distinct_traders': len(traders),
        'distinct_creators': len(creators),
        'mints_with_pool_account': len(pool_mints),
        'mints_with_curve_account': len(curve_mints),
        'mint_event_count_quantiles': {
            'max': counts[0] if counts else 0,
            'p50': q(0.50), 'p90': q(0.90), 'p99': q(0.99),
            'min': counts[-1] if counts else 0,
        },
        'mints_with_1_event': sum(1 for c in counts if c == 1),
        'mints_with_le_5_events': sum(1 for c in counts if c <= 5),
        'total_mint_events': total_mint_events,
        'time_span_ms': [t_min, t_max],
        'time_span_utc': [__import__('datetime').datetime.utcfromtimestamp(t_min / 1000).isoformat() + 'Z' if t_min else None,
                          __import__('datetime').datetime.utcfromtimestamp(t_max / 1000).isoformat() + 'Z' if t_max else None],
        'quality': manifest.get('quality', {}),
        'verdict': ('universe_capture' if len(mints) > 500 and
                    sum(1 for c in counts if c <= 5) / max(len(counts), 1) > 0.5
                    else 'mint_scoped_or_partial'),
    }
    a.out.mkdir(parents=True, exist_ok=True)
    out = a.out / f'CAPTURE_CENSUS_{a.session.name}.json'
    out.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')

    print(json.dumps({k: report[k] for k in (
        'session', 'events_read', 'manifest_total_events', 'distinct_mints', 'distinct_traders',
        'mints_with_pool_account', 'mints_with_curve_account', 'mint_event_count_quantiles',
        'mints_with_1_event', 'mints_with_le_5_events', 'time_span_utc', 'verdict')}, indent=2))
    print('by_type_venue:', json.dumps(dict(by_type_venue.most_common(12))))


if __name__ == '__main__':
    main()
