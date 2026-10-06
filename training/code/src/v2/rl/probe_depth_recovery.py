"""Can AMM pool reserves be recovered from the tape we ALREADY have?

If yes, the 31% refusal is fixable with zero new data purchase.

Method: `reward_engine.measure_depth` estimates the pool depth D (in SOL) from the
fill sequence alone (slope-through-origin of |log(px_i/px_{i-1})| on notional q_i).
We compare that ESTIMATE against the RECORDED pool reserves from the forward
parquet, for the 845 mints where we actually hold recorded reserves. Agreement
validates the estimator; then we measure how many corpus mints get an estimate.
"""
import json
import sys

import numpy as np

sys.path.insert(0, "/training/v2/code/src/v2/rl")

from reward_engine import LAMPORTS_PER_SOL, load_canonical_tapes, measure_depth

TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
POOL = "/training/v2/reserves/forward/pumpswap_pool_reserves_v1_part0000.parquet"

if __name__ == "__main__":
    import pyarrow.parquet as pq
    t = pq.read_table(POOL, columns=["mint", "pool_sol_lamports"])
    d = t.to_pydict()
    rec = {}
    for m, s in zip(d["mint"], d["pool_sol_lamports"]):
        rec.setdefault(m, []).append(float(s) / LAMPORTS_PER_SOL)
    rec = {m: float(np.median(v)) for m, v in rec.items()}
    print(json.dumps({"recorded_pools": len(rec)}))

    tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000, max_px_ratio=50)
    print(json.dumps({"tapes": len(tapes)}))

    pairs = []
    for m, d_sol in rec.items():
        tp = tapes.get(m)
        if tp is None:
            continue
        est = measure_depth(tp, min_fills=30, min_notional_lamports=100_000)
        if est is None:
            continue
        pairs.append((m, est, d_sol))

    print(json.dumps({"both_measurable": len(pairs)}))
    if pairs:
        e = np.asarray([p[1] for p in pairs], float)
        r = np.asarray([p[2] for p in pairs], float)
        ok = np.isfinite(e) & np.isfinite(r) & (e > 0) & (r > 0)
        e, r = e[ok], r[ok]
        ratio = e / r
        print(json.dumps({
            "n": int(e.size),
            "recorded_sol_p50": float(np.median(r)),
            "estimated_sol_p50": float(np.median(e)),
            "ratio_p50": float(np.median(ratio)),
            "log10_ratio_p05": float(np.percentile(np.log10(ratio), 5)),
            "log10_ratio_p95": float(np.percentile(np.log10(ratio), 95)),
            "frac_within_2x": float(((ratio > 0.5) & (ratio < 2.0)).mean()),
            "frac_within_10x": float(((ratio > 0.1) & (ratio < 10.0)).mean()),
            "spearman": float(np.corrcoef(np.argsort(np.argsort(e)),
                                          np.argsort(np.argsort(r)))[0, 1]),
        }, indent=1))

        # coverage across ALL corpus mints
        n_all = 0
        n_est = 0
        for m, tp in tapes.items():
            n_all += 1
            if measure_depth(tp, min_fills=30, min_notional_lamports=100_000) is not None:
                n_est += 1
        print(json.dumps({"corpus_mints": n_all,
                          "mints_with_depth_estimate": n_est,
                          "coverage": round(n_est / n_all, 4) if n_all else 0.0},
                         indent=1))
