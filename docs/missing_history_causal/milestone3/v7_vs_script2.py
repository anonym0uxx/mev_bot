"""Does repo renormalize_raw.py reproduce training tape renormalized_v7 rows?  Compare, for signatures in the two capture sessions'
parts 0..NP (the sessions are in the training corpus iff v7 holds their signatures), the (sig, mint, trader, side, sol_lamports, tokens_raw, fee, cu)
sets from my corpus-script output (pf.jsonl, same script) vs the v7 tape rows with the same signatures."""
import sys,json,collections
sess=sys.argv[1]
mine=collections.defaultdict(list)
for l in open(f'/tmp/mh_recon2/ref/{sess}/pf.jsonl'):
    d=json.loads(l); mine[d['signature']].append(d)
print(sess,'my sigs',len(mine))
want=set(mine)
v7=collections.defaultdict(list); scanned=0
with open('/training/v2/canonical/renormalized_v7/trades.jsonl') as f:
    for l in f:
        scanned+=1
        i=l.find('"signature": "')
        if i<0: continue
        sig=l[i+14:i+14+88].split('"')[0]
        if sig in want: v7[sig].append(json.loads(l))
print('scanned',scanned,'v7 rows with those sigs',sum(len(v) for v in v7.values()),'sigs',len(v7))
v7={k:[r for r in v if r['venue']=='pumpfun'] for k,v in v7.items()}
key=lambda d:(d['mint'],d['trader'],d['side'],d['sol_lamports'],d['tokens_raw'],d.get('fee_lamports'),d.get('cu_consumed'),d['venue'],d['status'],d['resolution'])
c=collections.Counter()
for sig,rows in mine.items():
    a=sorted(map(key,rows)); b=sorted(map(key,v7.get(sig,[])))
    if not b: c['sig_absent_in_v7']+=1
    elif a==b: c['identical']+=1
    else: c['differs']+=1
print(dict(c))
import itertools
n=0
for sig,rows in mine.items():
    a=sorted(map(key,rows)); b=sorted(map(key,v7.get(sig,[])))
    if a!=b and n<4:
        n+=1; print('MINE',[ (r['side'],r['sol_lamports'],r['resolution'],r['trader'][:6]) for r in rows],'V7',[(r['side'],r['sol_lamports'],r['resolution'],r['trader'][:6]) for r in v7[sig]])

