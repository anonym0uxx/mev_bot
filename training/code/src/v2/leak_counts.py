#!/usr/bin/env python
import glob, numpy as np, collections, pyarrow as pa, pyarrow.parquet as pq
ROOT="/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"
PID=[0,1,2]
def load(layer,cols=None):
    f=sorted(glob.glob(f"{ROOT}/{layer}/*.parquet"))
    return pa.concat_tables([pq.read_table(f[i],columns=cols) for i in PID],promote_options="default")
def npc(t,n): return t.column(n).to_numpy(zero_copy_only=False)
oc=load('pump_outcome_v3'); cf=load('counterfactual_trade_v3')
print(f"[ROWS] outcome={oc.num_rows} cf={cf.num_rows}")
for c in ['ret_1s','ret_5s','ret_30s','ret_300s','mfe_bp','mae_bp']:
    v=npc(oc,c).astype(float); print(f"  {c:9s} finite={int(np.isfinite(v).sum())} ({np.isfinite(v).mean()*100:.1f}%)")
for c in ['has_trade_within_1s','has_trade_within_30s','has_trade_within_300s','right_censored_300s','observed_through_300s']:
    v=npc(oc,c); print(f"  {c:22s} true={int(np.sum(v.astype(float)))}")
ec=np.array(npc(cf,'economic_class')); rs=np.array(npc(cf,'economic_class_reason'))
print("\n[economic_class_reason by class]")
for cls in ['BAD','TOXIC','SKIP','STRONG','GOOD','MARGINAL']:
    m=ec==cls
    if m.sum()==0: continue
    top=collections.Counter(rs[m]).most_common(3)
    print(f"  {cls:9s} n={int(m.sum()):7d} reasons={top}")
# BAD vs liquidity in state
st=load('pump_state_v3'); sid=np.array(npc(st,'state_id'))
po={s:i for i,s in enumerate(np.array(npc(cf,'state_id')))}
ks=np.array([i for i,s in enumerate(sid) if s in po]); kc=np.array([po[s] for s in sid if s in po])
liq=npc(st,'liquidity_sol')[ks].astype(float); bad=(ec[kc]=='BAD')
print(f"\n[BAD vs liquidity_sol] BAD mean_liq={np.nanmean(liq[bad]):.4f} median={np.nanmedian(liq[bad]):.4f}; nonBAD mean={np.nanmean(liq[~bad]):.4f} median={np.nanmedian(liq[~bad]):.4f}")
ef=npc(cf,'exit_feasible')[kc].astype(bool)
print(f"  frac exit_feasible=True: BAD={ef[bad].mean():.4f} nonBAD={ef[~bad].mean():.4f}")
# does BAD == ~exit_feasible?
print(f"  BAD & !exit_feasible: {int((bad&~ef).sum())}; BAD & exit_feasible: {int((bad&ef).sum())}; !BAD & !exit_feasible: {int((~bad&~ef).sum())}")