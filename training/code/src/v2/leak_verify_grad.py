#!/usr/bin/env python
import glob, numpy as np, pyarrow as pa, pyarrow.parquet as pq
ROOT="/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"
STATE=sorted(glob.glob(f"{ROOT}/pump_state_v3/*.parquet")); OUT=sorted(glob.glob(f"{ROOT}/pump_outcome_v3/*.parquet"))
PID=[0,1,2]
def load(f,col=None): return pa.concat_tables([pq.read_table(f[i],columns=col) for i in PID],promote_options="default")
def npc(t,n): return t.column(n).to_numpy(zero_copy_only=False)
st=load(STATE); oc=load(OUT)
sid=np.array(npc(st,'state_id')); pos={s:i for i,s in enumerate(np.array(npc(oc,'state_id')))}
ks,ko=[],[]
for i,s in enumerate(sid):
    j=pos.get(s)
    if j is not None: ks.append(i);ko.append(j)
ks=np.array(ks);ko=np.array(ko);N=len(ks)
stg=npc(st,'seconds_to_graduation')[ks].astype(float)
gp =npc(st,'graduation_proximity_pct')[ks].astype(float)
ga =npc(oc,'graduated_after_state')[ko].astype(bool)
ostg=npc(oc,'seconds_to_graduation')[ko].astype(float)
print(f"[CHECK] joined rows={N}")
assert N>0
m_st=(np.isfinite(stg)); m_gp=(np.isfinite(gp)); m_o=(np.isfinite(ostg))
print(f"state.seconds_to_graduation non-null: {int(m_st.sum())}/{N}")
print(f"outcome.seconds_to_graduation non-null: {int(m_o.sum())}/{N}")
print(f"state.graduation_proximity_pct non-null: {int(m_gp.sum())}/{N}")
print(f"outcome.graduated_after True: {int(ga.sum())}/{N}")
# does state non-null => graduated_after True?
print(f"state.stg non-null & graduated_after True: {int((m_st & ga).sum())}  (of {int(m_st.sum())} state non-null)")
print(f"state.stg non-null & graduated_after False: {int((m_st & ~ga).sum())}")
print(f"graduated_after True & state.stg null: {int((ga & ~m_st).sum())}")
# equality where both non-null
both=m_st & m_o
d=np.abs(stg[both]-ostg[both])
print(f"both-non-null={int(both.sum())}  max|state-outhg|={np.nanmax(d) if both.sum() else 'NA'}  frac_exact_equal={float((d==0).mean()) if both.sum() else 'NA'}")
# same for gp
print(f"gp non-null & graduated_after True: {int((m_gp & ga).sum())}  gp non-null & False: {int((m_gp & ~ga).sum())}")
print(f"[CONCLUSION] state.stg non-null is exactly the future-graduation indicator: {bool((m_st==ga).all())}")
print(f"[CONCLUSION] state.stg == outcome.stg verbatim on shared support: {bool(both.sum()>0 and np.nanmax(d)==0)}")