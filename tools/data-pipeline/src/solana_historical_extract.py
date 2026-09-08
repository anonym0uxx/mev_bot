#!/usr/bin/env python
"""solana_historical_extract.py — enrich slinky mints with historical tx data.

Pulls Solana transaction history keyed on mint hash via the Helius historical RPC,
and emits a JSONL enrichment (tx timeline + parsed detail for key events).

Design:
  * Credentials read from the creds env file (never command line, never echoed).
  * Targets: graduated mints from slinky21 (full launch->graduation lifecycle).
  * Per mint: getSignaturesForAddress (cheap timeline), then getTransaction for a
    bounded subset (creation + first trades) for parsed instruction/balance detail.
  * Rate-limited (~2 req/s), resumable (skips mints already written).

Output: output/solana_historical_enrichment/<mint>.jsonl  (one file per mint)
"""
import os, sys, json, time, hashlib, argparse, urllib.request

CREDS = 'C:/Users/Alon/.hermes/creds/pump-quant.env'
RPC = 'https://mainnet.helius-rpc.com'
OUT_ROOT = 'D:/repos/mev_bot/tools/data-pipeline/output/solana_historical_enrichment'


def load_creds(path):
    d = {}
    with open(path, encoding='utf-8') as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#') or '=' not in line:
                continue
            k, v = line.split('=', 1)
            d[k.strip()] = v.strip()
    return d


def rpc_call(key, method, params, timeout=30):
    url = f'{RPC}/?api-key={key}'
    body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params}).encode()
    req = urllib.request.Request(url, data=body, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


def load_graduated_mints(limit):
    import duckdb
    con = duckdb.connect()
    pg = 'D:/mev_bot-artifacts/rust-data/slinky21_data/postgard_snapshots.parquet'
    rows = con.execute(
        f"SELECT mint FROM read_parquet('{pg}') GROUP BY mint ORDER BY mint LIMIT {limit}"
    ).fetchall()
    return [r[0] for r in rows]


def classify_tx(tx):
    """Classify a parsed transaction into create/buy/sell/migrate/other via logs."""
    meta = tx.get('meta', {}) or {}
    logs = meta.get('logMessages', []) or []
    joined = ' '.join(logs)
    if 'create' in joined.lower() and 'instruction: create' in joined.lower():
        return 'create'
    if 'global:migrate' in joined or 'migrate' in joined.lower() and 'pumpswap' in joined.lower():
        return 'migrate'
    if 'buy' in joined.lower() and 'bonding curve' in joined.lower():
        return 'buy'
    if 'sell' in joined.lower() and 'bonding curve' in joined.lower():
        return 'sell'
    return 'other'


def extract_balances(tx):
    """Pull pre/post token balance changes for the mint from the tx meta."""
    meta = tx.get('meta', {}) or {}
    out = []
    for pre, post in zip(meta.get('preTokenBalances', []) or [], meta.get('postTokenBalances', []) or []):
        out.append({
            'account': pre.get('owner'),
            'ui_before': pre.get('uiTokenAmount', {}).get('uiAmount'),
            'ui_after': post.get('uiTokenAmount', {}).get('uiAmount'),
        })
    return out


def load_creator_map(mints):
    """Authoritative mint->creator from slinky21 tokens.parquet (not a fee-payer guess)."""
    import duckdb
    con = duckdb.connect()
    tok = 'D:/mev_bot-artifacts/rust-data/slinky21_data/tokens.parquet'
    mlist = "','".join(mints)
    rows = con.execute(
        f"SELECT mint, creator, creator_past_rugs, dev_buy_pct FROM read_parquet('{tok}') "
        f"WHERE mint IN ('{mlist}')"
    ).fetchall()
    return {r[0]: {'creator': r[1], 'creator_past_rugs': r[2], 'dev_buy_pct': r[3]} for r in rows}


def process_mint(key, mint, max_tx=20):
    # 1) timeline
    sigs = rpc_call(key, 'getSignaturesForAddress', [mint, {'limit': 1000}])['result'] or []
    timeline = [{
        'signature': s.get('signature'),
        'slot': s.get('slot'),
        'block_time': s.get('blockTime'),
        'err': bool(s.get('err')),
        'memo': s.get('memo'),
    } for s in sigs]

    # 2) parsed detail for a bounded subset (first N non-err)
    detail = []
    for s in sigs[:max_tx]:
        try:
            tx = rpc_call(key, 'getTransaction', [s['signature'],
                         {'encoding': 'jsonParsed', 'maxSupportedTransactionVersion': 0}])['result']
            if not tx:
                continue
            detail.append({
                'signature': s['signature'],
                'block_time': s.get('blockTime'),
                'kind': classify_tx(tx),
                'balances': extract_balances(tx),
                'fee_payer': (tx.get('transaction', {}).get('message', {}).get('accountKeys') or [{}])[0],
            })
            time.sleep(0.4)  # ~2.5 req/s incl. signatures call
        except Exception:
            continue
    return {'mint': mint, 'tx_count': len(timeline), 'timeline': timeline, 'detail': detail}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--limit', type=int, default=100, help='max mints to process')
    ap.add_argument('--max-tx', type=int, default=20, help='max parsed txs per mint')
    ap.add_argument('--offset', type=int, default=0, help='skip first N mints (resume)')
    args = ap.parse_args()

    key = load_creds(CREDS).get('HELIUS_API_KEY', '')
    if not key:
        print('ERROR: no HELIUS_API_KEY'); sys.exit(1)
    print('key SHA-256[:8] =', hashlib.sha256(key.encode()).hexdigest()[:8])

    mints = load_graduated_mints(args.limit + args.offset)[args.offset:]
    os.makedirs(OUT_ROOT, exist_ok=True)
    print(f'processing {len(mints)} mints (offset {args.offset}, max_tx {args.max_tx})')

    creators = load_creator_map(mints)
    print(f'creator map loaded: {len(creators)}/{len(mints)} mints')

    for i, mint in enumerate(mints):
        out_path = os.path.join(OUT_ROOT, f'{mint}.jsonl')
        if os.path.exists(out_path):
            continue  # resumable
        try:
            rec = process_mint(key, mint, args.max_tx)
            rec['creator'] = creators.get(mint, {}).get('creator')
            rec['creator_past_rugs'] = creators.get(mint, {}).get('creator_past_rugs')
            rec['dev_buy_pct'] = creators.get(mint, {}).get('dev_buy_pct')
            with open(out_path, 'w', encoding='utf-8') as f:
                f.write(json.dumps(rec, ensure_ascii=False) + '\n')
            if (i + 1) % 10 == 0:
                print(f'  {i+1}/{len(mints)} done; last {mint[:12]}.. tx_count={rec["tx_count"]} creator={str(rec["creator"])[:12]}')
        except Exception as e:
            print(f'  [{mint[:12]}..] ERROR {e}')
        time.sleep(0.4)  # rate limit
    print('DONE')


if __name__ == '__main__':
    main()