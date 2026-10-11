import pickle,collections,json
SESS=['20260909_144906_000490','20260909_173928_000596']
RES={}
for s in SESS:
    S,T,L,O=pickle.load(open(f'/tmp/mh_recon2/work/{s}.v2.pkl','rb'))
    lab=[l.rstrip('\n').split('\t')[4] for l in open(f'/tmp/mh_recon/out_{s}.tsv')]
    assert len(lab)==len(S)
    txsig={t[0]:t for t in T}
    curves=set(x[1] for x in S)
    # curve->mint: exact post-state match of the snapshot's own tx event
    c2m={}
    for x in S:
        t=txsig.get(x[10])
        if t:
            for e in t[8]:
                if e[3:7]==(x[4],x[5],x[6],x[7]): c2m[x[1]]=e[7]
    m2c={v:k for k,v in c2m.items()}
    tl=collections.defaultdict(list)
    for t in T:
        if not t[5]: continue   # failed tx never change state
        for j,e in enumerate(t[8]):
            cv=m2c.get(e[7])
            if cv: tl[cv].append((t[1],t[2],j,t[0],e))
    for cv in tl: tl[cv].sort(key=lambda z:z[:3])
    pos={}
    for cv,v in tl.items():
        for k,z in enumerate(v): pos[(cv,z[4][3:7])]=k
    ev_in_slot=collections.Counter()
    for cv,v in tl.items():
        for z in v: ev_in_slot[(cv,z[0])]+=1
    last={};cls=collections.Counter();allc=collections.Counter();lslot=collections.Counter()
    rebuilt=collections.Counter();missing_hist=collections.Counter();slotgap=collections.Counter()
    unm=collections.Counter();zer=collections.Counter()
    for x,l in zip(S,lab):
        p=last.get(x[1]);last[x[1]]=x
        n=ev_in_slot.get((x[1],x[2]),0)
        lslot[(l,'multi' if n>1 else ('single' if n==1 else 'none'))]+=1
        if not p: continue
        a=pos.get((x[1],(p[4],p[5],p[6],p[7])));b=pos.get((x[1],(x[4],x[5],x[6],x[7])))
        miss=(b-a-1) if (a is not None and b is not None) else None
        allc[(l,'unmatched' if miss is None else ('missing>0' if miss>0 else 'adjacent'))]+=1
        if l!='possible_trade': continue
        if x[5]==0:
            zer[(x[8],p[8])]+=1; cls['B_vtoken==0']+=1;continue
        if miss is None:
            cls['unmatched']+=1
            unm[('curve_has_mint_map' if x[1] in c2m else 'no_mint_map', 'A_event' if a is not None else 'noA','B_event' if b is not None else 'noB')]+=1
            continue
        mids=tl[x[1]][a+1:b+1]
        mids=mids[:-1]  # drop B's own event; remaining are skipped events
        missing_hist[min(miss,6)]+=1
        if miss==0: cls['adjacent_events(no skipped event)']+=1; continue
        inB=all(z[0]==x[2] for z in mids)
        cls['skipped_events_all_in_B_slot' if inB else 'skipped_events_in_earlier_slots']+=1
        vt,vs,rs,rt=p[5],p[4],p[6],p[7]
        for z in tl[x[1]][a+1:b+1]:
            e=z[4];sg=1 if e[2] else -1
            vt-=sg*e[1];rt-=sg*e[1];rs+=sg*e[0];vs+=sg*e[0]
        rebuilt[(vt==x[5] and rt==x[7] and rs==x[6], vs==x[4])]+=1
    RES[s]=dict(n=len(S),labels=dict(collections.Counter(lab)),flag_classes=dict(cls),missing_hist=dict(missing_hist),
      rebuilt_tok_realsol_vsol=dict((str(k),v) for k,v in rebuilt.items()),unmatched=dict((str(k),v) for k,v in unm.items()),zeroed_complete_B_A=dict((str(k),v) for k,v in zer.items()),
      label_x_missing=dict((str(k),v) for k,v in allc.items()),label_x_eventsInBslot=dict((str(k),v) for k,v in lslot.items()))
    print(s);print(json.dumps(RES[s],indent=1))
json.dump(RES,open('/tmp/mh_recon2/results.json','w'),indent=1)

# ---- part 2: direction mix + unmatched characterization
for s in SESS:
    S,T,L,O=pickle.load(open(f'/tmp/mh_recon2/work/{s}.v2.pkl','rb'))
    lab=[l.rstrip('\n').split('\t')[4] for l in open(f'/tmp/mh_recon/out_{s}.tsv')]
    txsig={t[0]:t for t in T}
    by=collections.defaultdict(list)
    for t in T:
        if t[5]:
            for e in t[8]: by[e[7]].append((t[1],t[2],e))
    # slot-level: for (curve,slot) multi-event, directions and net same-sign test, by label of the snapshot
    c2m={}
    for x in S:
        t=txsig.get(x[10])
        if t:
            for e in t[8]:
                if e[3:7]==(x[4],x[5],x[6],x[7]): c2m[x[1]]=e[7]
    sl=collections.defaultdict(list)
    for m,v in by.items():
        for a,b,e in v: sl[(m,a)].append((b,e))
    res=collections.Counter();last={}
    for x,l in zip(S,lab):
        p=last.get(x[1]);last[x[1]]=x
        m=c2m.get(x[1])
        if not p or not m: continue
        ev=sorted(sl.get((m,x[2]),[]),key=lambda z:z[0])
        if len(ev)<2: continue
        dirs=set(e[2] for _,e in ev)
        res[(l,'mixed_dir' if len(dirs)==2 else 'same_dir')]+=1
    print(s,'multi-event slots: label x direction mix',dict(res))
    # unmatched flagged: what are the txs / A,B shape
    last={};u=collections.Counter();ex=[]
    for x,l in zip(S,lab):
        p=last.get(x[1]);last[x[1]]=x
        if l=='possible_trade' and p and x[1] not in c2m:
            t=txsig.get(x[10])
            u[('B_tx_events=%d'%(len(t[8]) if t else -1),'tx_ok' if t and t[5] else 'x','B_complete=%d'%x[8],'vtokenB0' if x[5]==0 else 'nz')]+=1
            if len(ex)<2: ex.append((x[1][:6],p[2:8],x[2:8]))
    print(' unmatched flagged:',dict(u),ex)
