"""Classify remaining Rust-vs-corpus diffs at each clock: does the BAND set (corpus lookahead filter) reproduce the corpus value exactly?
Compares rust counts to (tape<=t) and corpus counts to (band<=t)."""
import json,sys,statistics,re
M=sys.argv[1]; sess=sys.argv[2]; diff=json.load(open(sys.argv[3]))
rows=[json.loads(l) for l in open(f'/tmp/mh_recon2/ref/{sess}/pf.jsonl') if M in l]
rows=[r for r in rows if r['mint']==M]
fl=[r for r in rows if abs(r['sol_lamports'])>=1e5 and abs(r['tokens_raw'])>=1e6]
px=[abs(r['sol_lamports'])/abs(r['tokens_raw']) for r in fl]; med=statistics.median(px)
band=[r for r,p in zip(fl,px) if med/10<=p<=med*10]
bandset={id(r) for r in band}
tot=0;ok_causal=0;ok_band=0
for x in diff:
    t=x['t']; d={f['field']:f for f in x.get('diffs',[])}
    c=[r for r in fl if r['recv_unix_ms']<=t]; b=[r for r in band if r['recv_unix_ms']<=t]
    exp_c=len(c); exp_b=len(b)
    rust_n=int(d['n_prior_trades']['rust']) if 'n_prior_trades' in d else exp_b
    corp_n=int(d['n_prior_trades']['corpus']) if 'n_prior_trades' in d else exp_b
    out=[t, 'rust_n',rust_n,'causal_floors_n',exp_c,'| corpus_n',corp_n,'band_n',exp_b, 'dropped_by_band',exp_c-exp_b]
    tot+=1; ok_causal+= rust_n==exp_c; ok_band+= corp_n==exp_b
    print(*out)
print('clocks',tot,'rust==causal',ok_causal,'corpus==band',ok_band)
