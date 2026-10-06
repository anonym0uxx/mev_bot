#!/usr/bin/env python
"""Causal-leakage audit of slinky_gold_v3_compact (pandas-free: pyarrow + numpy only)."""
import glob, os, json
import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq
import pyarrow.compute as pc

ROOT = "/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact"
STATE = sorted(glob.glob(f"{ROOT}/pump_state_v3/*.parquet"))
OUT = sorted(glob.glob(f"{ROOT}/pump_outcome_v3/*.parquet"))
CF = sorted(glob.glob(f"{ROOT}/counterfactual_trade_v3/*.parquet"))
PART_IDS = [0, 1, 2]
report = {}

def load(files, cols=None):
    tabs = [pq.read_table(files[i], columns=cols) for i in PART_IDS]
    return pa.concat_tables(tabs, promote_options="default")

def col_np(tbl, name):
    a = tbl.column(name)
    return a.to_numpy(zero_copy_only=False)

def schema_map(tbl):
    return {f.name: f.type for f in tbl.schema}

st = load(STATE); oc = load(OUT); cf = load(CF)
print(f"[LOAD] state rows={st.num_rows} cols={st.num_columns}; outcome rows={oc.num_rows} cols={oc.num_columns}; cf rows={cf.num_rows} cols={cf.num_columns}")
assert st.num_rows > 0 and oc.num_rows > 0 and cf.num_rows > 0
report['loaded_rows'] = {'state': st.num_rows, 'outcome': oc.num_rows, 'cf': cf.num_rows}

# ---------- join state x outcome on state_id ----------
sid_st = np.array(col_np(st, 'state_id'))
sid_oc = np.array(col_np(oc, 'state_id'))
oc_pos = {s: i for i, s in enumerate(sid_oc)}
keep_st, keep_oc = [], []
for i, s in enumerate(sid_st):
    j = oc_pos.get(s)
    if j is not None:
        keep_st.append(i); keep_oc.append(j)
keep_st = np.array(keep_st); keep_oc = np.array(keep_oc)
N = len(keep_st)
print(f"[JOIN state x outcome] matched rows={N} (state={st.num_rows}, outcome={oc.num_rows})")
assert N > 0, "STATE x OUTCOME JOIN PRODUCED 0 ROWS"
report['join_state_outcome_rows'] = int(N)

FUT = ['ret_1s', 'ret_5s', 'ret_30s', 'ret_300s', 'mfe_bp', 'mae_bp']
FUT_NP = {f: col_np(oc, f)[keep_oc].astype(np.float64) for f in FUT}

# ---------- join state x counterfactual ----------
sid_cf = np.array(col_np(cf, 'state_id'))
cf_pos = {s: i for i, s in enumerate(sid_cf)}
ks2, kc2 = [], []
for i, s in enumerate(sid_st):
    j = cf_pos.get(s)
    if j is not None:
        ks2.append(i); kc2.append(j)
ks2 = np.array(ks2); kc2 = np.array(kc2)
Nc = len(ks2)
print(f"[JOIN state x cf] matched rows={Nc}")
assert Nc > 0, "STATE x CF JOIN PRODUCED 0 ROWS"
report['join_state_cf_rows'] = int(Nc)

ec_arr = np.array(col_np(cf, 'economic_class'))[kc2]
CLASSES = sorted([c for c in np.unique(ec_arr) if c is not None])
print(f"[CF] economic_class values={CLASSES}  counts={ {c: int((ec_arr==c).sum()) for c in CLASSES} }")
report['economic_classes'] = {str(c): int((ec_arr == c).sum()) for c in CLASSES}
nrb = col_np(cf, 'net_return_bp')[kc2].astype(np.float64)
feas = col_np(cf, 'exit_feasible')[kc2]

# =====================================================================
# 1) COLUMN-BY-COLUMN LEAK TEST
# =====================================================================
sm = schema_map(st)
num_state_cols = [c for c in st.schema.names
                  if pa.types.is_integer(sm[c]) or pa.types.is_floating(sm[c])]
