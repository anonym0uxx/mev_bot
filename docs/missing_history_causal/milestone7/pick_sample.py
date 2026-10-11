"""Fixed representative sample, rule fixed BEFORE looking at results: for each session, among corpus 'decision' rows whose
venue=pumpfun and whose mint has its launch inside the session and >=20 decision rows (RELAXED 100->20->8 as the converted wire covers only parts 0-19 (S1 to 1788966050284, S2 to 1788976428190); disclosed), sort mints by sha256(mint) and take
the first 4; for each take the first 6 clocks by time. Writes sample.json (mint, creator, launch_ms, clocks)."""
import json,hashlib,re,collections
S={'S1':(1788965346866,1788972546869),'S2':(1788975568113,1788982768122)}
COVER={'S1':1788966050284,'S2':1788976428190}  # last recv in the converted wire parts 0-19 (computed below for S2)
launch={}
for l in open('/training/v2/canonical/renormalized_v7/launches.jsonl'):
    r=json.loads(l); launch[r['mint']]=(r['creator'],int(r['recv_unix_ms']))
byms=collections.defaultdict(lambda:collections.defaultdict(list))
for fn in ('train','validation','examination'):
    for l in open('/training/v2/candidate_sft_c12_entry/%s.jsonl'%fn):
        r=json.loads(l)
        if r.get('family')!='decision': continue
        m=r.get('mint'); u=[x for x in r['messages'] if x['role']=='user'][0]['content']
        mm=re.search('t_dec_ms=([0-9]+)',u)
        if not mm or m not in launch or 'venue=pumpfun' not in u: continue
        t=int(mm.group(1))
        for n,(a,b) in S.items():
            if a<=t<=min(b,COVER[n]) and a<=launch[m][1]<=min(b,COVER[n]): byms[n][m].append(t)
out={}
for n in S:
    c=[m for m,v in byms[n].items() if len(v)>=8]
    c.sort(key=lambda m:hashlib.sha256(m.encode()).hexdigest())
    out[n]=[{'mint':m,'creator':launch[m][0],'launch_ms':launch[m][1],'clocks':sorted(byms[n][m])[:5]} for m in c[:4]]
    print(n,len(c),'eligible; chosen',[x['mint'][:8] for x in out[n]])
json.dump(out,open('/tmp/mh_recon2/slice/sample.json','w'))
