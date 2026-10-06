"""Independent reference + before/after comparison for the milestone.
Reference R1: TradeEvents re-decoded in Python from the SAME wire lines (no Rust decoder code), served with the
corpus's c11 window semantics (build_c11_flow_enrichment.serve): events with t-300s <= recv < t.
Reference R2: the corpus pipeline's own tape (renormalize_raw.py output) restricted to the slice -- different
quantity definitions (trader balance delta incl. fees; instruction-level), so only COUNT-type features are compared.
Usage: compare.py SESSION   (reads /tmp/mh_recon2/m1/SESSION{/wire_*.ndjson,.out}, ref/SESSION/pf.jsonl)
"""
import sys,json,glob,base64,struct,collections
sess=sys.argv[1]
PUMP='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P'; TR=bytes.fromhex('e445a52e51cb9a1dbddb7fd34ee661ee')
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def b58(b):
    n=int.from_bytes(b,'big');s=''
    while n: n,r=divmod(n,58);s=A[r]+s
    return '1'*(len(b)-len(b.lstrip(b'\0')))+s
ev=collections.defaultdict(list)   # mint -> [(recv, trader, side, sol, fee, cu)]
maxrecv=0; minrecv=10**18; seen=set()
for f in sorted(glob.glob(f'/tmp/mh_recon2/m1/{sess}/wire_*.ndjson')):
    for l in open(f):
        d=json.loads(l)
        if d['kind']!='transaction': continue
        maxrecv=max(maxrecv,d['recv_unix_ms']); minrecv=min(minrecv,d['recv_unix_ms'])
        if not d['meta']['tx_ok']: continue
        for k,ix in enumerate(d['instructions']):
            if ix['program_b58']!=PUMP: continue
            b=base64.b64decode(ix['data_b64'])
            if b[:16]!=TR or len(b)<137: continue
            key=(d['signature_b58'],k)
            if key in seen: continue
            seen.add(key)
            vs=struct.unpack('<Q',b[105:113])[0]
            if vs==0: continue   # same refusal rule as the Rust path (named gap), applied identically
            sol=struct.unpack('<Q',b[48:56])[0]; isb=b[64]
            ev[b58(b[16:48])].append((d['recv_unix_ms'],b58(b[65:97]),'buy' if isb else 'sell',-sol if isb else sol,d['meta']['fee'],d['meta']['compute_units_consumed']))
def pct(v,q):
    if not v: return None
    return v[min(len(v)-1,max(0,int(round(q*(len(v)-1)))))]
def serve(evs,t):
    w=[e for e in evs if t-300000<=e[0]<t]
    buys=[e for e in w if e[2]=='buy']
    ent={e[1] for e in buys}; ent60={e[1] for e in buys if e[0]>=t-60000}
    net=-sum(e[3] for e in w)
    ac=collections.Counter(e[3] for e in buys)
    uni=sum(1 for e in buys if ac[e[3]]>=3)
    fees=sorted(e[4] for e in buys); cus=sorted(e[5] for e in buys if e[5] is not None)
    return {"entrants_60s":len(ent60),"entrants_300s":len(ent),"net_flow_lamports":net,
            "bot_uniform":(uni,len(buys)),"fee_p90":pct(fees,.9),"cu_p50":pct(cus,.5),"n_events":len(w),"n_buys":len(buys)}
# R2 corpus tape
ref2=collections.defaultdict(list)
for l in open(f'/tmp/mh_recon2/ref/{sess}/pf.jsonl'):
    d=json.loads(l)
    if minrecv<=d['recv_unix_ms']<=maxrecv and d['status']=='success':
        ref2[d['mint']].append((d['recv_unix_ms'],d['trader'],d['side'],d['sol_lamports'],d['fee_lamports'],d['cu_consumed']))
def near(a,micro): # rust micro vs lamports
    return abs(a-round(micro*1000))<=1000   # within 1 micro-SOL (rust rounds to 1e-6 SOL)
rows=[json.loads(l) for l in open(f'/tmp/mh_recon2/m1/{sess}.out')]
summary=rows.pop()['summary']
G=collections.defaultdict(collections.Counter)
ex=[]
for r in rows:
    m=r['mint'];t=r['t'];R1=serve(ev.get(m,[]),t);R2=serve(ref2.get(m,[]),t)
    flagged=r['snapshot_drops_in_window']>0; gap=r['event_gaps_in_window']>0
    grp=('flagged' if flagged else 'unflagged')+('|event_gap' if gap else '')
    g=G[grp]; g['windows']+=1
    for path in ('after','before'):
        x=r[path]
        if x.get('no_prior_flow'):
            ok_np= R1['n_events']==0
            g[f'{path}:no_prior_flow']+=1
            continue
        c60=x['entrants_60s']==R1['entrants_60s']; c300=x['entrants_300s']==R1['entrants_300s']
        net=near(R1['net_flow_lamports'],x['net_flow_micro'])
        fee=x['fee_p90']==R1['fee_p90']; cu=x['cu_p50']==R1['cu_p50']
        bu=R1['bot_uniform']; bum=(None if bu[1]==0 else round(bu[0]/bu[1]*1e6))
        bot=(x['bot_uniform_micro'] is None and bum is None) or (x['bot_uniform_micro'] is not None and bum is not None and abs(x['bot_uniform_micro']-bum)<=1)
        for nm,v in (('entrants_60s',c60),('entrants_300s',c300),('net_flow',net),('bot_uniform',bot),('fee_p90',fee),('cu_p50',cu)):
            g[f'{path}:{nm}:exact']+=int(v)
        g[f'{path}:all_exact']+=int(c60 and c300 and net and bot and fee and cu)
        g[f'{path}:served']+=1
    # corpus-tape cross-check (counts only)
    if not r['after'].get('no_prior_flow'):
        g['after:entrants_300s==corpus_tape']+=int(r['after']['entrants_300s']==R2['entrants_300s'])
        g['after:entrants_60s==corpus_tape']+=int(r['after']['entrants_60s']==R2['entrants_60s'])
        g['corpus_tape:served']+=1
    if r['before'].get('entrants_300s') is not None and len(ex)<4 and r['before']['entrants_300s']!=R1['entrants_300s']:
        ex.append((m[:6],t,'before',r['before']['entrants_300s'],'ref',R1['entrants_300s'],'after',r['after']['entrants_300s']))
out={'session':sess,'slice':'parts 0000-0019','min_recv_ms':minrecv,'max_recv_ms':maxrecv,'harness_summary':summary,'groups':{k:dict(v) for k,v in G.items()},'examples_before_wrong':ex}
json.dump(out,open(f'/tmp/mh_recon2/m1/{sess}.compare.json','w'),indent=1,sort_keys=True)
print(json.dumps(out,indent=1,sort_keys=True))
