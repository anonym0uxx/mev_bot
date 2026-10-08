"""Corpus-tape multi-swap repetition: rows per (signature, trader) and how much SOL is counted more than once.
Reads the corpus-script output (pf.jsonl = pumpfun rows of renormalize_raw.py, byte-identical to training tape v7 on these sigs)."""
import sys,json,collections
sess=sys.argv[1]
g=collections.defaultdict(list)
for l in open(f'/tmp/mh_recon2/ref/{sess}/pf.jsonl'):
    d=json.loads(l); g[(d['signature'],d['trader'])].append(d)
n=len(g); rows=sum(len(v) for v in g.values())
multi=[v for v in g.values() if len(v)>1]
same_sol=[v for v in multi if len({r['sol_lamports'] for r in v})==1]
extra=sum(len(v)-1 for v in multi)
extra_abs=sum(abs(v[0]['sol_lamports'])*(len(v)-1) for v in same_sol)
tot_abs=sum(abs(r['sol_lamports']) for v in g.values() for r in v)
print(sess,'rows',rows,'(sig,trader) groups',n,'groups with >1 row',len(multi),'of which identical sol on every row',len(same_sol),'extra rows',extra)
print('  extra rows pct %.4f ; abs sol counted again pct %.4f'%(100*extra/rows,100*extra_abs/max(1,tot_abs)))
sides=collections.Counter(tuple(sorted(r['side'] for r in v)) for v in multi); print('  side mixes',dict(sides.most_common(5)))
res=collections.Counter(r['resolution'] for v in multi for r in v); print('  resolution in repeated rows',dict(res))
