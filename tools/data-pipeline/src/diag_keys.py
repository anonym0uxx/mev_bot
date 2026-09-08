import pandas as pd, glob
fs = sorted(glob.glob('D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact/pump_outcome_v3/*.parquet'))
df = pd.read_parquet(fs[0])
print([c for c in df.columns if 'state' in c or 'mint' in c or 'id' in c][:20])
print(df[['state_id'] if 'state_id' in df.columns else df.columns[:5]].head(3))
