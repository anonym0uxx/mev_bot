#!/usr/bin/env python
import glob, numpy as np, pyarrow as pa, pyarrow.parquet as pq, collections
ROOT="/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"
STATE=sorted(glob.glob(f"{ROOT}/pump_state_v3/*.parquet")); OUT=sorted(glob.glob(f"{ROOT}/pump_outcome_v3/*.parquet")); CF=sorted(glob.glob(f"{ROOT}/counterfactual_trade_v3/*.parquet"))
PID=[0,1,2]
def load(f,col=None): return pa.concat_tables([pq.read_table(f[i],columns=col) for i in PID],promote_options="default")
def npc(t,n): return t.column(n).to_numpy(zero_copy_only=False)
st=load(STATE); oc=load(OUT); cf=load(CF)
sid=np.array(npc(st,'state_id'))
po={s:i for i,s in enumerate(np.array(npc(oc,'state_id')))}; pc={s:i for i,s in enumerate(np.array(npc(cf,'state_id')))}
ks,ko,kc=[],[],[]
for i,s in enumerate(sid):
    j=po.get(s); k=pc.get(s)
    if j is not None and k is not None: ks.append(i);ko.append(j);kc.append(k)
ks=np.array(ks);ko=np.array(ko);kc=np.array(kc);N=len(ks)
assert N>0
print(f"[N] joined all three={N}")
stg=npc(st,'seconds_to_graduation')[ks].astype(float)
gp =npc(st,'graduation_proximity_pct')[ks].astype(float)
ret=npc(oc,'ret_300s')[ko].astype(float)
ec=np.array(npc(cf,'economic_class'))[kc]
present=np.isfinite(stg)  # future-graduation indicator leak
print(f"[LEAK-STRENGTH] state.seconds_to_graduation present (== future graduation) rows={int(present.sum())} frac={present.mean():.4f}")
y=(ret>0).astype(int)
def auc(y,s):
    y=np.asarray(y);s=np.asarray(s,float);p=s[y==1];n=s[y==0]
    if len(p)==0 or len(n)==0: return float('nan')
    a=np.concatenate([p,n]);o=a.argsort();rk=np.empty(len(a));rk[o]=np.arange(1,len(a)+1)
    _,inv,c=np.unique(a,return_inverse=True,return_counts=True);sr=np.zeros(len(c));np.add.at(sr,inv,rk);av=(sr/c)[inv]
    return float((av[:len(p)].sum()-len(p)*(len(p)+1)/2)/(len(p)*len(n)))
print(f"[LEAK-STRENGTH] AUC of ret_300s>0 using ONLY nullity-of-seconds_to_graduation = {auc(y,present.astype(float)):.4f}")
for lab,m in [('graduation-present',present),('graduation-absent',~present)]:
    print(f"   {lab}: n={int(m.sum())} mean_ret300={np.nanmean(ret[m]):+.4f} med={np.nanmedian(ret[m]):+.4f} frac_ret300>0={np.nanmean((ret[m]>0)):.4f}")
print("   economic_class dist | graduation-present:", dict(collections.Counter(ec[present])))
print("   economic_class dist | graduation-absent :", dict(collections.Counter(ec[~present])))
# BAD class top weights (refit small)
feat=[c for c in st.schema.names if pa.types.is_integer(st.schema.field(c).type) or pa.types.is_floating(st.schema.field(c).type)]
keep=[c for c in feat if len(set(npc(st,c)[ks][:50000].tolist()))>1]
X=np.empty((N,len(keep)))
for k,c in enumerate(keep): X[:,k]=npc(st,c)[ks].astype(float)
mu=np.nanmean(X,0);sd=np.nanstd(X,0);sd[sd<1e-9]=1;X=np.where(np.isfinite(X),X,mu);X=(X-mu)/sd
ybad=(ec=='BAD').astype(int)
tr=np.arange(N)%5!=0; te=~tr
w=np.zeros(X.shape[1]);b=0
for _ in range(400):
    p=1/(1+np.exp(-np.clip(X[tr]@w+b,-30,30)));g=p-ybad[tr];w-=0.5*(X[tr].T@g/tr.sum()+1e-3*w);b-=0.5*g.mean()
print(f"[BAD] n={N} pos={ybad.mean():.4f} AUC={auc(ybad[te],1/(1+np.exp(-np.clip(X[te]@w+b,-30,30)))):.4f}")
order=np.argsort(-np.abs(w))[:10]
print("[BAD] top-10 |weight| features:", [(keep[i],round(float(w[i]),3)) for i in order])