import json
d=json.load(open('/tmp/mh_recon2/slice/band_population.json'))
names={'F':'frozen: all venues + whole-run median band (uses future)','N':'all venues, NO band','C1':'all venues, causal running-median band 10x',
'C2':'all venues, causal band 1000x (production ledger rule)','PF':'pumpfun-only + frozen band','PN':'pumpfun-only, NO band'}
for c,fv in d['match_by_condition'].items():
    tot=sum(v[0] for v in fv.values()); n=sum(v[1] for v in fv.values())
    print(f"{c:3} {names[c]:60} {tot}/{n} ({100*tot/n:.0f}%)  "+' '.join(f"{k.split('_')[0][:4]}{k.split('_')[1][:3]}={v[0]}" for k,v in fv.items()))
print('whole-run band drop per mint (mint, session, trades, dropped, share):')
for r in d['per_mint_whole_run_band_drop']: print(' ',r)
print('per-mint 7-field matches (of 7*clocks):')
for m,v in d['per_mint_total_match'].items(): print(' ',m,v)