res = []
examined = 0
for cname in num_state_cols:
    x = col_np(st, cname)[keep_st].astype(np.float64)
    for fc in FUT:
        y = FUT_NP[fc]
        m = np.isfinite(x) & np.isfinite(y)
        n = int(m.sum()); examined += n
        if n < 1000:
            continue
        xs, ys = x[m], y[m]
        if xs.std() < 1e-12 or ys.std() < 1e-12:
            r = 0.0
        else:
            r = float(np.corrcoef(xs, ys)[0, 1])
        if np.isfinite(r):
            res.append((abs(r), r, cname, fc, n))
res.sort(reverse=True)
print(f"\n[LEAK-TEST] pairs computed={len(res)} from numeric_state_cols={len(num_state_cols)} joined_rows={N}")
assert len(res) > 0
report['corr_pairs_computed'] = len(res)
report['corr_numeric_state_cols'] = len(num_state_cols)
top25 = res[:25]
print("--- TOP 25 (state_col x future_col  r  |r|  n) ---")
for a, r, c1, c2, n in top25:
    print(f"  {c1:38s} x {c2:9s} r={r:+.4f} |r|={a:.4f} n={n}")
report['top25'] = [f"{c1} x {c2}: r={r:+.4f} |r|={a:.4f} n={n}" for a, r, c1, c2, n in top25]
over = [(c1, c2, r) for a, r, c1, c2, n in res if a > 0.5]
print(f"[LEAK-TEST] pairs with |r|>0.5: {len(over)}")
for c1, c2, r in over[:25]:
    print(f"     !! {c1} x {c2}: r={r:+.4f}")
report['pairs_gt_0.5'] = [f"{c1} x {c2}: r={r:+.4f}" for c1, c2, r in over]

# =====================================================================
# 2) NAME-BASED SUSPICION SCAN
# =====================================================================
KEYWORDS = ['ret_', 'future', 'outcome', 'exit_', 'pnl', 'net_', 'mfe', 'mae',
            'hit_', 'graduated_after', 'survived_', 'collapsed_', 'class',
            'label', 'target', 'peak', 'time_to_', 'to_grad', 'grad']
print(f"\n[NAME-SCAN]")
report['name_scan'] = []
for c in st.schema.names:
    lc = c.lower()
    hit = [k for k in KEYWORDS if k in lc]
    if not hit:
        continue
    a = st.column(c)
    if pa.types.is_integer(a.type) or pa.types.is_floating(a.type):
        v = a.to_numpy(zero_copy_only=False).astype(np.float64)
        vv = v[np.isfinite(v)]
        stat = f"min={vv.min():.4g} med={np.median(vv):.4g} max={vv.max():.4g} nuniq~{len(np.unique(vv))}" if len(vv) else "all-null"
    elif pa.types.is_boolean(a.type):
        v = a.to_numpy(zero_copy_only=False)
        stat = f"bool true={int(np.nansum(v.astype(float)))}/{len(v)}"
    else:
        vals, cnts = np.unique(np.array(a.to_numpy(zero_copy_only=False)), return_counts=True)
        order = np.argsort(-cnts)[:4]
        stat = "top=" + str({str(vals[o]): int(cnts[o]) for o in order})
    print(f"  {c:32s} kw={hit} | {stat}")
    report['name_scan'].append(f"{c} [{','.join(hit)}] {stat}")

# =====================================================================
# 3) DERIVABILITY TEST
# =====================================================================
def auc(y, s):
    y = np.asarray(y); s = np.asarray(s, dtype=np.float64)
    pos = s[y == 1]; neg = s[y == 0]
    if len(pos) == 0 or len(neg) == 0:
        return float('nan')
    allv = np.concatenate([pos, neg])
    order = allv.argsort()
    ranks = np.empty(len(allv), dtype=np.float64); ranks[order] = np.arange(1, len(allv) + 1)
    _, inv, cnt = np.unique(allv, return_inverse=True, return_counts=True)
    sr = np.zeros(len(cnt)); np.add.at(sr, inv, ranks)
    avg = (sr / cnt)[inv]
    n1, n0 = len(pos), len(neg)
    return float((avg[:n1].sum() - n1 * (n1 + 1) / 2) / (n1 * n0))

def logistic_fit(X, y, iters=500, lr=0.5, l2=1e-3):
    n, d = X.shape
    w = np.zeros(d); b = 0.0
    for _ in range(iters):
        p = 1.0 / (1.0 + np.exp(-np.clip(X @ w + b, -30, 30)))
        g = p - y
        w -= lr * (X.T @ g / n + l2 * w)
        b -= lr * g.mean()
    return w, b

