import collections,datetime
hours=collections.Counter()
for line in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    i=line.find('"recv_unix_ms":'); k=line.find(',',i+15); t=int(line[i+15:k])
    hours[t//3600000]+=1
hs=sorted(hours); first,last=hs[0],hs[-1]
span=last-first+1; present=len(hs)
gaps=[]; prev=hs[0]
for h in hs[1:]:
    if h-prev>1: gaps.append((prev+1,h-1,h-prev-1))
    prev=h
fmt=lambda h:datetime.datetime.utcfromtimestamp(h*3600).strftime('%Y-%m-%d %H:00')
print('span_hours',span,'hours_with_events',present,'missing_hours',sum(g[2] for g in gaps),'observed_days %.2f of span_days %.2f'%(present/24,span/24))
for g in gaps:
    if g[2]>=6: print('gap',fmt(g[0]),'->',fmt(g[1]),g[2],'h')
