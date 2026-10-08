import json,collections,datetime
cnt=collections.Counter(); first={}; last={}
f=open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl')
for line in f:
    i=line.find('"recv_unix_ms": ')
    if i<0: i=line.find('"recv_unix_ms":')
    j=i+len('"recv_unix_ms":'); k=line.find(',',j)
    t=int(line[j:k]); d=datetime.datetime.utcfromtimestamp(t/1000).strftime('%Y-%m-%d')
    cnt[d]+=1
tot=0
for d in sorted(cnt): print(d,cnt[d])
