import sys,json,glob,base64
sig=sys.argv[1]
d=None
for f in sorted(glob.glob('/tmp/mh_recon2/m1/s1/wire_*.ndjson')):
    for l in open(f):
        if sig in l[:400]:
            x=json.loads(l)
            if x.get('signature_b58')==sig: d=x; break
    if d: break
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
for i,ix in enumerate(d['instructions']):
    p=ix['program_b58'][:6]
    data=base64.b64decode(ix['data_b64'])
    if p=='6EF8rr': print(i,'PUMP',data[:8].hex(),len(data),ix['accounts'][:4])
    elif p not in('Comput','111111','Tokenk','AToken'): print(i,p,data[:8].hex())
b=d['meta']['balances'] if 'balances' in d['meta'] else d['meta']
print('pre_tok mints',sorted({e['mint'][:6] for e in b['pre_token_balances']}),'post',sorted({e['mint'][:6] for e in b['post_token_balances']}))
