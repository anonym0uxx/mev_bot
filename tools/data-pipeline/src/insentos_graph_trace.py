#!/usr/bin/env python
"""insentos_graph_trace.py — trace outbound edges from the shill wallet to find
the real trading wallet(s).

A real trader moves SOL OUT (to fund buy sub-wallets) and receives tokens/SOL IN
(gains). This script paginates the wallet's history, samples txs across time, and maps
SOL-native and token outbound edges to counter-party wallets, ranked by frequency.
"""
import json, urllib.request, time
from collections import Counter

CREDS = 'C:/Users/Alon/.hermes/creds/pump-quant.env'
RPC = 'https://mainnet.helius-rpc.com'
W = '7SDs3PjT2mswKQ7Zo4FTucn9gJdtuW4jaacPA65BseHS'
PUMP = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'
PAMM = 'pAMMBay6oceH9fJKBRHGP5D4apD2cMChhx64UHd2wa9'


def load_creds(p):
    d = {}
    for l in open(p):
        l = l.strip()
        if l and '=' in l and not l.startswith('#'):
            k, v = l.split('=', 1); d[k.strip()] = v.strip()
    return d


def rpc(key, m, p):
    url = f'{RPC}/?api-key={key}'
    b = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': m, 'params': p}).encode()
    r = urllib.request.Request(url, data=b, headers={'Content-Type': 'application/json'})
    return json.loads(urllib.request.urlopen(r, timeout=30).read().decode())


key = load_creds(CREDS)['HELIUS_API_KEY']

# 1) paginate all signatures
sigs = []
before = None
while True:
    pr = [W, {'limit': 1000, 'before': before}] if before else [W, {'limit': 1000}]
    b = rpc(key, 'getSignaturesForAddress', pr).get('result') or []
    if not b:
        break
    sigs.extend([(s['signature'], s.get('blockTime')) for s in b])
    before = b[-1]['signature']
    if len(b) < 1000:
        break
print(f'total sigs: {len(sigs)}')

# 2) sample evenly across history (cap ~600 getTransaction calls)
N = min(600, len(sigs))
step = len(sigs) // N
sample = [sigs[i * step] for i in range(N)]

sol_out = Counter()     # recipient -> count (SOL native out)
tok_out = Counter()     # recipient -> count (token out)
linked_wallets = Counter()  # any other account that frequently appears with a pump.fun tx

def acct_keys(tx):
    msg = (tx.get('transaction') or {}).get('message') or {}
    return [(a if isinstance(a, str) else a.get('pubkey')) for a in (msg.get('accountKeys') or [])]

for i, (sig, bt) in enumerate(sample):
    tx = rpc(key, 'getTransaction', [sig, {'encoding': 'jsonParsed', 'maxSupportedTransactionVersion': 0}]).get('result')
    if not tx:
        continue
    meta = tx.get('meta', {}) or {}
    keys = acct_keys(tx)
    # determine if W's SOL (native) balance changed, and who else was involved
    pre = meta.get('preBalances') or []
    post = meta.get('postBalances') or []
    # accountKeys[0] is fee payer often; find W's index
    try:
        wi = keys.index(W)
    except ValueError:
        continue
    if wi < len(pre) and wi < len(post):
        dsol = (post[wi] - pre[wi]) / 1e9
        if dsol < -0.05:  # W sent SOL out (>0.05 SOL)
            # recipient = other signers with SOL gain
            for j, k in enumerate(keys):
                if j == wi or j >= len(pre):
                    continue
                d = (post[j] - pre[j]) / 1e9 if j < len(post) else 0
                if d > 0.05:
                    sol_out[k] += 1
    # token outbound: W's token balance decreased
    for b in (meta.get('preTokenBalances') or []):
        if b.get('owner') == W and b.get('mint'):
            m = b['mint']
            amt = b.get('uiTokenAmount', {}).get('uiAmount') or 0
            # find who gained this mint (post)
            for bb in (meta.get('postTokenBalances') or []):
                if bb.get('mint') == m and bb.get('owner') != W:
                    ga = bb.get('uiTokenAmount', {}).get('uiAmount') or 0
                    if ga > amt:  # someone gained tokens while W had some
                        tok_out[bb.get('owner')] += 1
    time.sleep(0.25)
    if (i + 1) % 200 == 0:
        print(f'  scanned {i+1}/{N}')

print('\n=== SOL-native outbound counter-parties (top 10) ===')
for k, c in sol_out.most_common(10):
    print(f'  {k}  x{c}')
print('\n=== Token outbound counter-parties (top 10) ===')
for k, c in tok_out.most_common(10):
    print(f'  {k}  x{c}')