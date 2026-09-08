#!/usr/bin/env python
"""elite_wallet_tracker.py — profile mapped elite traders' on-chain behavior.

Reads wallet addresses from narrative_seeds_v1.yaml, joins against slinky21_data
trades, emits a per-wallet behavioral profile. This is the D06 (wallet behavior)
+ D11 (decision-shaped) substitute for the login-walled X/Twitter narrative lane:
on-chain truth > creator claims (authority hierarchy).

Output: tools/data-pipeline/output/narrative_gold_v1/elite_wallet_profiles.json
"""
import json, os, yaml, duckdb

SEEDS = 'D:/repos/mev_bot/tools/data-pipeline/schemas/narrative_seeds_v1.yaml'
TRADES = 'D:/mev_bot-artifacts/rust-data/slinky21_data/trades/trades-*.parquet'
OUT = 'D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1/elite_wallet_profiles.json'


def load_wallets():
    seeds = yaml.safe_load(open(SEEDS, encoding='utf-8'))
    out = {}
    for cname, c in seeds.get('creators', {}).items():
        for w in c.get('wallets', []) or []:
            out[w['address']] = {'creator': cname, 'name': w.get('name', ''), 'tier': c.get('tier', '')}
    for cname, c in seeds.get('additional_kols', {}).items():
        for w in c.get('wallets', []) or []:
            out[w['address']] = {'creator': cname, 'name': w.get('name', ''), 'tier': str(c.get('tier', ''))}
    return out


def main():
    wallets = load_wallets()
    con = duckdb.connect()
    wlist = "','".join(wallets.keys())
    sql = f"""
        SELECT user_wallet,
               COUNT(*) AS trades,
               SUM(CASE WHEN is_buy THEN 1 ELSE 0 END) AS buys,
               SUM(CASE WHEN is_buy THEN 0 ELSE 1 END) AS sells,
               COUNT(DISTINCT mint) AS tokens,
               ROUND(SUM(sol_amount), 4) AS sol_volume,
               ROUND(AVG(sol_amount), 4) AS avg_trade_sol,
               MIN(event_time) AS first_seen,
               MAX(event_time) AS last_seen
        FROM read_parquet('{TRADES}')
        WHERE user_wallet IN ('{wlist}')
        GROUP BY user_wallet
        ORDER BY trades DESC
    """
    rows = con.execute(sql).fetchall()
    profiles = {}
    for r in rows:
        addr = r[0]
        meta = wallets[addr]
        profiles[addr] = {
            'creator': meta['creator'], 'name': meta['name'], 'tier': meta['tier'],
            'trades': r[1], 'buys': r[2], 'sells': r[3], 'tokens': r[4],
            'sol_volume': r[5], 'avg_trade_sol': r[6],
            'buy_ratio': round(r[2] / r[1], 4) if r[1] else None,
            'first_seen': str(r[7]), 'last_seen': str(r[8]),
        }
    result = {
        'mapped_wallets': len(wallets),
        'active_wallets': len(profiles),
        'inactive_wallets': sorted(set(wallets) - set(profiles)),
        'profiles': profiles,
    }
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, 'w', encoding='utf-8') as f:
        json.dump(result, f, indent=2, ensure_ascii=False)
    print(f'mapped={len(wallets)} active={len(profiles)} inactive={len(wallets)-len(profiles)}')
    print(f'wrote {OUT}')


if __name__ == '__main__':
    main()
