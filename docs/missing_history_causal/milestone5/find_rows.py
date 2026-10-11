import json,re,sys,collections
S={'S1':(1788965346866,1788972546869),'S2':(1788975568113,1788982768122)}
out=collections.defaultdict(list)
for fn in ('train','validation','examination'):
    for l in open(f'/training/v2/candidate_sft_c12_entry/{fn}.jsonl'):
        m=re.search(r't_dec_ms=(\d+)',l)
        if not m: continue
        t=int(m.group(1))
        for k,(a,b) in S.items():
            if a<=t<=b:
                out[(k,fn)].append(l); break
for k,v in out.items(): print(k,len(v))
if out:
    r=json.loads(next(iter(out.values()))[0]); print(list(r.keys()))
    u=[m for m in r['messages'] if m['role']=='user'][0]['content']; print(u[:3500])
    json.dump({f'{k[0]}|{k[1]}':[json.loads(x) for x in v[:400]] for k,v in out.items()},open('/tmp/mh_recon2/slice_rows.json','w'))
