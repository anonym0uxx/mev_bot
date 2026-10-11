import json,re
rows=[json.loads(l) for l in open('/tmp/mh_recon2/slice/s1_51nH.rust_seed_band.jsonl')]
out=[]
for r in rows:
    u=r['prompt']
    a=re.search('price_lamports_per_raw_token=([0-9.e+-]+)',u)
    b=re.search('curve_price_sol_per_raw_token=([0-9.e+-]+)',u)
    if a and b:
        trader=float(a.group(1)); reserve=float(b.group(1))*1e9
        out.append((r['t'],trader/reserve))
print(len(out),'clocks; trader/reserve price ratio min %.4f median %.4f max %.4f'%(min(x[1] for x in out),sorted(x[1] for x in out)[len(out)//2],max(x[1] for x in out)))
print('share with trader price BELOW reserve:',sum(1 for x in out if x[1]<1)/len(out))
