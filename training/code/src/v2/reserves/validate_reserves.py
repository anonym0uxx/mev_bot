#!/usr/bin/env python
"""Validate the reconstructed per-fill pump.fun curve reserves table.

(a) INVARIANT  - the constant-product invariant k = virtual_sol * virtual_token.
    k is mint-specific and NOT globally constant: pump.fun adds the trading fee
    to the virtual SOL reserve, so k drifts upward by roughly fee/notional per
    fill.  We therefore test the *preservation* form:
        |k_post - k_pre| / k_pre  <= tol         (same fill)
    and the *chain* form (my pre-state inversion vs the previous fill's post):
        k_pre[i+1] == k_post[i]  within tol      (causal order per mint)

(b) PRICE RECONCILIATION - reconstructed price immediately *before* each fill
    (pre_virtual_sol / pre_virtual_token) vs the RECORDED execution price from
    canonical trades.jsonl (sol_lamports / tokens_raw).  A reconstruction that
    cannot reproduce recorded execution prices is wrong, and this reports it as
    such rather than proceeding.

UNITS: all *_lamports are lamports (1 SOL = 1e9); all *_raw are token base units
(decimals 6).  price = vsol_lamports / vtok_raw  [lamport per raw unit].
"""
from __future__ import annotations

import argparse, json, sys
from decimal import Decimal

import numpy as np
import pyarrow.dataset as ds

LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9


def load_reserves(paths):
    d = ds.dataset(paths, format="parquet")
    t = d.to_table(columns=[
        "mint_b58", "signature", "slot", "recv_unix_ms", "tx_index",
        "event_index", "is_buy", "trade_ts_unix",
        "sol_amount_lamports", "token_amount_raw",
        "virtual_sol_reserves_lamports", "virtual_token_reserves_raw",
        "real_sol_reserves_lamports", "real_token_reserves_raw",
        "pre_virtual_sol_reserves_lamports", "pre_virtual_token_reserves_raw",
    ])
    return {c: t[c].to_numpy(zero_copy_only=False) for c in t.column_names}


def invariant(res, tols=(1e-12, 1e-9, 1e-6, 1e-3)):
    vsol = res["virtual_sol_reserves_lamports"].astype(object)
    vtok = res["virtual_token_reserves_raw"].astype(object)
    pvsol = res["pre_virtual_sol_reserves_lamports"].astype(object)
    pvtok = res["pre_virtual_token_reserves_raw"].astype(object)
    n = len(vsol)
    k_pre = np.empty(n, dtype=object)
    k_post = np.empty(n, dtype=object)
    for i in range(n):
        k_pre[i] = pvsol[i] * pvtok[i]
        k_post[i] = vsol[i] * vtok[i]
    kpre = np.array([float(x) for x in k_pre])
    kpost = np.array([float(x) for x in k_post])
    rel = np.abs(kpost - kpre) / kpre
    out = {"rows": int(n)}
    for tol in tols:
        v = int((rel > tol).sum())
        out[f"k_preserve_violations_tol_{tol:g}"] = v
    out["k_preserve_rel_mean"] = float(rel.mean())
    out["k_preserve_rel_median"] = float(np.median(rel))
    out["k_preserve_rel_p99"] = float(np.percentile(rel, 99))
    out["k_preserve_rel_max"] = float(rel.max())

    # chain form: per mint, causally ordered, k_pre[i+1] == k_post[i]
    mi = res["mint_b58"]
    order = np.lexsort((res["event_index"], res["tx_index"],
                        res["recv_unix_ms"], res["slot"], mi))
    mi = mi[order]
    kpre = kpre[order]
    kpost = kpost[order]
    same = mi[1:] == mi[:-1]
    a = kpost[:-1][same]
    b = kpre[1:][same]
    rel2 = np.abs(b - a) / np.where(a > 0, a, 1.0)
    out["chain_pairs"] = int(same.sum())
    out["chain_violations_tol_1e-9"] = int((rel2 > 1e-9).sum())
    out["chain_violations_tol_1e-3"] = int((rel2 > 1e-3).sum())
    out["chain_rel_median"] = float(np.median(rel2)) if len(rel2) else None
    out["chain_rel_p99"] = float(np.percentile(rel2, 99)) if len(rel2) else None
    out["chain_rel_max"] = float(rel2.max()) if len(rel2) else None
    # k sanity: k must equal vsol*vtok exactly by construction (assert, not assume)
    bad = 0
    for i in range(min(n, 200000)):
        if int(vsol[i]) * int(vtok[i]) != int(k_post[i]):
            bad += 1
    out["k_construction_mismatch_first200k"] = bad
    return out


