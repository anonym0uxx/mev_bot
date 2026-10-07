import json,glob,collections,re
# Which clocks differ on `reason`, and does the mint have a PumpSwap row in the frozen tape strictly AFTER the clock?
rows=[]
for f in sorted(glob.glob('/tmp/mh_recon2/slice/prod_S*.diff.json'))+['/tmp/mh_recon2/slice/prod14_51nH.diff.json']:
    for x in json.load(open(f)):
        for g in x.get('diffs',[]):
            if g['field']=='reason': rows.append((f.split('/')[-1][:22],x['t'],g['rust'],g['corpus']))
print(len(rows))
mints={}
for r in rows: mints.setdefault(r[0],[]).append(r)
full={}
import os
for l in open('/training/mh_build/mint_full.tsv') if os.path.exists('/training/mh_build/mint_full.tsv') else []:
    a,b=l.rstrip('\n').split('\t'); full[a[:8]]=b
want={r[0].split('_')[-1][:8] for r in rows}
# find first pumpswap row time per wanted mint prefix
first={}
for l in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    if '"pumpswap"' not in l: continue
    i=l.find('"mint": "')
    p=l[i+9:i+17]
    if p in want:
        t=json.loads(l)['recv_unix_ms']
        if p not in first or t<first[p]: first[p]=t
for r in rows:
    p=r[0].split('_')[-1][:8]
    if r[0].startswith('prod14'): p='51nHMcvh'
    print(r[0],r[1],'rust=',r[2][:60],'| corpus=',r[3][:60],'| first_pumpswap_row_in_tape=',first.get(p),'| after_clock=',(first.get(p) or 0)>r[1])
