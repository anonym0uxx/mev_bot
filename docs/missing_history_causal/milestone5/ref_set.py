"""Independent reference: frozen builder's trade set for a mint at t, from the tape (pf.jsonl == v7 pumpfun rows on these sigs).
Applies the builder's rules: status ok, value floors, price band [median/10, median*10] over the WHOLE run (lookahead).
Prints counts/volumes at t for (a) tape<=t (b) +floors (c) +band, to compare with corpus prompt and Rust prompt."""
import json,sys,re,statistics
M=sys.argv[1]; ts=[int(x) for x in sys.argv[2].split(',')]; sess=sys.argv[3]
rows=[json.loads(l) for l in open(f'/tmp/mh_recon2/ref/{sess}/pf.jsonl') if M in l]
rows=[r for r in rows if r['mint']==M]
MIN_SOL,MIN_TOK=1e5,1e6
def floors(r): return abs(r['sol_lamports'])>=MIN_SOL and abs(r['tokens_raw'])>=MIN_TOK
fl=[r for r in rows if floors(r)]
px=[abs(r['sol_lamports'])/abs(r['tokens_raw']) for r in fl]
med=statistics.median(px)
band=[r for r,p in zip(fl,px) if med/10<=p<=med*10]
print('rows',len(rows),'floors',len(fl),'band',len(band),'median px',med)
CORP={}
for t in ts:
    out={}
    for name,S in (('tape',rows),('floors',fl),('band',band)):
        s=[r for r in S if r['recv_unix_ms']<=t]
        b=[r for r in s if r['side']=='buy']; se=[r for r in s if r['side']=='sell']
        out[name]=dict(n=len(s),buys=len(b),sells=len(se),buy_vol=sum(abs(r['sol_lamports']) for r in b),sell_vol=sum(abs(r['sol_lamports']) for r in se),traders=len({r['trader'] for r in s}))
    print(t,json.dumps(out))
