import json
import numpy as np

P = "/training/v2/canonical/ledger_v7/states.jsonl"
mfe, mae, net = [], [], []
n = 0
for line in open(P):
    r = json.loads(line)
    oe = r.get("outcome_evidence", {})
    if oe.get("mfe_bp") is not None:
        mfe.append(oe["mfe_bp"]); mae.append(oe["mae_bp"]); net.append(oe.get("net_bp_300s"))
    n += 1
mfe = np.array(mfe, float); mae = np.array(mae, float)
print(f"episodes={n:,} with_outcome={mfe.size:,}")
for name, a in (("mfe_bp", mfe), ("mae_bp", mae)):
    a = a[np.isfinite(a)]
    print(f"{name}: p50={np.percentile(a,50):.1f} p90={np.percentile(a,90):.1f} "
          f"p99={np.percentile(a,99):.1f} p99.9={np.percentile(a,99.9):.1f} max={a.max():.1f}")
    print(f"   frac >1e4 bp (100x)={float((a>1e4).mean()):.5f}  frac >1e6 bp={float((a>1e6).mean()):.5f}")
