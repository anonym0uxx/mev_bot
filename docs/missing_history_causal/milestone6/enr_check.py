import json,re,sys
M='51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump'
enr={}
for l in open('/training/v2/reports/C11_FLOW_ENRICHMENT.jsonl'):
    if M in l[:120]:
        r=json.loads(l)
        if r['mint']==M: enr[r['t_dec_ms']]=r
print('enrichment rows for mint',len(enr))
cl=[json.loads(l) for l in open('/training/v2/reports/C11_CLOCKS.jsonl') if M in l]
print('clock rows',len(cl),cl[0] if cl else None)
bad=0;n=0
for fn in ('examination','validation','train'):
    for l in open(f'/training/v2/candidate_sft_c12_entry/{fn}.jsonl'):
        if M not in l[:500]: continue
        r=json.loads(l); u=[m for m in r['messages'] if m['role']=='user'][0]['content']
        t=re.search(r't_dec_ms=(\d+)',u)
        if not t or int(t.group(1)) not in enr: continue
        line=re.search(r'LIVE FLOW STATE: (.*)',u).group(1)
        kv=dict(x.split('=',1) for x in line.split())
        e=enr[int(t.group(1))]; n+=1
        for k in ('entrants_300s','smart_entrants_300s','coentry_wallets_300s','flow_lookback_d','smart_net_flow_sol_300s','net_flow_sol_300s'):
            if str(e.get(k))!=kv[k] and float(kv[k])!=float(e.get(k)): bad+=1; print('MISMATCH',t.group(1),k,e.get(k),kv[k])
print('prompts checked',n,'field mismatches vs persisted enrichment',bad)
