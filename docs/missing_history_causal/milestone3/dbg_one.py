import sys,json,collections
sig=sys.argv[1]; sess='20260909_144906_000490'
import glob
line=None
for f in sorted(glob.glob(f'/tmp/mh_recon2/m1/s1/wire_*.ndjson')):
    for l in open(f):
        if sig in l[:400] and '"transaction"' in l[:80]:
            line=l; break
    if line: break
d=json.loads(line)
open('/tmp/mh_recon2/m1/one.ndjson','w').write(line)
keys=d['account_keys']; m=d['meta']
print('dup keys:',[k[:6] for k,c in collections.Counter(keys).items() if c>1])
W='So11111111111111111111111111111111111111112'
for nm in ('pre_token_balances','post_token_balances'):
    print(nm,[(e['mint'][:6],e['owner'][:6],e['amount']) for e in m[nm]])
tr='9GTj99'
idx=[i for i,k in enumerate(keys) if k.startswith(tr)]
print('trader key idx',idx,'native delta',[m['post_balances'][i]-m['pre_balances'][i] for i in idx])
for i,ix in enumerate(d['instructions']):
    if ix['program_b58']=='6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P': print('ix',i,ix['data_b64'][:12],ix['accounts'][:20])
