import sys, pickle, time, os, gc
import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

SRC = 'D:/mev_bot-artifacts/rust-data/full_trades.pkl'
OUT = 'D:/mev_bot-artifacts/rust-data/full_trades.parquet'

t0 = time.time()
print('loading pickle...', flush=True)
with open(SRC, 'rb') as f:
    obj = pickle.load(f)
print(f'loaded in {time.time()-t0:.1f}s, type={type(obj).__name__}', flush=True)

# Introspect
if isinstance(obj, pd.DataFrame):
    print(f'DataFrame: shape={obj.shape}', flush=True)
    print(f'columns: {list(obj.columns)}', flush=True)
    print('dtypes:')
    for c in obj.columns:
        print(f'  {c}: {obj[c].dtype}', flush=True)
    print('converting to parquet...', flush=True)
    obj.to_parquet(OUT, index=False)
    print(f'wrote {OUT}', flush=True)
elif isinstance(obj, dict):
    print(f'dict keys: {list(obj.keys())[:50]}', flush=True)
    for k, v in list(obj.items())[:10]:
        print(f'  key={k!r}: type={type(v).__name__}, len={len(v) if hasattr(v,"__len__") else "n/a"}', flush=True)
elif isinstance(obj, list):
    print(f'list len={len(obj)}', flush=True)
    if obj:
        print(f'first elem type={type(obj[0]).__name__}', flush=True)
        print(f'first elem: {repr(obj[0])[:500]}', flush=True)
else:
    print(f'repr head: {repr(obj)[:1000]}', flush=True)
print('DONE', flush=True)
