#!/usr/bin/env python
"""kol_decision_extract.py — derive D11 decision trajectories from KOL on-chain trades.

Substitutes for missing human decision history: instead of "I bought X", we read the
KOL's *actual* on-chain buy/sell decisions from slinky21 trades (user_wallet), grouped
and aggregated by coin.

Targets (per operator): Ansem (real wallets only), Cupsey, Cented, Megga.

Output:
  output/kol_decisions/kol_decisions.jsonl  — one record per (kol, mint): buy/sell sequence
  output/kol_decisions/kol_coin_consensus.json — per-mint KOL aggregation (consensus signal)
"""
import os, json, yaml
import duckdb
import pandas as pd

SEEDS = 'D:/repos/mev_bot/tools/data-pipeline/schemas/narrative_seeds_v1.yaml'
TRADES = 'D:/mev_bot-artifacts/rust-data/slinky21_data/trades/trades-*.parquet'
OUT = 'D:/repos/mev_bot/tools/data-pipeline/output/kol_decisions'

# KOLs + which of their seed-registry wallets to use (drop 'fake' ansem).
TARGETS = {
    'insentos': ['Insentos'],
    'ansem':  ['ansem', 'Ansem'],            # exclude 'Fake Ansem' / 'fake ansem'
    'cupsey': ['Cupsey', 'cupsey multi', 'Cupsey 1', 'Cupsey 2', 'Cupsey 3'],
    'cented': ['Cented', 'cented', "Cented's Friend", 'Cented dev'],
    'megga':  ['megga', 'Megga side'],
}


def load_wallets():
    s = yaml.safe_load(open(SEEDS, encoding='utf-8'))
    creators = s.get('creators', {})
    kol_wallets = {}  # kol -> list of addresses
    addr_to_kol = {}
    for kol, names in TARGETS.items():
        c = creators.get(kol)
        if not c:
            continue
        addrs = []
        for w in c.get('wallets', []):
            if isinstance(w, dict) and w.get('name') in names:
                a = w.get('address')
                if a:
                    addrs.append(a)
                    addr_to_kol[a] = kol
        kol_wallets[kol] = addrs
    return kol_wallets, addr_to_kol


def main():
    kol_wallets, addr_to_kol = load_wallets()
    all_addrs = [a for addrs in kol_wallets.values() for a in addrs]
    print(f'KOL wallets: ' + ', '.join(f'{k}={len(v)}' for k, v in kol_wallets.items()))
    print(f'total wallets: {len(all_addrs)}')

    con = duckdb.connect()
    addr_list = "','".join(all_addrs)
    df = con.execute(
        f"SELECT user_wallet, mint, is_buy, sol_amount, token_amount, market_cap_sol, "
        f"price_sol, event_time, seconds_since_launch "
        f"FROM read_parquet('{TRADES}') WHERE user_wallet IN ('{addr_list}')"
    ).fetchdf()
    print(f'KOL trades in slinky21 window: {len(df)}')

    df['kol'] = df['user_wallet'].map(addr_to_kol)

    # normalize event_time to epoch seconds (duckdb returns datetime64)
    df['event_time_s'] = (pd.to_datetime(df['event_time']).astype('int64') // 10**9).astype('int64')

    os.makedirs(OUT, exist_ok=True)
    decisions = []
    coin_kols = {}
    for (kol, mint), grp in df.groupby(['kol', 'mint']):
        g = grp.sort_values('event_time_s')
        buys = g[g['is_buy']]
        sells = g[~g['is_buy']]
        rec = {
            'kol': kol,
            'mint': mint,
            'n_buys': int(len(buys)),
            'n_sells': int(len(sells)),
            'buy_sol': float(buys['sol_amount'].sum()),
            'sell_sol': float(sells['sol_amount'].sum()),
            'first_trade_s': int(g['event_time_s'].min()) if len(g) else None,
            'last_trade_s': int(g['event_time_s'].max()) if len(g) else None,
            'entry_mcap_sol': float(g['market_cap_sol'].iloc[0]) if len(g) else None,
            'min_price_sol': float(g['price_sol'].min()) if len(g) else None,
            'max_price_sol': float(g['price_sol'].max()) if len(g) else None,
            'avg_seconds_since_launch': float(g['seconds_since_launch'].mean()) if len(g) else None,
        }
        decisions.append(rec)
        coin_kols.setdefault(mint, set()).add(kol)

    # write per-(kol,mint) decisions
    with open(os.path.join(OUT, 'kol_decisions.jsonl'), 'w', encoding='utf-8') as f:
        for rec in decisions:
            f.write(json.dumps(rec, ensure_ascii=False) + '\n')

    # write per-mint consensus (which KOLs touched each coin)
    consensus = [{'mint': m, 'kols': sorted(ks), 'n_kols': len(ks)} for m, ks in coin_kols.items()]
    consensus.sort(key=lambda x: -x['n_kols'])
    with open(os.path.join(OUT, 'kol_coin_consensus.json'), 'w', encoding='utf-8') as f:
        json.dump(consensus, f, ensure_ascii=False, indent=1)

    # summary
    n_coins = df['mint'].nunique()
    per_kol = df.groupby('kol').agg(n=('user_wallet', 'count'), coins=('mint', 'nunique'), buys=('is_buy', 'sum'))
    per_kol['sells'] = per_kol['n'] - per_kol['buys']
    print(f'\ncoins touched: {n_coins}')
    print(per_kol.to_string())
    multi = sum(1 for c in consensus if c['n_kols'] >= 2)
    print(f'\ncoins with >=2 KOLs (consensus signal): {multi}')
    print(f'wrote {OUT}/kol_decisions.jsonl ({len(decisions)} records) + kol_coin_consensus.json')


if __name__ == '__main__':
    main()