def build_X(tbl, idx, feat_cols, mu=None, sd=None):
    X = np.empty((len(idx), len(feat_cols)), dtype=np.float64)
    for k, c in enumerate(feat_cols):
        X[:, k] = tbl.column(c).to_numpy(zero_copy_only=False)[idx].astype(np.float64)
    if mu is None:
        mu = np.nanmean(X, axis=0); sd = np.nanstd(X, axis=0); sd[sd < 1e-9] = 1.0
    X = np.where(np.isfinite(X), X, mu)
    return (X - mu) / sd, mu, sd

feat_cols = [c for c in st.schema.names if pa.types.is_integer(sm[c]) or pa.types.is_floating(sm[c])]
# drop constant
nzfeat = []
for c in feat_cols:
    if st.column(c).num_chunks and len(pc.unique(st.column(c)).to_pylist()) > 1:
        nzfeat.append(c)
feat_cols = nzfeat
print(f"\n[DERIVABILITY] numeric feature cols used={len(feat_cols)}  joined_rows={N}")

mint_st = np.array(col_np(st, 'mint'))
def split_mask(n_):
    return np.array([hash(s) % 5 for s in mint_st[:n_]])

# ---- Target A: ret_300s > 0 ----
yA = (FUT_NP['ret_300s'] > 0).astype(int)
mh = split_mask(N)
tr = mh != 0; te = mh == 0
Xtr, mu, sd = build_X(st, keep_st[tr], feat_cols)
Xte, _, _ = build_X(st, keep_st[te], feat_cols, mu, sd)
w, b = logistic_fit(Xtr, yA[tr])
pte = 1/(1+np.exp(-np.clip(Xte@w+b, -30, 30)))
aucA = auc(yA[te], pte); accA = float(((pte>0.5).astype(int)==yA[te]).mean())
baseA = float(max(yA[te].mean(), 1-yA[te].mean()))
print(f"[DERIVABILITY A] ret_300s>0  train={int(tr.sum())} test={int(te.sum())} pos_rate={yA.mean():.4f} "
      f"AUC={aucA:.4f} acc={accA:.4f} majority_baseline={baseA:.4f}")
report['deriv_ret300'] = {'target':'ret_300s>0','train_n':int(tr.sum()),'test_n':int(te.sum()),
                          'pos_rate':float(yA.mean()),'auc':aucA,'acc':accA,'baseline':baseA}

# also a small-threshold binary (ret_300s_bp > 100 i.e. +1%)
ret300bp = col_np(oc, 'ret_300s_bp')[keep_oc].astype(np.float64)
yA2 = (ret300bp > 100).astype(int)
w2, b2 = logistic_fit(Xtr, yA2[tr])
pt2 = 1/(1+np.exp(-np.clip(Xte@w2+b2, -30, 30)))
aucA2 = auc(yA2[te], pt2); accA2 = float(((pt2>0.5).astype(int)==yA2[te]).mean())
print(f"[DERIVABILITY A2] ret_300s_bp>100  pos_rate={yA2.mean():.4f} AUC={aucA2:.4f} acc={accA2:.4f} baseline={max(yA2[te].mean(),1-yA2[te].mean()):.4f}")
report['deriv_ret300bp100'] = {'target':'ret_300s_bp>100','auc':aucA2,'acc':accA2,'pos_rate':float(yA2.mean())}

