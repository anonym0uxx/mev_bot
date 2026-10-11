"""Enriched block rebuild (build_c9_enrichment_full.py logic) from the verified tape for one mint, vs the corpus prompts.
Boundary variants tested separately: <= t (c9 rule) and < t (state/flow rule). Populations: all-venue vs pumpfun-only.
No band, no dust floor (c9 applies none)."""
import json,re,collections,sys
M=sys.argv[1]; SESS=sys.argv[2]
rows=[]
for l in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    if M in l:
        r=json.loads(l)
        if r['mint']==M and r.get('status')=='success': rows.append(r)
ev=[(int(r['recv_unix_ms']),r['trader'],str(r['side']),int(r['tokens_raw']),int(r['sol_lamports']),r['slot'],r['venue']) for r in rows if r.get('trader') and r.get('tokens_raw') is not None and r.get('sol_lamports') is not None]
ev.sort(key=lambda e:e[:6])
def enrich(t,le,venues):
    cut=[e for e in ev if (e[0]<=t if le else e[0]<t) and e[6] in venues]
    if len(cut)<2: return None
    bal=collections.Counter(); vol=0; slots=collections.defaultdict(set); rt=collections.Counter()
    for (_,tr,side,tok,sol,slot,_v) in cut:
        bal[tr]+=tok; vol+=abs(sol)
        rt[tr]+= 1 if side=='buy' else -1
        if slot is not None: slots[slot].add(tr)
    hs=[v for v in bal.values() if v>0]; tot=sum(hs) or 1; d=sorted(hs,reverse=True)
    return {'holders_at_t':len(hs),'top1_float_share':round(d[0]/tot,6) if d else 0.0,'top5_float_share':round(sum(d[:5])/tot,6) if d else 0.0,
            'holder_hhi':round(sum((x/tot)**2 for x in hs),6),'bundle_slots':sum(1 for s,t_ in slots.items() if len(t_)>=2),
            'bundle_wallets':sum(len(t_) for t_ in slots.values() if len(t_)>=2),'volume_sol_at_t':round(vol/1e9,6),
            'wash_ratio':round(sum(1 for n in rt.values() if n==0)/max(1,len(bal)),6)}
corp={}
for fn in ('train','validation','examination'):
    for l in open(f'/training/v2/candidate_sft_c12_entry/{fn}.jsonl'):
        if M not in l: continue
        r=json.loads(l)
        if r.get('mint')!=M or r.get('family')!='decision': continue
        u=[m for m in r['messages'] if m['role']=='user'][0]['content']
        mm=re.search('t_dec_ms=([0-9]+)',u)
        if not mm: continue
        e=re.search('ENRICHED CANDIDATE STATE[^'+chr(10)+']*',u)
        if e: corp[int(mm.group(1))]={k:v for k,v in re.findall('([A-Za-z0-9_]+)=([-A-Za-z0-9_.]+)',e.group(0))}
clocks=sorted(t for t in corp if 1788965346866<=t<=1788972546869)
F=('holders_at_t','top1_float_share','top5_float_share','holder_hhi','bundle_slots','bundle_wallets','volume_sol_at_t','wash_ratio')
res={}
for name,le,venues in (('le_allvenue',True,{'pumpfun','pumpswap'}),('lt_allvenue',False,{'pumpfun','pumpswap'}),('le_pumpfun_only',True,{'pumpfun'})):
    bad=collections.Counter(); n=0
    for t in clocks:
        o=enrich(t,le,venues)
        if o is None: continue
        n+=1
        for k in F:
            if k not in corp[t] or abs(float(o[k])-float(corp[t][k]))>1e-6:
                bad[k]+=1
                if bad[k]==1: print('FIRST',name,k,t,o[k],corp[t].get(k),file=sys.stderr)
    res[name]={'clocks':n,'mismatch_by_field':dict(bad)}
print(json.dumps({'mint':M,'clocks_in_session':len(clocks),'results':res},indent=1))
