import json,statistics
M='51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump'
rows=[json.loads(l) for l in open('/tmp/mh_recon2/ref/20260909_144906_000490/pf.jsonl') if M in l]
rows=[r for r in rows if r['mint']==M]
fl=[r for r in rows if abs(r['sol_lamports'])>=1e5 and abs(r['tokens_raw'])>=1e6]
med=statistics.median(abs(r['sol_lamports'])/abs(r['tokens_raw']) for r in fl)
band=[r for r in fl if med/10<=abs(r['sol_lamports'])/abs(r['tokens_raw'])<=med*10]
d=json.load(open('/tmp/mh_recon2/slice/s1_51nH.diff_band.json'))
x=[y for y in d if y['t']==1788965530942][0]
m={f['field']:(f['rust'],f['corpus']) for f in x['diffs']}
print(m.get('sell_volume_lamports'),m.get('sell_count'))
T=1788965530942
sells=[r for r in band if r['side']=='sell' and r['recv_unix_ms']<=T]
tot=sum(abs(r['sol_lamports']) for r in sells); print('band sells',len(sells),tot)
cv=int(m['sell_volume_lamports'][1]) if 'sell_volume_lamports' in m else tot
gap=tot-cv; print('gap lamports',gap)
for r in sells:
    if abs(abs(r['sol_lamports'])-gap)<=1: print('exact row',r['signature'][:14],r['recv_unix_ms'],r['sol_lamports'],r['tokens_raw'],r['resolution'],r['status'])
# other candidate attributes: duplicate (slot,trader,...)?
import collections
c=collections.Counter((r['signature'],r['mint'],r['trader']) for r in sells)
print('dup sig+trader in sells',[k[0][:10] for k,v in c.items() if v>1])
