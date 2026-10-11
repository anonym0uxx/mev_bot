import json,collections,sys
sess=sys.argv[1]
c=collections.Counter(); resid=[]; big=0
for l in open(f'/tmp/mh_recon2/ledger/{sess}.jsonl'):
    r=json.loads(l)
    if r['cls']!='matched': continue
    c['matched']+=1
    c['tape==user_balance_delta' if r['tape_sol']==r['u_sd'] else 'tape!=user_balance_delta']+=1
    # swap quantity net of curve fees (what the user gets/pays in the swap itself)
    fee=(r['fee'] or 0)+(r['cfee'] or 0)
    swap=-(r['ev_sol']+fee) if r['side']=='buy' else (r['ev_sol']-fee)
    d=r['tape_sol']-swap          # everything else in the wallet delta
    nonswap=d+(r['txfee'] if r['payer'] else 0)   # remove tx fee when the user pays it
    resid.append((abs(nonswap), r['ev_sol']))
    c['nonswap==0' if nonswap==0 else 'nonswap!=0']+=1
    if nonswap!=0:
        if abs(nonswap)<=r['txfee']*40+10_000_000: c['nonswap<=40*txfee+0.01SOL']+=1
        else: c['nonswap_large']+=1
    c['multi' if (r['n_ev']>1 or r['n_curve_ix']>1) else 'single']+=1
    c[('tape_sol_abs_vs_event_sol', 'tape>event' if abs(r['tape_sol'])>r['ev_sol'] else 'tape<=event')]+=1
for k,v in sorted(c.items(),key=str): print(v,k)
rs=sorted(x[0] for x in resid)
n=len(rs); print('nonswap abs quantiles lamports p50',rs[n//2],'p90',rs[9*n//10],'p99',rs[99*n//100],'max',rs[-1])
rel=sorted(x[0]/x[1] for x in resid if x[1]>0); m=len(rel)
print('nonswap/event_sol p50',round(rel[m//2],4),'p90',round(rel[9*m//10],4),'p99',round(rel[99*m//100],4))