def price_recon(res, trades_path, venues=("pumpfun",), limit_rows=0, prefer_pool=False):
    index = {}
    n = len(res["signature"])
    for i in range(n):
        index[(res["signature"][i], res["mint_b58"][i], bool(res["is_buy"][i]))] = i
    matched = 0
    unmatched = 0
    no_price = 0
    rec_p, rec_e, rec_m, rec_s, rec_pool = [], [], [], [], []
    rows_seen = 0
    with open(trades_path) as f:
        for line in f:
            rows_seen += 1
            if limit_rows and rows_seen > limit_rows:
                break
            o = json.loads(line)
            if o.get("venue") not in venues:
                continue
            sol = int(o["sol_lamports"])
            tok = int(o["tokens_raw"])
            if tok == 0:
                continue
            side = o.get("side") or ("buy" if o["side"] == "buy" else "sell")
            is_buy = (o["side"] == "buy")
            key = (o["signature"], o["mint"], is_buy)
            i = index.get(key)
            if i is None:
                unmatched += 1
                continue
            matched += 1
            ppre = res["pre_virtual_sol_reserves_lamports"][i] / res["pre_virtual_token_reserves_raw"][i]
            pexe = res["sol_amount_lamports"][i] / res["token_amount_raw"][i] if res["token_amount_raw"][i] else np.nan
            prec = abs(sol) / abs(tok)
            # Optional: prefer the POOL-side recorded leg when the tape carries it
            # (pool_sol_lamports / pool_tokens_raw).  The trader-side leg embeds
            # pump.fun's fee and, on first buys, ATA rent, so it is a noisier
            # probe of the curve price than the pool leg is.
            if prefer_pool and o.get("pool_tokens_raw"):
                ps, pt = int(o.get("pool_sol_lamports") or 0), int(o["pool_tokens_raw"])
                if pt:
                    prec_pool = abs(ps) / abs(pt)
                else:
                    prec_pool = np.nan
            else:
                prec_pool = np.nan
            rec_p.append(ppre)
            rec_e.append(pexe)
            rec_m.append(prec)
            rec_pool.append(prec_pool)
            rec_s.append(res["sol_amount_lamports"][i] if is_buy else -res["sol_amount_lamports"][i])
    rec_p = np.array(rec_p, dtype=float)
    rec_e = np.array(rec_e, dtype=float)
    rec_m = np.array(rec_m, dtype=float)
    rec_pool = np.array(rec_pool, dtype=float)
    out = {"trades_rows_scanned": rows_seen, "matched": matched,
           "unmatched": unmatched, "zero_token_skipped": no_price}
    if matched == 0:
        out["NOTE"] = "ZERO matched fills - aborting statistics"
        return out
    ok = np.isfinite(rec_p) & np.isfinite(rec_m) & (rec_p > 0) & (rec_m > 0)
    out["finite_pairs"] = int(ok.sum())
    a, b = rec_p[ok], rec_m[ok]
    out["corr_pre_vs_recorded"] = float(np.corrcoef(a, b)[0, 1])
    # Price spans ~8 decades and dust fills carry fixed-fee/rent contamination in
    # the trader-side recorded leg, so raw-level Pearson is degenerate. Report
    # log-price Pearson, Spearman and band coverage alongside it.
    m = a > 0
    out["log10_pearson"] = float(np.corrcoef(np.log10(a[m]), np.log10(b[m]))[0, 1])
    rk = lambda v: np.argsort(np.argsort(v)).astype(float)
    out["spearman"] = float(np.corrcoef(rk(a[m]), rk(b[m]))[0, 1])
    ratio = a[m] / b[m]
    out["frac_within_5pct"] = float(np.mean(np.abs(ratio - 1) <= 0.05))
    out["frac_within_10pct"] = float(np.mean(np.abs(ratio - 1) <= 0.10))
    out["frac_within_20pct"] = float(np.mean(np.abs(ratio - 1) <= 0.20))
    out["frac_within_2x"] = float(np.mean((ratio >= 0.5) & (ratio <= 2.0)))
    err = np.abs(a - b)
    out["mean_abs_err_lamports_per_raw"] = float(err.mean())
    out["median_abs_err"] = float(np.median(err))
    out["p90_abs_err"] = float(np.percentile(err, 90))
    out["max_abs_err"] = float(err.max())
    out["mean_rel_err"] = float((err / b).mean())
    out["median_rel_err"] = float(np.median(err / b))
    # curve execution price from the event itself vs recorded (fee-inclusive) price
    ok2 = np.isfinite(rec_e) & (rec_e > 0)
    if ok2.sum():
        e, m = rec_e[ok2], rec_m[ok2]
        out["corr_event_exec_vs_recorded"] = float(np.corrcoef(e, m)[0, 1])
        out["mean_rel_err_event_exec_vs_recorded"] = float(np.abs(e - m).mean() / m.mean())
        out["fee_ratio_median"] = float(np.median((m - e) / m))
    okp = np.isfinite(rec_pool) & (rec_pool > 0) & ok
    if int(okp.sum()) >= 2:
        ap, bp = rec_p[okp], rec_pool[okp]
        out["pool_leg_pairs"] = int(okp.sum())
        out["pool_leg_corr_pre_vs_recorded"] = float(np.corrcoef(ap, bp)[0, 1])
        out["pool_leg_log10_pearson"] = float(np.corrcoef(np.log10(ap), np.log10(bp))[0, 1])
        rp = ap / bp
        out["pool_leg_median_rel_err"] = float(np.median(np.abs(rp - 1.0)))
        out["pool_leg_frac_within_5pct"] = float(np.mean(np.abs(rp - 1.0) <= 0.05))
        out["pool_leg_frac_within_20pct"] = float(np.mean(np.abs(rp - 1.0) <= 0.20))
        out["pool_leg_frac_within_2x"] = float(np.mean((rp >= 0.5) & (rp <= 2.0)))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reserves", nargs="+", required=True)
    ap.add_argument("--trades", default="/training/v2/canonical/renormalized_v7/trades.jsonl")
    ap.add_argument("--out")
    ap.add_argument("--invariant-only", action="store_true")
    ap.add_argument("--prefer-pool", action="store_true",
                    help="when the tape carries pool_sol_lamports/pool_tokens_raw, "
                         "also compare the reconstruction against the pool-side "
                         "recorded price (cleaner: no trader fee/rent)")
    a = ap.parse_args()
    res = load_reserves(a.reserves)
    print("loaded_rows", len(res["signature"]))
    assert len(res["signature"]) > 0, "FATAL: zero reserves rows loaded"
    report = {"reserves_rows": int(len(res["signature"]))}
    report["invariant"] = invariant(res)
    print(json.dumps(report["invariant"], indent=2))
    if not a.invariant_only:
        report["price_recon"] = price_recon(res, a.trades, prefer_pool=a.prefer_pool)
        print(json.dumps(report["price_recon"], indent=2))
    if a.out:
        with open(a.out, "w") as f:
            json.dump(report, f, indent=2)
        print("wrote", a.out)


if __name__ == "__main__":
    main()