import pandas as pd, glob, math
fs = sorted(glob.glob('D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact/pump_outcome_v3/pump_outcome_v3_compact_part*.parquet'))
print(len(fs), 'files')
df = pd.read_parquet(fs[0])
cols = [c for c in df.columns if 'surv' in c or 'ret_' in c or 'collaps' in c or 'mfe' in c or 'mae' in c]
print(cols)
for c in cols:
    s = df[c]
    if s.dtype == bool or str(s.dtype).startswith('bool'):
        print(c, 'true_frac=%.4f' % s.mean())
    else:
        print(c, 'nonnull=%.3f' % s.notna().mean(), 'median=', s.median())
