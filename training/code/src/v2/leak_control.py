#!/usr/bin/env python
"""Control: does removing suspect columns change derivability? + exact missingness-leak overlap."""
import glob, numpy as np, pyarrow as pa, pyarrow.parquet as pq
ROOT="/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"
STATE=sorted(glob.glob(f"{ROOT}/pump_state_v3/*.parquet"))
OUT=sorted(glob.glob(f"{ROOT}/pump_outcome_v3/*.parquet"))
CF=sorted(glob.glob(f"{ROOT}/counterfactual_trade_v3/*.parquet"))
PID=[0,1,2]
def load(f,cols=None): return pa.concat_tables([pq.read_table(f[i],columns=cols) for i in PID],promote_options="default")
def npc(t,n): return t.column(n).to_numpy(zero_copy_only=False)
st=load(STATE); oc=load(OUT); cf=load(CF)
sid=np.array(npc(st,'state_id'))
po={s:i for i,s in enumerate(np.array(npc(oc,'state_id')))}
ks=np.array([i for i,s in enumerate(sid) if s in po])
ko=np.array([po[s] for s in sid if s in po]); N=len(ks)
print(f"[JOIN] rows={N}"); assert N>0

gp=npc(st,'graduation_proximity_pct')[ks].astype(float)
stg=npc(st,'seconds_to_graduation')[ks].astype(float)
grad_after=npc(oc,'graduated_after_state')[ko].astype(bool)
stg_out=npc(oc,'seconds_to_graduation')[ko].astype(float)
print(f"[MISSINGNESS LEAK]")
print(f"  gp finite         = {int(np.isfinite(gp).sum())}")
print(f"  seconds_to_grad finite = {int(np.isfinite(stg).sum())}")
print(f"  graduated_after True   = {int(grad_after.sum())}")
print(f"  gp finite & grad_after: {int((np.isfinite(gp)&grad_after).sum())}   gp finite & !grad_after: {int((np.isfinite(gp)&~grad_after).sum())}")
print(f"  stg finite & grad_after:{int((np.isfinite(stg)&grad_after).sum())}  stg finite & !grad_after:{int((np.isfinite(stg)&~grad_after).sum())}")
m=np.isfinite(stg)&np.isfinite(stg_out)
print(f"  stg == stg_out (on finite, n={int(m.sum())}): max|diff|={np.max(np.abs(stg[m]-stg_out[m])):.6g}")
# does presence of stg alone recover grad_after?
pred=np.isfinite(stg).astype(int)
acc=(pred==grad_after.astype(int)).mean()
print(f"  'stg is non-null' as predictor of graduated_after: acc={acc:.4f} (baseline={max(grad_after.mean(),1-grad_after.mean()):.4f}) prec={ (grad_after[pred==1].mean()):.4f}")

feat_all=[c for c in st.schema.names if pa.types.is_integer(st.schema.field(c).type) or pa.types.is_floating(st.schema.field(c).type)]
SUSPECT=['seconds_to_graduation','graduation_proximity_pct','is_graduated']
feat_pruned=[c for c in feat_all if c not in SUSPECT]
import pyarrow.compute as pc
def nonconst(cols):
    out=[]
    for c in cols:
        if len(pc.unique(st.column(c)).to_pylist())>1: out.append(c)
    return out
feat_all=nonconst(feat_all); feat_pruned=nonconst(feat_pruned)
def auc(y,s):
    y=np.asarray(y);s=np.asarray(s,float)
    pos=s[y==1];neg=s[y==0]
    if len(pos)==0 or len(neg)==0: return float('nan')
    a=np.concatenate([pos,neg]);o=a.argsort();rk=np.empty(len(a));rk[o]=np.arange(1,len(a)+1)
    _,inv,cnt=np.unique(a,return_inverse=True,return_counts=True);sr=np.zeros(len(cnt));np.add.at(sr,inv,rk);av=(sr/cnt)[inv]
    n1,n0=len(pos),len(neg);return float((av[:n1].sum()-n1*(n1+1)/2)/(n1*n0))
def lfit(X,y,it=500,lr=.5,l2=1e-3):
    n,d=X.shape;w=np.zeros(d);b=0.
    for _ in range(it):
        p=1/(1+np.exp(-np.clip(X@w+b,-30,30)));g=p-y
        w-=lr*(X.T@g/n+l2*w);b-=lr*g.mean()
    return w,b
def build(cols,idx):
    X=np.empty((len(idx),len(cols)))
    for k,c in enumerate(cols): X[:,k]=npc(st,c)[idx].astype(float)
    mu=np.nanmean(X,0);sd=np.nanstd(X,0);sd[sd<1e-9]=1
    X=np.where(np.isfinite(X),X,mu);return (X-mu)/sd
mint=np.array(npc(st,'mint'))
mh=np.array([hash(s)%5 for s in mint[:N]])
tr=mh!=0;te=mh==0
po2={s:i for i,s in enumerate(np.array(npc(cf,'state_id')))}
kc=np.array([po2[s] for s in sid if s in po2])
ec=np.array(npc(cf,'economic_class'))
ecj=ec[kc]
# align kc to N by state order (sid order) - same as ks
for label,feats in [('ALL',feat_all),('PRUNED',feat_pruned)]:
    for tname,y in [('ret300>0',(npc(oc,'ret_300s')[ko].astype(float)>0).astype(int)),
                    ('netret>0',(npc(cf,'net_return_bp')[kc].astype(float)>0).astype(int)),
                    ('BAD',(ecj=='BAD').astype(int))]:
        Xtr=build(feats,ks[tr]);Xte=build(feats,ks[te])
        w,b=lfit(Xtr,y[tr]);p=1/(1+np.exp(-np.clip(Xte@w+b,-30,30)))
        a=auc(y[te],p);ac=((p>0.5).astype(int)==y[te]).mean()
        print(f"  [{label:6s}] {tname:9s} nfeat={len(feats):3d} AUC={a:.4f} acc={ac:.4f} base={max(y[te].mean(),1-y[te].mean()):.4f}")