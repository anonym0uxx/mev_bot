"""Whole-run-median band defect, both sessions, ALL mints (diagnostic; never a serving rule).
Frozen builder: per mint, trades (tape, status ok, value floors) -> keep px within [med/10, med*10], med over the WHOLE run.
Measures per session: mints, trades, trades dropped by the band, mints with >=1 dropped, and the per-mint max drop share.
Also the CAUSAL alternative's disagreement: running-median band (1000x, the production ledger rule) vs the frozen band."""
import json,sys,collections,statistics
S={'S1':(1788965346866,1788972546869),'S2':(1788975568113,1788982768122)}
per={k:collections.defaultdict(list) for k in S}
for line in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    i=line.find('"recv_unix_ms":'); k=line.find(',',i+15); t=int(line[i+15:k])
    sess=None
    for n,(a,b) in S.items():
        if a<=t<=b: sess=n
    if sess is None:
        if t>S['S2'][1]: break
        continue
    r=json.loads(line)
    if r.get('status')!='success': continue
    sol=r.get('sol_lamports'); tok=r.get('tokens_raw')
    if sol is None or tok is None or abs(sol)<100_000 or abs(tok)<1_000_000: continue
    per[sess][r['mint']].append((t,abs(sol)/abs(tok)))
out={}
for sess,d in per.items():
    nm=nt=nd=mints_hit=0; shares=[]; causal_dis=0
    for m,v in d.items():
        if len(v)<5: continue
        med=statistics.median(p for _,p in v)
        drop=sum(1 for _,p in v if p<med/10 or p>med*10)
        nm+=1; nt+=len(v); nd+=drop
        if drop: mints_hit+=1; shares.append(drop/len(v))
    out[sess]={'mints_ge5':nm,'trades':nt,'dropped_by_whole_run_band':nd,'drop_share':round(nd/max(nt,1),6),'mints_with_any_drop':mints_hit,'median_drop_share_among_hit':round(statistics.median(shares),4) if shares else 0,'max_drop_share':round(max(shares),4) if shares else 0}
print(json.dumps(out,indent=1))
