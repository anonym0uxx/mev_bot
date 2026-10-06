import json,re,collections
d=json.load(open('/tmp/mh_recon2/slice_rows.json'))
R={'S1':(1788965347185,1788966050284)}
c=collections.Counter(); cand=[]
for k,rows in d.items():
    s=k.split('|')[0]
    if s not in R: continue
    for r in rows:
        u=[m for m in r['messages'] if m['role']=='user'][0]['content']
        t=int(re.search(r't_dec_ms=(\d+)',u).group(1))
        if not R[s][0]<=t<=R[s][1]: continue
        v=re.search(r'venue=(\w+)',u).group(1); n=int(re.search(r'n_prior_trades=(\d+)',u).group(1))
        c[(k,v)]+=1
        cand.append((k,r['mint'],t,v,n,r['episode_id']))
print(c)
for x in sorted(cand,key=lambda x:-x[4])[:8]: print(x)
json.dump(cand,open('/tmp/mh_recon2/cand_s1.json','w'))
