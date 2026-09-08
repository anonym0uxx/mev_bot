#!/usr/bin/env python
"""wayback_x_archive.py — fetch archived X/twitter snapshots for mapped handles.

Uses the Wayback Machine CDX API (web.archive.org) — a public, legal archive of
publicly-published web content. No login, no account, no ToS evasion. This is the
legitimate substitute for the login-walled live X feed: historical "what the elite
trader actually said," keyed by timestamp for ex-ante/retrospective split.

CDX query: url=x.com/<handle> (and twitter.com/<handle>), filter status:200,
collapse urlkey to dedupe. Returns snapshot inventory (not content) — content fetch
is a follow-on scrape of the archived snapshot pages.
"""
import json, time, urllib.request, urllib.parse, os
import yaml

SEEDS = 'D:/repos/mev_bot/tools/data-pipeline/schemas/narrative_seeds_v1.yaml'
OUT = 'D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1/wayback_x_index.json'
CDX = 'http://web.archive.org/cdx/search/cdx'


def collect_handles():
    seeds = yaml.safe_load(open(SEEDS, encoding='utf-8'))
    handles = {}
    for cname, c in seeds.get('creators', {}).items():
        h = c.get('x_handle')
        if h:
            handles[h] = {'creator': cname, 'tier': c.get('tier', '')}
    for cname, c in seeds.get('additional_kols', {}).items():
        h = c.get('x_handle')
        if h:
            handles[h] = {'creator': cname, 'tier': str(c.get('tier', ''))}
    for a in seeds.get('x_amplifiers', []):
        h = a.get('handle')
        if h:
            handles.setdefault(h, {'creator': a.get('alias', 'amplifier'), 'tier': a.get('tier', '')})
    return handles


def cdx_count(handle):
    """Return snapshot counts for a handle across x.com and twitter.com, and earliest/latest ts."""
    results = {'x_com': 0, 'twitter_com': 0, 'earliest': None, 'latest': None, 'urls': []}
    for domain in ('x.com', 'twitter.com'):
        url = f'{domain}/{handle}'
        params = {
            'url': url,
            'output': 'json',
            'filter': 'statuscode:200',
            'collapse': 'timestamp:6',  # dedupe by ts precision
            'fl': 'timestamp,original',
            'limit': '2000',
        }
        qs = urllib.parse.urlencode(params)
        req = urllib.request.Request(f'{CDX}?{qs}', headers={'User-Agent': 'mev-bot-northstar/1.0'})
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                data = json.loads(resp.read().decode())
            rows = data[1:]  # skip header
            results['urls'].extend([{'ts': r[0], 'url': r[1]} for r in rows])
            if rows:
                ts = [r[0] for r in rows]
                if results['earliest'] is None or min(ts) < results['earliest']:
                    results['earliest'] = min(ts)
                if results['latest'] is None or max(ts) > results['latest']:
                    results['latest'] = max(ts)
            n = len(rows)
        except Exception as e:
            n = 0
            results['urls'].append({'error': str(e)[:80], 'domain': domain})
        results['x_com' if domain == 'x.com' else 'twitter_com'] = n
        time.sleep(0.5)  # polite
    results['total'] = len(results['urls'])
    return results


def main():
    handles = collect_handles()
    out = {}
    for handle, meta in handles.items():
        r = cdx_count(handle)
        r.update(meta)
        out[handle] = r
        print(f'{handle:20s} x={r["x_com"]:4d} tw={r["twitter_com"]:4d} '
              f'earliest={r["earliest"]} latest={r["latest"]}')
        time.sleep(0.3)
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, 'w', encoding='utf-8') as f:
        json.dump({'handles': len(out), 'data': out}, f, indent=2, ensure_ascii=False)
    print(f'wrote {OUT}')


if __name__ == '__main__':
    main()