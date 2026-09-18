#!/usr/bin/env python
"""insentos_full_history.py — paginate Insentos's full signature history + report."""
import json, urllib.request, hashlib

CREDS = 'C:/Users/Alon/.hermes/creds/pump-quant.env'
RPC = 'https://mainnet.helius-rpc.com'
W = '7SDs3PjT2mswKQ7Zo4FTucn9gJdtuW4jaacPA65BseHS'

def load_creds(p):
    d = {}
    for l in open(p):
        l = l.strip()
        if l and '=' in l and not l.startswith('#'):
            k, v = l.split('=', 1); d[k.strip()] = v.strip()
    return d

def rpc(key, method, params):
    url = f'{RPC}/?api-key={key}'
    body = json.dumps({'jsonrpc':'2.0','id':1,'method':method,'params':params}).encode()
    req = urllib.request.Request(url, data=body, headers={'Content-Type':'application/json'})
    return json.loads(urllib.request.urlopen(req, timeout=30).read().decode())

key = load_creds(CREDS)['HELIUS_API_KEY']
sigs = []
before = None
pages = 0
while True:
    params = [W, {'limit': 1000, 'before': before}] if before else [W, {'limit': 1000}]
    r = rpc(key, 'getSignaturesForAddress', params)
    batch = r.get('result') or []
    if not batch:
        break
    sigs.extend(batch)
    pages += 1
    before = batch[-1]['signature']
    if len(batch) < 1000:
        break
    if pages >= 20:  # safety cap
        break

times = [s.get('blockTime') for s in sigs if s.get('blockTime')]
errs = sum(1 for s in sigs if s.get('err'))
print(f'total signatures: {len(sigs)} (pages={pages})')
print(f'  err: {errs}')
if times:
    print(f'  blockTime range: {min(times)} -> {max(times)}  (span {max(times)-min(times)}s = {(max(times)-min(times))/86400:.1f} days)')