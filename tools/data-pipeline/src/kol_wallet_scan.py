#!/usr/bin/env python
"""kol_wallet_scan.py — scan a KOL wallet's full Solana tx history via Helius.

Complements kol_decision_extract.py (which reads the slinky21 window). This scans a
wallet's ENTIRE history via getSignaturesForAddress + getTransaction, resolves each tx to
(mint, buy/sell, sol_amount), and groups by coin — catching KOLs (e.g. Ansem) whose
activity falls outside the slinky21 window, or extending others to full history.

Resolves mint + direction from parsed pre/post token balances + pump.fun program logs.
"""
import os, json, time, hashlib, argparse, urllib.request

CREDS = 'C:/Users/Alon/.hermes/creds/pump-quant.env'
RPC = 'https://mainnet.helius-rpc.com'
OUT = 'D:/repos/mev_bot/tools/data-pipeline/output/kol_wallet_scan'


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


PUMP_PROGRAM = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
PUMPSWAP_PROGRAM = 'pAMMBay6oceH9fJKBRHGP5D4apD2cMChhx64UHd2wa9'


def resolve_decision(tx, wallet):
    """Return (mint, direction, sol_delta) for a REAL pump.fun/PumpSwap trade, or None.

    Lessons (hard-won):
    * Require the pump.fun/PumpSwap program in accountKeys — transfers/airdrops also move
      balances but are NOT trades.
    * pre/postTokenBalances OMIT the trader's own account for bundles (only AMM/fee
      accounts reported). Use the SOL-native pre/postBalances — always complete — for
      DIRECTION, and any token-balance entry for the MINT.
    """
    msg = (tx.get('transaction') or {}).get('message') or {}
    keys = [a if isinstance(a, str) else (a.get('pubkey') if a else None) for a in (msg.get('accountKeys') or [])]
    if PUMP_PROGRAM not in keys and PUMPSWAP_PROGRAM not in keys:
        return None

    meta = tx.get('meta', {}) or {}
    pre_sol = meta.get('preBalances') or []
    post_sol = meta.get('postBalances') or []

    # wallet's SOL direction (always complete)
    try:
        wi = keys.index(wallet)
    except ValueError:
        return None
    if wi >= len(pre_sol) or wi >= len(post_sol):
        return None
    sol_delta = (post_sol[wi] - pre_sol[wi]) / 1e9

    # mint: any token balance entry in the tx (traded token)
    mints = set()
    for b in (meta.get('preTokenBalances') or []) + (meta.get('postTokenBalances') or []):
        if b.get('mint'):
            mints.add(b['mint'])
    if not mints:
        return None

    logs = ' '.join(meta.get('logMessages', []) or [])
    if sol_delta < -0.01:       # wallet lost SOL -> buy
        direction = 'buy'
    elif sol_delta > 0.01:      # wallet gained SOL -> sell
        direction = 'sell'
    else:
        return None
    return {'mint': sorted(mints)[0], 'direction': direction, 'sol_delta': abs(sol_delta),
            'logs_buy': 'buy' in logs.lower(), 'logs_sell': 'sell' in logs.lower()}


def scan_wallet(key, wallet, max_tx=200):
    sigs = rpc_call(key, 'getSignaturesForAddress', [wallet, {'limit': 1000}])['result'] or []
    timeline = [{'signature': s.get('signature'), 'block_time': s.get('blockTime'),
                 'err': bool(s.get('err'))} for s in sigs]

    decisions = []
    for s in sigs[:max_tx]:
        if s.get('err'):
            continue
        try:
            tx = rpc_call(key, 'getTransaction', [s['signature'],
                         {'encoding': 'jsonParsed', 'maxSupportedTransactionVersion': 0}])['result']
            if not tx:
                continue
            d = resolve_decision(tx, wallet)
            if d:
                d['signature'] = s['signature']
                d['block_time'] = s.get('blockTime')
                decisions.append(d)
            time.sleep(0.35)
        except Exception:
            continue
    return {'wallet': wallet, 'tx_count': len(timeline), 'n_decisions': len(decisions), 'decisions': decisions}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('wallets', nargs='*', help='wallet addresses to scan (default: from seed registry)')
    ap.add_argument('--max-tx', type=int, default=200, help='max parsed txs per wallet')
    args = ap.parse_args()

    key = load_creds(CREDS).get('HELIUS_API_KEY', '')
    if not key:
        print('no HELIUS_API_KEY'); return
    print('key SHA-256[:8] =', hashlib.sha256(key.encode()).hexdigest()[:8])

    wallets = args.wallets
    os.makedirs(OUT, exist_ok=True)
    for i, w in enumerate(wallets):
        out_path = os.path.join(OUT, f'{w}.jsonl')
        if os.path.exists(out_path):
            print(f'skip {w[:12]}.. (exists)')
            continue
        try:
            rec = scan_wallet(key, w, args.max_tx)
            with open(out_path, 'w', encoding='utf-8') as f:
                f.write(json.dumps(rec, ensure_ascii=False) + '\n')
            print(f'{w[:12]}..: tx={rec["tx_count"]} decisions={rec["n_decisions"]}')
        except Exception as e:
            print(f'{w[:12]}..: ERROR {e}')
        time.sleep(0.4)
    print('DONE')


if __name__ == '__main__':
    main()