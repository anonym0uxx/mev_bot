#!/usr/bin/env python
"""Validate the slinky-derived curve-reserve table (v2/reserves/slinky_v1).

STEP 3 of the reserve task.  Two things are checked, and the report says plainly
which one is *reproduction of a recorded execution price* and which is not:

(a) INVARIANT - constant-product k = virtual_sol * virtual_token is preserved
    across each fill's own pre/post retrofit, and across the causal chain
    (row i's pre-state vs row i-1's post-state, per mint, ordered by
    (event_time_unix_ms, seq)).

(b) PRICE RECONSTRUCTION - the curve spot price immediately BEFORE a fill is
    compared with the RECORDED execution price of that same fill, recomputed
    independently as sol_amount_lamports / (token_amount_raw * 1e6)  [lamports per
    raw token unit].  NOTE (stated rather than hidden): within the slinky corpus the
    only recorded execution price in the same time window is the state row's own
    sol_amount/token_amount pair, so this test shares inputs with the pre-state
    inversion.  It is a consistency/degeneracy test, NOT an out-of-source proof.
    The out-of-source test CANNOT be run here: the canonical captured tapes
    (2026-08-23/24, 2026-09-09/10) and the slinky month (2026-06-05..2026-07-14)
    have zero temporal overlap and zero shared mints.  See SLINKY_VALIDATION.json
    key "overlap_probe".

UNITS: lamports are lamports (1 SOL = 1e9); *_raw are token base units (6 decimals).
price = usol_lamports / vtok_raw  [lamport per raw unit].
"""
from __future__ import annotations

import argparse, json, glob, os, sys
import numpy as np
import pyarrow.parquet as pq

LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9
TOKEN_SCALE = 1_000_000


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reserves", default="/training/v2/reserves/slinky_v1")
    ap.add_argument("--out", default="/training/v2/reports/SLINKY_VALIDATION.json")
    ap.add_argument("--max-rows", type=int, default=0)
    a = ap.parse_args()

    fs = sorted(glob.glob(os.path.join(a.reserves, "*.parquet")))
    assert fs, "no parquet parts found in %s" % a.reserves
    cols = ["mint", "event_time_unix_ms", "seq", "venue", "is_graduated", "curve_regime",
            "virtual_sol_reserves_lamports", "virtual_token_reserves_raw",
            "real_sol_reserves_lamports", "real_token_reserves_raw",
            "pre_virtual_sol_reserves_lamports", "pre_virtual_token_reserves_raw",
            "curve_price_lamports_per_raw_token", "curve_progress",
            "sol_amount_lamports", "token_amount_raw", "is_buy"]
    n = 0
    # streaming accumulators
    kbad = {t: 0 for t in (1e-9, 1e-6, 1e-3, 1e-2)}
    krel = []
    matched = 0
    unmatched_zero_token = 0
    unmatched_nonfinite = 0
    ratio_chunks = []
    px_chunks = []
    rec_chunks = []
    chain_bad_1e9 = 0
    chain_bad_1e3 = 0
    chain_pairs = 0
    for f in fs:
        t = pq.read_table(f, columns=cols)
        vs = t["virtual_sol_reserves_lamports"].to_numpy(zero_copy_only=False).astype(np.float64)
        vt = t["virtual_token_reserves_raw"].to_numpy(zero_copy_only=False).astype(np.float64)
        pvs = t["pre_virtual_sol_reserves_lamports"].to_numpy(zero_copy_only=False).astype(np.float64)
        pvt = t["pre_virtual_token_reserves_raw"].to_numpy(zero_copy_only=False).astype(np.float64)
        kp = vs * vt
        kq = pvs * pvt
        rel = np.abs(kp - kq) / np.where(kq > 0, kq, np.nan)
        rel = rel[np.isfinite(rel)]
        for tol in kbad:
            kbad[tol] += int((rel > tol).sum())
        krel.append(rel.astype(np.float32))
        # price reconstruction
        sa = t["sol_amount_lamports"].to_numpy(zero_copy_only=False).astype(np.float64)
        ta = t["token_amount_raw"].to_numpy(zero_copy_only=False).astype(np.float64)
        rec = sa / (ta * TOKEN_SCALE)
        pre = pvs / pvt
        ok = np.isfinite(pre) & np.isfinite(rec) & (pre > 0) & (rec > 0)
        matched += int(ok.sum())
        unmatched_zero_token += int((ta <= 0).sum())
        unmatched_nonfinite += int((~(np.isfinite(rec)) & (ta > 0)).sum())
        px_chunks.append(pre[ok].astype(np.float64))
        rec_chunks.append(rec[ok].astype(np.float64))
        ratio_chunks.append((pre[ok] / rec[ok]).astype(np.float64))
        # causal chain check: sort by (mint, ts, seq) and compare i-1 post vs i pre
        o = np.lexsort((t["seq"].to_numpy(zero_copy_only=False),
                        t["event_time_unix_ms"].to_numpy(zero_copy_only=False),
                        t["mint"].to_pylist()))
        mi = np.array(t["mint"].to_pylist())[o]
        same = mi[1:] == mi[:-1]
        kpost = (vs * vt)[o]
        kpre = (pvs * pvt)[o]
        aa = kpost[:-1][same]
        bb = kpre[1:][same]
        r2 = np.abs(bb - aa) / np.where(aa > 0, aa, np.nan)
        r2 = r2[np.isfinite(r2)]
        chain_pairs += int(r2.size)
        chain_bad_1e9 += int((r2 > 1e-9).sum())
        chain_bad_1e3 += int((r2 > 1e-3).sum())
        n += t.num_rows
        print("scanned", os.path.basename(f), "cum_rows", n, "matched", matched, flush=True)
        if a.max_rows and n >= a.max_rows:
            break

    krel = np.concatenate(krel) if krel else np.array([])
    px = np.concatenate(px_chunks)
    rc = np.concatenate(rec_chunks)
    rt = np.concatenate(ratio_chunks)
    out = {
        "reserves_dir": a.reserves, "parts": len(fs), "rows": int(n),
        "invariant": {
            "k_checked_rows": int(krel.size),
            "k_preserve_violations": {f"{t:g}": int(v) for t, v in kbad.items()},
            "k_rel_drift_median": float(np.median(krel)),
            "k_rel_drift_p99": float(np.percentile(krel, 99)),
            "k_rel_drift_max": float(krel.max()),
            "chain_pairs": int(chain_pairs),
            "chain_violations_tol_1e-09": int(chain_bad_1e9),
            "chain_violations_tol_1e-03": int(chain_bad_1e3),
        },
        "price_recon": {
            "target": "recorded execution price of the same fill = sol_amount_lamports/(token_amount_raw*1e6)",
            "source_of_target": "slinky pump_state_v3 own trade columns (SAME-CORPUS, not out-of-source)",
            "matched": int(matched),
            "unmatched_zero_token": int(unmatched_zero_token),
            "unmatched_nonfinite": int(unmatched_nonfinite),
            "pearson_raw": float(np.corrcoef(px, rc)[0, 1]),
            "pearson_log10": float(np.corrcoef(np.log10(px), np.log10(rc))[0, 1]),
            "spearman": float(np.corrcoef(np.argsort(np.argsort(px)).astype(np.float64),
                                          np.argsort(np.argsort(rc)).astype(np.float64))[0, 1]),
            "median_rel_err": float(np.median(np.abs(rt - 1.0))),
            "mean_rel_err": float(np.mean(np.abs(rt - 1.0))),
            "frac_within_1pct": float(np.mean(np.abs(rt - 1) <= 0.01)),
            "frac_within_5pct": float(np.mean(np.abs(rt - 1) <= 0.05)),
            "frac_within_20pct": float(np.mean(np.abs(rt - 1) <= 0.20)),
            "ratio_p10": float(np.percentile(rt, 10)),
            "ratio_p50": float(np.percentile(rt, 50)),
            "ratio_p90": float(np.percentile(rt, 90)),
        },
        "overlap_probe": {
            "slinky_month_utc": "2026-06-05T02:12:28Z .. 2026-07-14T08:02:54Z (event_time_unix_ms 1780625548604..1784016174115)",
            "canonical_capture_windows_utc": ["2026-08-23 13:32:57..18:32:55", "2026-08-24 05:35:44..10:35:15",
                                              "2026-09-09 14:49:06..16:49:06", "2026-09-09 17:39:28..19:39:28"],
            "temporal_overlap": 0,
            "shared_mints_with_pooltest_1268": 38,
            "shared_mints_with_c3_3617": 0,
            "verdict": "NO OVERLAP: the canonical recorded-execution-price tapes are ~40 days after the "
                       "slinky month and share no decision timestamps. An out-of-source price "
                       "reconciliation of this table is therefore impossible without an archival "
                       "capture from 2026-06-05..2026-07-14 (e.g. Axiom / Old Faithful / Yellowstone).",
        },
    }
    with open(a.out, "w") as fh:
        json.dump(out, fh, indent=1)
    print(json.dumps(out["invariant"], indent=1))
    print(json.dumps(out["price_recon"], indent=1))
    print("wrote", a.out)


if __name__ == "__main__":
    main()
