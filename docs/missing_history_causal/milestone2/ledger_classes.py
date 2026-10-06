import json,collections,sys
sess=sys.argv[1]
rows=[json.loads(l) for l in open(f'/tmp/mh_recon2/ledger/{sess}.jsonl')]
def show(r): return {k:r[k] for k in ('side','ev_sol','ev_tok','tape_sol','u_sd','u_td','n_ev','n_curve_ix','payer','txfee','res') if k in r}
tm=[r for r in rows if r['cls']=='trader_mismatch']
c=collections.Counter()
for r in tm:
    # who is consistent with the event? event user token delta vs event qty ; tape trader unknown token delta here
    c[('user_td==ev_tok', r.get('u_td') is not None and abs(r['u_td'])==r['ev_tok'], 'n_ev>1', r['n_ev']>1, 'res', r.get('res'))]+=1
print('TRADER MISMATCH',len(tm)); [print(' ',n,k) for k,n in c.most_common(8)]
eo=[r for r in rows if r['cls']=='event_only' and not (r.get('vs')==0)]
c=collections.Counter()
for r in eo:
    c[(r.get('amt') or r.get('why') or '-', 'n_ev>1' if r['n_ev']>1 else 'n_ev=1', 'payer' if r.get('payer') else 'notpayer', 'tokmatch' if r.get('u_td') is not None and abs(r['u_td'])==r['ev_tok'] else 'tokdiff')]+=1
print('EVENT_ONLY (non-zero-reserve)',len(eo)); [print(' ',n,k) for k,n in c.most_common(12)]
to=[r for r in rows if r['cls']=='tape_only']
c=collections.Counter((r['n_ev'], r['n_tape'], r.get('n_curve_ix')) for r in to)
print('TAPE_ONLY',len(to)); [print(' ',n,k) for k,n in c.most_common(8)]
print('sample tape_only',[show(r) for r in to[:2]])
print('sample event_only',[show(r) for r in eo[:3]])
mm=[r for r in rows if r['cls']=='matched']
eq=sum(1 for r in mm if abs(r['tape_sol'])==r['ev_sol']); print('MATCHED',len(mm),'tape_sol==ev_sol exactly',eq)
