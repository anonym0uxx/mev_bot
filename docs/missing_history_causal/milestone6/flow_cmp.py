import json,collections,sys
M='51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump'
enr={}
for l in open('/training/v2/reports/C11_FLOW_ENRICHMENT.jsonl'):
    if M in l[:120]:
        r=json.loads(l)
        if r['mint']==M: enr[r['t_dec_ms']]=r
F=('entrants_60s','entrants_300s','net_flow_sol_300s','fresh_wallet_share_300s','flow_lookback_d','sniper_share_300s','bot_uniform_share_300s','smart_entrants_300s','smart_net_flow_sol_300s','coentry_wallets_300s')
for name in sys.argv[1:]:
    rows={json.loads(l)['t']:json.loads(l) for l in open(name)}
    c=collections.Counter(); ex=[]
    for t,r in rows.items():
        e=enr[t]
        for k in F:
            a,b=r.get(k),e.get(k)
            if a is None and b is None: continue
            ok = (a is not None and b is not None and abs(float(a)-float(b))<1e-9)
            if not ok: c[k]+=1; ex.append((t,k,a,b))
    print(name.split('/')[-1],'clocks',len(rows),'field mismatches',dict(c))
    for x in ex[:4]: print('   ',x)
