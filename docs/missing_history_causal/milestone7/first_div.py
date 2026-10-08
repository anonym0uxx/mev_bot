"""First divergence: smart-wallet status under cur vs cur+ps at the FIRST clock for mint M (seed < capture start, in-session < clock).
Reducer rule (flow_reducer.rs): smart = extracted_lamports(sum of signed sol over ALL applied events) >= 5e9 AND distinct mints >= 5.
Reports the buyer wallets whose smart status differs between populations and the earliest event that makes the difference."""
import json,sys,collections
M='51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump'; S1=1788965346866; T=1788965440942
fed=set()
for l in open('/tmp/mh_recon2/slice/fed_keys_s1.tsv'):
    p=l.rstrip('\n').split('\t'); fed.add((p[0],p[1]=='buy',int(p[2])))
ext={'cur':collections.Counter(),'all':collections.Counter()}
mints={'cur':collections.defaultdict(set),'all':collections.defaultdict(set)}
firstev={}
buyers=set(); win=T-300000
n=0
for line in open('/training/v2/canonical/renormalized_v7/trades.sorted.jsonl'):
    i=line.find('"recv_unix_ms":'); j=i+15; k=line.find(',',j)
    t=int(line[j:k])
    if t>=T: break
    r=json.loads(line)
    if r.get('status')!='success': continue
    w=r['trader']; sol=int(r['sol_lamports']); buy=r['side']=='buy'
    is_ps=r['venue']=='pumpswap'
    in_cur=(t<S1) or ((not is_ps) and (r['signature'],buy,sol) in fed)
    ext['all'][w]+=sol; mints['all'][w].add(r['mint'])
    if in_cur: ext['cur'][w]+=sol; mints['cur'][w].add(r['mint'])
    elif (w,'ps' if is_ps else 'excl') not in firstev: firstev[(w,'ps' if is_ps else 'excl')]=(t,r['signature'][:12],r['venue'],sol)
    if r['mint']==M and buy and win<=t: buyers.add(w)
def smart(p,w): return ext[p][w]>=5_000_000_000 and len(mints[p][w])>=5
diff=[w for w in buyers if smart('cur',w)!=smart('all',w)]
print('buyers in window',len(buyers),'smart cur',sum(smart('cur',w) for w in buyers),'smart all',sum(smart('all',w) for w in buyers),'differ',len(diff))
for w in sorted(diff,key=lambda w:min([firstev.get((w,k),(1e18,))[0] for k in ('ps','excl')]))[:5]:
    print(w[:10],'ext cur',ext['cur'][w],'ext all',ext['all'][w],'mints cur',len(mints['cur'][w]),'all',len(mints['all'][w]),'first non-cur event',firstev.get((w,'ps')) or firstev.get((w,'excl')))
