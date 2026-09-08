import json, glob, math
import pyarrow.parquet as pq
import pandas as pd

# frozen eval state_ids + mints
p = '../output/qwen_curriculum_v1/eval_v1_1/qwen_eval_v1_1.jsonl'
recs = [json.loads(l) for l in open(p, encoding='utf-8') if l.strip()]
pairs = []  # (state_id, mint_id_raw?) -- v1.1 candidates have mint_id_raw?
for r in recs:
    if r.get('panel_source') != 'slinky':
        continue
    for c in r.get('candidates', []):
        sid = (c.get('provenance_ids') or {}).get('state_id')
        pairs.append((sid, c.get('mint_id_raw'), c.get('mint_id')))
print('total slinky cands:', len(pairs), 'sample:', pairs[0])

sids = list({s for s, _, _ in pairs if s})[:2000]
fs = sorted(glob.glob('D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact/pump_outcome_v3/*.parquet'))
cols = ['state_id', 'mint', 'timestamp_ms', 'mfe_bp', 'mae_bp', 'survived_60s',
        'ret_300s_bp', 'collapsed_50pct_within_300s']
hits = []
mints = set()
for fp in fs:
    t = pq.read_table(fp, columns=[c for c in cols if c in pq.ParquetFile(fp).schema_arrow.names],
                      filters=[('state_id', 'in', sids)])
    d = t.to_pandas()
    if len(d):
        hits.append(d)
        mints.update(d['mint'].tolist())
df = pd.concat(hits) if hits else pd.DataFrame()
print('state-matched rows:', len(df))
if len(df):
    print('mfe nonnull:', df['mfe_bp'].notna().mean(), 'surv true:', df['survived_60s'].mean())

# by-mint ALL rows for a few mints: how does mfe vary across rows of one mint?
some = list(mints)[:5]
rows = []
for fp in fs:
    t = pq.read_table(fp, columns=cols, filters=[('mint', 'in', some)])
    d = t.to_pandas()
    if len(d):
        rows.append(d)
allr = pd.concat(rows).sort_values(['mint', 'timestamp_ms'])
for m, g in allr.groupby('mint'):
    print(m[:8], 'rows=%d mfe_nonnull=%d surv_true=%d last_mfe=%s' %
          (len(g), g['mfe_bp'].notna().sum(), g['survived_60s'].sum(),
           g['mfe_bp'].iloc[-1]))
