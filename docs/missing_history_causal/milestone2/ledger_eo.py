import json,collections,sys
sess=sys.argv[1]
rows=[json.loads(l) for l in open(f'/tmp/mh_recon2/ledger/{sess}.jsonl')]
eo=[r for r in rows if r['cls']=='event_only' and r.get('vs')!=0]
print('event_only non-zero-reserve',len(eo))
print('by n_curve_ix',dict(collections.Counter(r['n_curve_ix'] for r in eo)))
print('n_curve_ix==0 and u_td==ev_tok(user moved exactly event qty):',sum(1 for r in eo if r['n_curve_ix']==0 and r.get('u_td') is not None and abs(r['u_td'])==r['ev_tok']))
print('res field of event_only',dict(collections.Counter(str(r.get('res')) for r in eo).most_common(5)))
mm=[r for r in rows if r['cls']=='matched']
import statistics
d=[abs(r['tape_sol'])-r['ev_sol'] for r in mm]
rel=sorted(abs(x)/max(1,r['ev_sol']) for x,r in zip(d,mm))
q=lambda p: rel[int(p*(len(rel)-1))]
print('matched |tape|-ev_sol relative: p50 %.4f p90 %.4f p99 %.4f'%(q(.5),q(.9),q(.99)),' exact',sum(1 for x in d if x==0),'of',len(d))
# does buy tape include tx fee? sign of (|tape|-ev_sol-fees)
print('sell tape > ev_sol - fee : ',sum(1 for r in mm if r['side']=='sell' and r['tape_sol']>r['ev_sol']-r['fee']-r['cfee']-5),'of',sum(1 for r in mm if r['side']=='sell'))
