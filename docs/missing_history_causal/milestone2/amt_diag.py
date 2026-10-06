import json,collections,sys
sess=sys.argv[1]
c=collections.Counter(); ex=[]
for l in open(f'/tmp/mh_recon2/ledger/{sess}.jsonl'):
    r=json.loads(l)
    if r['cls']!='matched' or r['amt']!='other_transfer_or_multi_ix': continue
    s=r['ev_sol']; t=abs(r['tape_sol']); fee=r['fee'] or 0; cf=r['cfee'] or 0
    rel=[]
    if r['side']=='buy' and t==s+fee+cf: rel.append('buy:sol+fee+cfee')
    if r['side']=='sell' and t==s-fee-cf: rel.append('sell:sol-fee-cfee')
    if t==s+fee: rel.append('sol+fee')
    if t==s+cf: rel.append('sol+cfee')
    if t==s: rel.append('sol')
    if t==s-fee: rel.append('sol-fee')
    c[tuple(rel) or ('none',)]+=1
    c[('side',r['side'],'multi' if r['n_ev']>1 or r['n_curve_ix']>1 else 'single')]+=1
    if not rel and len(ex)<8: ex.append((r['side'],'ev_sol',s,'fee',fee,'cfee',cf,'tape',r['tape_sol'],'u_sd',r['u_sd'],'txfee',r['txfee'],'n_ev',r['n_ev'],'payer',r['payer']))
for k,v in c.most_common(16): print(v,k)
for e in ex: print(e)