# part-disjoint split (parts 0,1 train ; part 2 test) -- join is order-preserved from state parts
nper = 251100
part_of = np.minimum(keep_st // nper, 2)
trp = part_of < 2; tep = part_of == 2
if trp.sum() > 0 and tep.sum() > 0:
    Xtrp, mu2, sd2 = build_X(st, keep_st[trp], feat_cols)
    Xtep, _, _ = build_X(st, keep_st[tep], feat_cols, mu2, sd2)
    wp, bp = logistic_fit(Xtrp, yA[trp])
    ptp = 1/(1+np.exp(-np.clip(Xtep@wp+bp, -30, 30)))
    aucP = auc(yA[tep], ptp); accP = float(((ptp>0.5).astype(int)==yA[tep]).mean())
    print(f"[DERIVABILITY A partsplit] train={int(trp.sum())} test={int(tep.sum())} AUC={aucP:.4f} acc={accP:.4f}")
    report['deriv_ret300_partsplit'] = {'auc':aucP,'acc':accP,'train_n':int(trp.sum()),'test_n':int(tep.sum())}

# ---- Target B: economic / net_return_bp>0 ----
yB = (nrb > 0).astype(int)
mhb = np.array([hash(s) % 5 for s in mint_st[:Nc]])
trb = mhb != 0; teb = mhb == 0
Xtrb, mub, sdb = build_X(st, ks2[trb], feat_cols)
Xteb, _, _ = build_X(st, ks2[teb], feat_cols, mub, sdb)
wb, bb = logistic_fit(Xtrb, yB[trb])
ptb = 1/(1+np.exp(-np.clip(Xteb@wb+bb, -30, 30)))
aucB = auc(yB[teb], ptb); accB = float(((ptb>0.5).astype(int)==yB[teb]).mean())
baseB = float(max(yB[teb].mean(), 1-yB[teb].mean()))
print(f"\n[DERIVABILITY B] net_return_bp>0 train={int(trb.sum())} test={int(teb.sum())} pos_rate={yB.mean():.4f} "
      f"AUC={aucB:.4f} acc={accB:.4f} baseline={baseB:.4f}")
report['deriv_class_netret'] = {'target':'net_return_bp>0','train_n':int(trb.sum()),'test_n':int(teb.sum()),
                                'pos_rate':float(yB.mean()),'auc':aucB,'acc':accB,'baseline':baseB}

# one-vs-rest for each economic_class label
ovr = {}
for c in CLASSES:
    yc = (ec_arr == c).astype(int)
    if yc[trb].sum() < 100 or yc[teb].sum() < 50:
        continue
    wc, bc = logistic_fit(Xtrb, yc[trb])
    pc_ = 1/(1+np.exp(-np.clip(Xteb@wc+bc, -30, 30)))
    ovr[str(c)] = auc(yc[teb], pc_)
print(f"[DERIVABILITY B-ovr] per economic_class AUC: { {k: round(v,4) for k,v in ovr.items()} }")
report['deriv_class_ovr_auc'] = {k: float(v) for k, v in ovr.items()}

# =====================================================================
# 4) TIMESTAMP DISCIPLINE
# =====================================================================
st_ts = col_np(st, 'event_time_unix_ms')[keep_st].astype(np.float64)
oc_ts = col_np(oc, 'event_time_unix_ms')[keep_oc].astype(np.float64)
oc_exit = col_np(oc, 'exit_event_time_ms')[keep_oc].astype(np.float64)
oc_obs = col_np(oc, 'observation_end_ms')[keep_oc].astype(np.float64)
print(f"\n[TS] rows examined={N}")
report['ts_rows_examined'] = int(N)
bad_event = int((oc_ts - st_ts < 0).sum())
d_exit = oc_exit - st_ts
exit_nonnull = np.isfinite(d_exit)
bad_exit = int(((d_exit < 0) & exit_nonnull).sum())
d_obs = oc_obs - st_ts
bad_obs = int((d_obs < 0).sum())
print(f"[TS] outcome.event_time < state.event_time: {bad_event}")
print(f"[TS] exit_event_time_ms < state.event_time (of {int(exit_nonnull.sum())} non-null): {bad_exit}")
print(f"[TS] observation_end_ms < state.event_time: {bad_obs} (min delta_obs={np.nanmin(d_obs):.0f} ms, max={np.nanmax(d_obs):.0f})")
print(f"[TS] outcome.event_time - state.event_time: min={np.nanmin(oc_ts-st_ts):.0f} max={np.nanmax(oc_ts-st_ts):.0f}")
report.update({'ts_bad_event':bad_event,'ts_bad_exit':bad_exit,'ts_bad_obs':bad_obs,'ts_exit_nonnull':int(exit_nonnull.sum())})

with open("/training/v2/reports/leak_audit_result.json", 'w') as f:
    json.dump(report, f, indent=2, default=str)
print("\n[WROTE] /training/v2/reports/leak_audit_result.json")
