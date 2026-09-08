#!/usr/bin/env python
"""confirm_stream_activity.py — verify a streamer's on-chain trades match a VOD's
broadcast window.

Model: a stream/VOD broadcast on Day X. A KOL trading live on that stream leaves
on-chain footprints on Day X at times inside the stream duration. This stage checks
whether a wallet has pump.fun/PumpSwap trades co-temporal with the broadcast — if so,
those trades are almost certainly the ones being narrated.

Usage:
  python confirm_stream_activity.py <wallet> <start_unix> <end_unix> [--max-tx 40]

Reads HELIUS_API_KEY from ~/.hermes/creds/pump-quant.env (never on cmdline/echoed).
"""
import os, sys, json, urllib.request, datetime, time

CREDS = 'C:/Users/Alon/.hermes/creds/pump-quant.env'
RPC = 'https://mainnet.helius-rpc.com'
PUMP = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMP_SWAP = "pAMMBay6oceH9fJKBRHGP5D4apD2cMChhx64UHd2wa9"


def load_key():
    d = {}
    with open(CREDS, encoding='utf-8') as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith('#') and '=' in line:
                k, v = line.split('=', 1)
                d[k.strip()] = v.strip()
    return d.get('HELIUS_API_KEY', '')


def rpc(method, params, key):
    url = f'{RPC}/?api-key={key}'
    body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method,
                       'params': params}).encode()
    req = urllib.request.Request(url, data=body, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=25) as r:
        return json.loads(r.read().decode())


def is_trade(tx):
    """Return True if the tx touched pump.fun or PumpSwap program."""
    msg = tx.get('transaction', {}).get('message', {})
    acct_keys = msg.get('accountKeys', [])
    progs = set()
    for a in acct_keys:
        if isinstance(a, dict) and a.get('pubkey'):
            progs.add(a['pubkey'])
        elif isinstance(a, str):
            progs.add(a)
    if PUMP in progs or PUMP_SWAP in progs:
        return True
    # also check inner instructions
    for inner in (tx.get('meta', {}).get('innerInstructions') or []):
        for ix in inner.get('instructions', []):
            if isinstance(ix, dict) and ix.get('programId') in (PUMP, PUMP_SWAP):
                return True
    return False


def main():
    wallet = sys.argv[1]
    start = int(sys.argv[2])
    end = int(sys.argv[3])
    max_tx = 40
    if '--max-tx' in sys.argv:
        max_tx = int(sys.argv[sys.argv.index('--max-tx') + 1])

    key = load_key()
    if not key:
        print('ERROR: HELIUS_API_KEY missing'); return
    print(f'window: {datetime.datetime.utcfromtimestamp(start).isoformat()}Z -> '
          f'{datetime.datetime.utcfromtimestamp(end).isoformat()}Z '
          f'({(end-start)/3600:.1f}h)')

    # 1) paginate signatures back to the start of the window
    all_sigs = []
    before = None
    while True:
        params = [wallet, {'limit': 1000}]
        if before:
            params[1]['before'] = before
        res = rpc('getSignaturesForAddress', params, key)
        page = res.get('result', [])
        if not page:
            break
        all_sigs.extend(page)
        oldest_bt = min((s.get('blockTime') for s in page if s.get('blockTime')),
                        default=None)
        if oldest_bt is None or oldest_bt < start:
            break
        before = page[-1]['signature']
        if len(page) < 1000:
            break
        time.sleep(0.2)
    in_win = [s for s in all_sigs if s.get('blockTime')
              and start <= s['blockTime'] <= end]
    print(f'fetched {len(all_sigs)} signatures back through window; '
          f'{len(in_win)} inside the broadcast window')

    # 2) classify in-window signatures as trades
    trades, non_trades, nulls = [], [], []
    for s in in_win[:max_tx]:
        res = rpc('getTransaction', [s['signature'],
                                     {'encoding': 'jsonParsed',
                                      'maxSupportedTransactionVersion': 0}], key)
        tx = res.get('result')
        if not tx:
            nulls.append(s); continue
        if is_trade(tx):
            trades.append(s)
        else:
            non_trades.append(s)
        time.sleep(0.15)

    def fmt(ts):
        return datetime.datetime.utcfromtimestamp(ts).strftime('%H:%M:%S')
    print(f'classified {len(in_win[:max_tx])}: trades={len(trades)} '
          f'non_trades={len(non_trades)} null={len(nulls)}')
    print('\n--- trades (pump.fun/PumpSwap) in window ---')
    for s in trades:
        print(f'  {fmt(s["blockTime"] + (end-start))}'.replace(' ', '')
              + f'  {s["signature"][:16]}  err={s.get("err")}')
    if nulls:
        print(f'\n({len(nulls)} signatures returned null tx — likely bundled/'
              f'pruned; treat as UNRESOLVED, not inactive)')


if __name__ == '__main__':
    main()