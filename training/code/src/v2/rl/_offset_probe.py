import json, base64, sys, collections
sys.path.insert(0, '.')
from resolve_pool_quotes import post, b58d, WATCH
from validate_pool_quote_scan import b58e
from amm_acquire_fast import load_key

url = "https://mainnet.helius-rpc.com/?api-key=" + load_key()
old = json.load(open('/training/v2/reports/AMM_POOLS_RESOLVED_V1.json'))['per_mint_pools']
pairs = []
for m, pl in old.items():
    for e in pl:
        if (e.get('quotes') or []) == ['WSOL']:
            pairs.append((m, e['pool']))
pairs = pairs[:8]
payload = [{"jsonrpc": "2.0", "id": i, "method": "getAccountInfo",
            "params": [p, {"encoding": "base64"}]} for i, (m, p) in enumerate(pairs)]
res = post(url, payload)
for i, (m, p) in enumerate(pairs):
    v = (res[i].get('result') or {}).get('value') or {}
    raw = base64.b64decode(v['data'][0]) if v else b''
    print(p[:10], 'len', len(raw), 'discr', raw[:8].hex(),
          '| off43==mint:', raw[43:75] == b58d(m),
          '| off75:', b58e(raw[75:107])[:14],
          '| off107:', b58e(raw[107:139])[:14])
print('WSOL b58 first14:', WATCH['WSOL'][:14])
