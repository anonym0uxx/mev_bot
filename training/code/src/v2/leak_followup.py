#!/usr/bin/env python
"""Focused follow-up: graduation_proximity_pct / seconds_to_graduation diagnostics + class counts."""
import glob, numpy as np, pyarrow as pa, pyarrow.parquet as pq
ROOT = "/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"
STATE = sorted(glob.glob(f"{ROOT}/pump_state_v3/*.parquet"))
OUT = sorted(glob.glob(f"{ROOT}/pump_outcome_v3/*.parquet"))
CF = sorted(glob.glob(f"{ROOT}/counterfactual_trade_v3/*.parquet"))
PID=[0,1,2]
def load(files, cols=None):
    return pa.concat_tables([pq.read_table(files[i], columns=cols) for i in PID], promote_options="default")
def npc(t,n): return t.column(n).to_numpy(zero_copy_only=False)
st=load(STATE); oc=load(OUT); cf=load(CF)
sid_st=np.array(npc(st,'state_id')); 
pos={s:i for i,s in enumerate(np.array(npc(oc,'state_id')))}
ks,ko=[],[]
for i,s in enumerate(sid_st):
    j=pos.get(s)
    if j is not None: ks.append(i);ko.append(j)
ks=np.array(ks);ko=np.array(ko); N=len(ks)
print(f"[JOIN] rows={N}")
assert N>0
gp = npc(st,'graduation_proximity_pct')[ks].astype(float)
stg= npc(st,'seconds_to_graduation')[ks].astype(float)
grad_state = npc(st,'is_graduated')[ks].astype(bool)
mae= npc(oc,'mae_bp')[ko].astype(float)
r300=npc(oc,'ret_300s')[ko].astype(float)
grad_after=npc(oc,'graduated_after_state')[ko].astype(bool)
stg_out = npc(oc,'seconds_to_graduation')[ko].astype(float)
surv300=npc(oc,'survived_300s')[ko].astype(bool)
def r(x,y):
    m=np.isfinite(x)&np.isfinite(y)
    return float(np.corrcoef(x[m],y[m])[0,1]), int(m.sum())
for nm,x,y in [('gp vs mae_bp',gp,mae),('gp vs ret_300s',gp,r300),
               ('state.seconds_to_grad vs out.graduated_after',stg,grad_after.astype(float)),
               ('state.seconds_to_grad vs out.seconds_to_grad',stg,stg_out),
               ('state.is_graduated vs out.graduated_after',grad_state.astype(float),grad_after.astype(float)),
               ('gp vs out.graduated_after',gp,grad_after.astype(float)),
               ('gp vs survived_300s',gp,surv300.astype(float))]:
    rr,n=r(np.asarray(x,float),np.asarray(y,float)); print(f"  {nm:52s} r={rr:+.4f} n={n}")
# bucket mae by gp
print("\n[gp buckets] bucket  n  mean_mae_bp  median_mae  mean_ret300  frac_grad_after")
bins=[0,.1,.3,.5,.7,.9,.99,1.11]
for i in range(len(bins)-1):
    m=(gp>=bins[i])&(gp<bins[i+1])
    if m.sum()==0: continue
    print(f"  [{bins[i]:.2f},{bins[i+1]:.2f}) n={int(m.sum()):7d} mean_mae={np.nanmean(mae[m]):9.1f} med_mae={np.nanmedian(mae[m]):9.1f} mean_ret300={np.nanmean(r300[m]):+.4f} frac_grad_after={grad_after[m].mean():.4f}")
print(f"gp quantiles: {np.nanpercentile(gp,[0,25,50,75,90,99,100])}")
print(f"is_graduated(state) True count={int(grad_state.sum())}; graduated_after True={int(grad_after.sum())}")
# seconds_to_graduation state: how many are exactly a function of curve?
print(f"state.seconds_to_graduation quantiles: {np.nanpercentile(stg[np.isfinite(stg)],[0,50,90,99,100])}")
# economic class counts
posc={s:i for i,s in enumerate(np.array(npc(cf,'state_id')))}
kc=[]
for s in sid_st:
    j=posc.get(s)
    if j is not None: kc.append(j)
kc=np.array(kc)
ec=np.array(npc(cf,'economic_class'))[kc]
exitr=np.array(npc(cf,'exit_reason'))[kc]
import collections
print(f"\n[CF] economic_class counts (n={len(ec)}):")
for k,v in collections.Counter(ec).most_common(): print(f"   {k}: {v} ({v/len(ec)*100:.1f}%)")
print(f"[CF] exit_reason counts:")
for k,v in collections.Counter(exitr).most_common(): print(f"   {k}: {v} ({v/len(ec)*100:.1f}%)")
# what predicts BAD?
bad=(ec=='BAD')
print(f"[CF] BAD frac={bad.mean():.4f}")
# is_graduated among BAD
print(f"[CF] among BAD: frac is_graduated(state)={grad_state[bad].mean():.4f}, frac grad_after={grad_after[bad].mean():.4f}")
print(f"[CF] among non-BAD: frac is_graduated(state)={grad_state[~bad].mean():.4f}, frac grad_after={grad_after[~bad].mean():.4f}")