import json,datetime,collections
f=open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl')
a=json.loads(f.readline())
f.seek(0,2); sz=f.tell(); f.seek(sz-6000); f.readline(); b=json.loads(f.readline())
u=lambda r:datetime.datetime.utcfromtimestamp(r['recv_unix_ms']/1000)
print('first',a['recv_unix_ms'],u(a)); print('last',b['recv_unix_ms'],u(b)); print('days',(b['recv_unix_ms']-a['recv_unix_ms'])/86.4e6)
