import json, glob, random
import pyarrow.parquet as pq
import pandas as pd

p = '../output/qwen_curriculum_v1/eval_v1_1/qwen_eval_v1_1.jsonl'
recs = [json.loads(l) for l in open(p, encoding='utf-8') if l.strip()]
sids = []
for r in recs:
    if r.get('panel_source') != 'slinky':
        continue
    for c in r.get('candidates', []):
        sid = (c.get('provenance_ids') or {}).get('state_id')
        if sid:
            sids.append(sid)
random.seed(0)
samp = random.sample(sids, 1500)
fs = sorted(glob.glob('D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact/pump_outcome_v3/*.parquet'))
cols = ['state_id', 'mint', 'timestamp_ms', 'mfe_bp', 'mae_bp', 'survived_60s',
        'survived_300s', 'ret_5s_bp', 'ret_30s_bp', 'ret_60s_bp', 'ret_120s_bp',
        'ret_300s_bp', 'collapsed_50pct_within_300s', 'observed_through_300s',
        'right_censored_300s', 'has_trade_within_300s']
hits = []
for fp in fs:
    avail = set(pq.ParquetFile(fp).schema_arrow.names)
    use = [c for c in cols if c in avail]
    d = pq.read_table(fp, columns=use, filters=[('state_id', 'in', samp)]).to_pandas()
    if len(d):
        hits.append(d)
df = pd.concat(hits)
print('matched:', len(df))
for c in cols[3:]:
    s = df[c]
    if s.dtype == bool:
        print(c, 'true=%.3f' % s.mean())
    else:
        print(c, 'nonnull=%.3f' % s.notna().mean(),
              'p50=%s' % (s.median() if s.notna().any() else None),
              'max=%s' % (s.max() if s.notna().any() else None))
