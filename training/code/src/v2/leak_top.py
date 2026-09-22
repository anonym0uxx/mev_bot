#!/usr/bin/env python
import glob, numpy as np, pyarrow as pa, pyarrow.parquet as pq
ROOT="/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"; PID=[0,1,2]
def load(layer,cols=None):
    f=sorted(glob.glob(f"{ROOT}/{layer}/*.parquet"))
    return pa.concat_tables([pq.read_table(f[i],columns=cols) for i in PID],promote_options="default")
def npc(t,n): return t.column(n).to_numpy(zero_copy_only=False)
st=load('pump_state_v3'); oc=load('pump_outcome_v3')
sid=np.array(npc(st,'state_id')); po={s:i for i,s in enumerate(np.array(npc(oc,'state_id')))}
ks=np.array([i for i,s in enumerate(sid) if s in po]); ko=np.array([po[s] for s in sid if s in po]); N=len(ks)
print(f"[JOIN] rows={N}")
num=[c for c in st.schema.names if pa.types.is_integer(st.schema.field(c).type) or pa.types.is_floating(st.schema.field(c).type)]
FUT=['ret_1s','ret_5s','ret_30s','ret_300s','mfe_bp','mae_bp']
X={c:npc(st,c)[ks].astype(float) for c in num}
for fc in FUT:
    y=npc(oc,fc)[ko].astype(float)
    rows=[]
    fn=int(np.isfinite(y).sum())
    for c in num:
        x=X[c];m=np.isfinite(x)&np.isfinite(y)
        if m.sum()<1000: continue
        xs,ys=x[m],y[m]
        if xs.std()<1e-12 or ys.std()<1e-12: continue
        r=float(np.corrcoef(xs,ys)[0,1])
        if np.isfinite(r): rows.append((abs(r),r,c,int(m.sum())))
    rows.sort(reverse=True)
    print(f"\n=== {fc} (finite n={fn}) top5:")
    for a,r,c,n in rows[:5]: print(f"   {c:34s} r={r:+.4f} |r|={a:.4f} n={n}")
    print(f"   MAX |r| over all {len(rows)} state cols = {rows[0][0]:.4f} ({rows[0][2]})")