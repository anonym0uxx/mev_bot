import json,sys
M='BHprQ1wT'; T=1788975741870
for l in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    i=l.find('"mint": "'+M)
    if i<0: continue
    r=json.loads(l)
    if abs(r['recv_unix_ms']-T)<=3000:
        print(r['recv_unix_ms']-T,r['venue'],r['side'],r['sol_lamports'],r['tokens_raw'],r['trader'][:8],r['status'],r['signature'][:10],r.get('resolution'))
