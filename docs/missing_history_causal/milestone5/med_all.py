import json,statistics,collections
M='51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump'
allr=[]
for l in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    if M in l:
        r=json.loads(l)
        if r['mint']==M and r.get('status','success') in ('success','ok',None): allr.append(r)
print('all-venue rows',len(allr),collections.Counter(r['venue'] for r in allr))
def px(r): return abs(r['sol_lamports'])/abs(r['tokens_raw'])
fl=[r for r in allr if abs(r['sol_lamports'])>=1e5 and abs(r['tokens_raw'])>=1e6]
med_all=statistics.median(px(r) for r in fl)
pf=[r for r in fl if r['venue']=='pumpfun']
med_pf=statistics.median(px(r) for r in pf)
print('median all-venue',med_all,'pumpfun-only',med_pf)
T=1788965530942
for name,med in (('all',med_all),('pf',med_pf)):
    b=[r for r in pf if med/10<=px(r)<=med*10 and r['recv_unix_ms']<=T]
    s=[r for r in b if r['side']=='sell']
    print(name,'band pf trades<=T',len(b),'sells',len(s),sum(abs(r['sol_lamports']) for r in s))
print('corpus: n_prior 472 sells 260 sell_vol 77735972861')
