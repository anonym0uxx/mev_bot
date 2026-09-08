#!/usr/bin/env python
"""helius_health.py — verify Helius RPC connectivity + report rate limits.

Reads the API key from the creds env file (never on the command line, never echoed).
Reports connection status, slot, and rate-limit headers only.
"""
import os, json, urllib.request, hashlib

CREDS = 'C:/Users/Alon/.hermes/creds/pump-quant.env'
RPC = 'https://mainnet.helius-rpc.com'


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


def main():
    creds = load_creds(CREDS)
    key = creds.get('HELIUS_API_KEY', '')
    if not key:
        print('ERROR: HELIUS_API_KEY missing')
        return
    fp = hashlib.sha256(key.encode()).hexdigest()[:8]
    print(f'key fingerprint SHA-256[:8] = {fp}')

    url = f'{RPC}/?api-key={key}'
    for method, params in [('getHealth', []), ('getSlot', [])]:
        body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params}).encode()
        req = urllib.request.Request(url, data=body, headers={'Content-Type': 'application/json'})
        try:
            with urllib.request.urlopen(req, timeout=20) as resp:
                data = json.loads(resp.read().decode())
                hdrs = resp.headers
                print(f'{method}: result={data.get("result")}')
                print(f'  rate-limit-remaining: {hdrs.get("x-ratelimit-remaining")}')
                print(f'  rate-limit-limit:     {hdrs.get("x-ratelimit-limit")}')
                print(f'  credits-remaining:    {hdrs.get("helius-credits-remaining") or hdrs.get("x-credit-remaining")}')
        except Exception as e:
            print(f'{method}: ERROR {e}')


if __name__ == '__main__':
    main()