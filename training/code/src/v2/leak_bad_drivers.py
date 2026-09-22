#!/usr/bin/env python
import glob, numpy as np, pyarrow as pa, pyarrow.parquet as pq
ROOT="/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"
STATE=sorted(glob.glob(f"{ROOT}/pump_state_v3/*.parquet")); CF=sorted(glob.glob(f"{ROOT}/counterfactual_trade_v3/*.parquet"))
PID=[0,1,2]
def load(f,col=None): return pa.concat_tables([pq.read_table(f[i],columns=col) for i in PID],promote_options="default")
def npc(t,n): return t.column(n).to_numpy(zero_copy_only=False)
st=load(STATE); cf=load(CF)
sid=np.array(npc(st,'state_id')); pc={s:i for i,s in enumerate(np.array(npc(cf,'state_id')))}
ks,kc=[],[]
for i,s in enumerate(sid):
    k=pc.get(s)
    if k is not None: ks.append(i);kc.append(k)
ks=np.array(ks);kc=np.array(kc);N=len(ks)
assert N>0
print(f"[N] rows={N}")
ec=np.array(npc(cf,'economic_class'))[kc]
ybad=(ec=='BAD').astype(int)
feat=[c for c in st.schema.names if pa.types.is_integer(st.schema.field(c).type) or pa.types.is_floating(st.schema.field(c).type)]
rows=[]
for c in feat:
    v=npc(st,c)[ks].astype(np.float64)
    m=np.isfinite(v)
    if m.sum()<1000 or np.nanstd(v)<1e-12: continue
    r=float(np.corrcoef(v[m],ybad[m])[0,1])
    if np.isfinite(r): rows.append((abs(r),r,c,int(m.sum())))
rows.sort(reverse=True)
print("[BAD] top-12 |corr| with BAD:")
for a,r,c,n in rows[:12]: print(f"   {c:34s} r={r:+.4f} n={n}")
# is_graduated cross
ig=npc(st,'is_graduated')[ks].astype(bool)
print(f"[BAD] P(BAD|is_graduated=1)={ybad[ig].mean():.4f} n={int(ig.sum())}; P(BAD|is_graduated=0)={ybad[~ig].mean():.4f}")
liq=npc(st,'liquidity_sol')[ks].astype(float)
zom=npc(st,'is_zombie')[ks].astype(bool)
print(f"[BAD] liquidity_sol median: BAD={np.nanmedian(liq[ybad==1]):.4f} nonBAD={np.nanmedian(liq[ybad==0]):.4f}")
print(f"[BAD] is_zombie frac: BAD={zom[ybad==1].mean():.4f} nonBAD={zom[ybad==0].mean():.4f}")