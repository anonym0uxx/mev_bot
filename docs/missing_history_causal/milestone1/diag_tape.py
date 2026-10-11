import json,sys,collections,glob,base64,struct
sess='20260909_144906_000490'
exec(open('compare.py').read().split("# R2 corpus tape")[0].replace("sess=sys.argv[1]","sess='%s'"%sess))
# corpus tape for same slice, per mint, with signature
tape=collections.defaultdict(list)
for l in open(f'/tmp/mh_recon2/ref/{sess}/pf.jsonl'):
    d=json.loads(l)
    if minrecv<=d['recv_unix_ms']<=maxrecv and d['status']=='success' and d['venue']=='pumpfun': tape[d['mint']].append(d)
# event side keyed (sig) -> set of traders
evsig=collections.defaultdict(list)
for m,v in ev.items():
    for e in v: evsig[m].append(e)
c=collections.Counter(); ex=[]
for m in list(tape)[:400]:
    t_ev=collections.Counter((e[0],e[2]) for e in evsig.get(m,[]))
    t_tp=collections.Counter((d['recv_unix_ms'],d['side']) for d in tape[m])
    c['tape_rows']+=sum(t_tp.values()); c['event_rows']+=sum(t_ev.values())
    c['common']+=sum((t_ev&t_tp).values()); c['tape_only']+=sum((t_tp-t_ev).values()); c['event_only']+=sum((t_ev-t_tp).values())
    # trader identity agreement on common (recv,side)
    tr_ev=collections.defaultdict(set)
    for e in evsig.get(m,[]): tr_ev[(e[0],e[2])].add(e[1])
    for d in tape[m]:
        k=(d['recv_unix_ms'],d['side'])
        if k in tr_ev: c['trader_same' if d['trader'] in tr_ev[k] else 'trader_differs']+=1
        if k in tr_ev and d['trader'] not in tr_ev[k] and len(ex)<3: ex.append((d['trader'][:8],[x[:8] for x in tr_ev[k]],d['resolution']))
print(dict(c)); print(ex)
res=collections.Counter(d['resolution'] for v in tape.values() for d in v); print(res)
