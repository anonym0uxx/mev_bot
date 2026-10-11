"""Rust corpus_rows resolver vs corpus tape (pumpfun rows of renormalize_raw.py == v7 on these sigs).
Match key: (signature, side, mint, trader, tokens_raw). Reports per-row equality of sol_lamports and unmatched sets, with
rejection accounting (REJ rows in Rust vs tape-missing)."""
import sys,json,collections
sess,tsv,ref=sys.argv[1:4]
tape=collections.defaultdict(list)
for l in open(ref):
    d=json.loads(l); tape[d['signature']].append(d)
rust=collections.defaultdict(list); rej=collections.Counter(); nobal=0
for l in open(tsv):
    f=l.rstrip('\n').split('\t')
    if f[0]=='NOBAL': nobal+=1; continue
    if f[0]=='REJ': rej[f[1]]+=1; continue
    sig,o,side,mint,tr,sol,tok,via=f
    rust[sig].append(dict(ord=int(o),side=side,mint=mint,trader=tr,sol=int(sol),tok=int(tok),via=int(via)))
# parts restriction: only signatures Rust saw (parts 0..19)
sigs=set(rust)|set(rej)
c=collections.Counter(); bad=[]; lab=collections.Counter()
for sig in sigs:
    T=[(t['side'],t['mint'],t['trader'],t['tokens_raw'],t['sol_lamports'],t['resolution']) for t in tape.get(sig,[])]
    R=[(r['side'],r['mint'],r['trader'],r['tok'],r['sol'],'net_position' if r['via'] else 'instruction_accounts') for r in rust.get(sig,[])]
    # the corpus emits one row per instruction, as does Rust; compare as multisets
    Tm=collections.Counter(T); Rm=collections.Counter(R)
    Tv=collections.Counter(x[:5] for x in T); Rv=collections.Counter(x[:5] for x in R)
    if Tm==Rm: c['sig_identical']+=1
    elif Tv==Rv: c['sig_values_identical_label_differs']+=1; lab[(sorted(Tm-Rm)[0][5],sorted(Rm-Tm)[0][5])]+=1
    else:
        c['sig_value_differs']+=1
        if len(bad)<8: bad.append((sig[:10],sorted(Tm-Rm),sorted(Rm-Tm),rej.get(sig,0)))
nrust=sum(len(v) for v in rust.values()); ntape=sum(len(tape.get(s,[])) for s in sigs)
print(sess,'sigs compared',len(sigs),'rust rows',nrust,'tape rows (same sigs)',ntape,'rust REJ instructions',sum(rej.values()),'NOBAL lines',nobal)
print(dict(c)); print('label flips',dict(lab))
for b in bad: print('  DIFF',b)
