"""Controlled population x band comparison of the STATE block, per (mint, clock), against the frozen corpus prompt.
Conditions (all strictly-prior, status success, dust floors as build_states_v2):
  F  = frozen reproduction: ALL venues, whole-run median band [med/10, med*10]   (diagnostic; uses future)
  N  = ALL venues, no band
  C1 = ALL venues, causal band: running median of strictly-prior accepted trades, 10x
  C2 = ALL venues, causal band 1000x (production ledger rule, as a running reference to the first prior trade)
  PF = PUMPFUN-only, frozen band           (population effect with the band held fixed)
  PN = PUMPFUN-only, no band
A condition is only trustworthy if F reproduces the corpus; that is reported first and can fail.
Fields: buy_count sell_count n_prior_trades unique_traders net_flow_lamports buy_volume_lamports sell_volume_lamports"""
import json,re,sys,statistics,bisect,collections
d=json.load(open('/tmp/mh_recon2/slice/sample.json'))
want={}
for s,L in d.items():
    for x in L: want[x['mint']]=(s,x['clocks'])
want['51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump']=('S1',[int(c) for c in open('/tmp/mh_recon2/slice/clocks.txt').read().strip().split(',')])
rows=collections.defaultdict(list)
for l in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    i=l.find('"mint": "'); m=l[i+9:i+9+44]
    # mints are 43/44 chars: match by prefix of 43
    hit=None
    for w in want:
        if m.startswith(w[:43]): hit=w;break
    if not hit: continue
    r=json.loads(l)
    if r.get('status')!='success': continue
    sol=r.get('sol_lamports'); tok=r.get('tokens_raw')
    if sol is None or tok is None or abs(sol)<100_000 or abs(tok)<1_000_000: continue
    rows[hit].append((r['recv_unix_ms'],r['venue'],r['side'],abs(sol),abs(tok),r['trader']))
corp={}
for fn in ('train','validation','examination'):
    for l in open(f'/training/v2/candidate_sft_c12_entry/{fn}.jsonl'):
        r=json.loads(l)
        if r.get('mint') not in want or r.get('family')!='decision': continue
        u=[m for m in r['messages'] if m['role']=='user'][0]['content']
        mm=re.search(r't_dec_ms=(\d+)',u)
        if mm: corp[(r['mint'],int(mm.group(1)))]=u
F=['buy_count','sell_count','n_prior_trades','unique_traders','net_flow_lamports','buy_volume_lamports','sell_volume_lamports']
TOK=re.compile(r'([A-Za-z_0-9]+)=([^\s]+)')
def parse(u):
    dd={}
    for ln in u.split('\n'):
        for k,v in TOK.findall(ln): dd.setdefault(k,v)
    return dd
def state(trs,t):
    prior=[x for x in trs if x[0]<t]
    b=[x for x in prior if x[2]=='buy']; s=[x for x in prior if x[2]=='sell']
    return {'buy_count':len(b),'sell_count':len(s),'n_prior_trades':len(prior),'unique_traders':len({x[5] for x in prior}),
      'net_flow_lamports':sum(x[3] for x in b)-sum(x[3] for x in s),'buy_volume_lamports':sum(x[3] for x in b),'sell_volume_lamports':sum(x[3] for x in s)}
def px(x): return x[3]/x[4]
def band_whole(trs,lo,hi):
    if len(trs)<5: return trs
    med=statistics.median(px(x) for x in trs); return [x for x in trs if med/lo<=px(x)<=med*hi]
def band_causal(trs,k):
    out=[];hist=[]
    for x in trs:
        if len(hist)>=1:
            med=statistics.median(hist)
            if not (med/k<=px(x)<=med*k): continue
        out.append(x); hist.append(px(x))
    return out
res=collections.defaultdict(lambda: collections.defaultdict(lambda:[0,0]))   # cond -> field -> [match,total]
clk_stats=[]; per_mint=collections.defaultdict(lambda: collections.defaultdict(lambda:[0,0]))
for m,(s,clocks) in want.items():
    allv=sorted(rows[m])
    conds={'F':band_whole(allv,10,10),'N':allv,'C1':band_causal(allv,10),'C2':band_causal(allv,1000)}
    pf=[x for x in allv if x[1]=='pumpfun']
    conds['PF']=band_whole(pf,10,10) if len(pf)>=5 else pf
    conds['PN']=pf
    dropped=len(allv)-len(conds['F'])
    for t in clocks:
        u=corp.get((m,t))
        if not u: continue
        cv=parse(u)
        for c,trs in conds.items():
            st=state(trs,t)
            for f in F:
                try: ok=int(float(cv[f]))==st[f]
                except Exception: ok=False
                res[c][f][1]+=1; res[c][f][0]+=ok
                per_mint[m][c][1]+=1; per_mint[m][c][0]+=ok
    clk_stats.append((m[:8],s,len(allv),dropped,round(dropped/max(len(allv),1),4)))
out={'fields':F,'match_by_condition':{c:{f:v for f,v in fv.items()} for c,fv in res.items()},
     'per_mint_whole_run_band_drop':clk_stats,
     'per_mint_total_match':{m[:8]:{c:f'{v[0]}/{v[1]}' for c,v in cd.items()} for m,cd in per_mint.items()}}
print(json.dumps(out,indent=1))
