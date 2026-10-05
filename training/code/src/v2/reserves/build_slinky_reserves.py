#!/usr/bin/env python
"""Build a per-(mint, state-event) curve-reserve table from the slinky_gold_v3_compact
pump_state_v3 layer (month-long, read-only on /mnt/data).

Units discipline (verified empirically, see reports/SLINKY_RESERVES_V1.json):
  1 SOL == 1e9 lamports exactly.
  v_sol_bonding_curve_lamports   : int64, LAMPORTS (virtual SOL reserve)
  v_tokens_bonding_curve_raw     : int64, RAW base units (6 decimals) (virtual token reserve)
  sol_amount_lamports            : int64, LAMPORTS  -> equals the row's own SOL delta
  token_amount_raw               : float-ish int64 but actually WHOLE TOKENS
                                   (verified: token_amount_raw/token_amount_tokens == 1.0),
                                   so raw delta = token_amount_raw * 1e6
  price_sol_lamports             : lamports per WHOLE token (integer, truncated)
  price_sol                      : SOL per WHOLE token
  => curve spot, lamports per RAW token = v_sol / v_tokens
  => curve spot, lamports per WHOLE token = v_sol / v_tokens * 1e6
"""
import argparse, glob, json, os, sys, math
import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq

LAMPORTS_PER_SOL = 1_000_000_000
TOKEN_DECIMALS = 6
TOKEN_SCALE = 10 ** TOKEN_DECIMALS
V0_LAMPORTS = 30 * LAMPORTS_PER_SOL          # pump.fun initial virtual SOL reserve
T0_RAW = 1_073_000_000 * TOKEN_SCALE         # pump.fun initial virtual token reserve
K0 = float(V0_LAMPORTS) * float(T0_RAW)      # 3.219e25
V_GRAD_LAMPORTS = 85 * LAMPORTS_PER_SOL      # graduation target on the real-SOL leg
REQUIRED = ["mint", "event_time_unix_ms", "seq", "v_sol_bonding_curve_lamports",
            "v_tokens_bonding_curve_raw", "trade_side", "is_buy", "sol_amount_lamports",
            "token_amount_raw", "is_graduated", "venue", "is_mayhem", "curve_pct_depleted"]
OUT_COLS = ["mint", "event_time_unix_ms", "seq", "venue", "is_graduated", "is_mayhem",
            "curve_seed_valid",
            "curve_regime", "virtual_sol_reserves_lamports", "virtual_token_reserves_raw",
            "real_sol_reserves_lamports", "real_token_reserves_raw",
            "pre_virtual_sol_reserves_lamports", "pre_virtual_token_reserves_raw",
            "curve_price_lamports_per_raw_token", "curve_price_lamports_per_token",
            "curve_k", "curve_k_pre", "curve_k_rel_drift", "curve_k_rel_drift_fee_corrected",
            "curve_progress", "curve_pct_depleted", "last_fill_price_impact_bp"]


def bool_col(t, name, part):
    """Null-safe boolean column read -> np.bool_ array. Fails LOUDLY on garbage.

    Historical landmine (this module, line ~142): a mask read as an *object*
    ndarray makes ``ndarray.sum()`` return ``None`` when the array holds a None
    element, and ``agg['mayhem_rows'] += int(None)`` then raises
    ``TypeError: unsupported operand type(s) for +: 'int' and 'NoneType'``.
    Masking NULL as False is the documented semantic (a missing is_mayhem flag is
    not evidence of mayhem mode), but doing it with a bare ``bool(x)`` loop hides
    genuinely malformed values, so non-boolean non-null values raise here instead.
    """
    col = t[name]
    if not pa.types.is_boolean(col.type):
        if pa.types.is_null(col.type):
            raise ValueError(
                f"{part}: column {name!r} is null-typed - refusing to guess a mask")
        out = np.zeros(col.length(), dtype=bool)
        for i, v in enumerate(col.to_pylist()):
            if v is None:
                continue
            if isinstance(v, (bool, np.bool_)):
                out[i] = bool(v)
            elif isinstance(v, (int, np.integer)) and int(v) in (0, 1):
                out[i] = bool(v)
            else:
                raise ValueError(
                    f"{part}: column {name!r} holds non-boolean value {v!r} at row {i}")
        return out
    return np.asarray(col.to_numpy(zero_copy_only=False), dtype=bool)


def count_true(mask, sel, what):
    """Count True in ``mask[sel]`` as a plain int. Can never return None.

    This is the counter that the builder crashed on. An object-dtype mask is
    refused explicitly rather than allowed to reach ``ndarray.sum()``, which is
    exactly how the ``int + NoneType`` TypeError was produced.
    """
    m = np.asarray(mask)[np.asarray(sel)]
    if m.dtype == object:
        raise TypeError(
            f"{what}: object-dtype mask reached the counter - a NULL leaked past "
            f"bool_col(); refusing to sum (this is the historical landmine)")
    return int(np.count_nonzero(m))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", default="/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact/pump_state_v3")
    ap.add_argument("--out", default="/training/v2/reserves/slinky_v1")
    ap.add_argument("--limit-parts", type=int, default=0)
    ap.add_argument("--report", default="/training/v2/reports/SLINKY_RESERVES_V1.json")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    fs = sorted(glob.glob(os.path.join(a.src, "*.parquet")))
    if a.limit_parts:
        fs = fs[:a.limit_parts]

    agg = dict(parts=0, rows_raw=0, rows_kept=0, rows_dropped_null_reserves=0,
               rows_dropped_nonpos=0, mints=set(), ts_min=None, ts_max=None,
               k_viol={t: 0 for t in ("1e-12", "1e-09", "1e-06", "1e-03", "1e-02")},
               k_checked=0, k_rel_abs=[], drift_abs=[], drift_fc_abs=[],
               venue={}, regime={}, grad_thresh_lo=None, grad_thresh_hi=None,
               seed_valid_rows=0, mayhem_rows=0, mayhem_null_masked_rows=0,
               fee_beta_num=0.0, fee_beta_den=0.0, fill_impact=[])

    for k, f in enumerate(fs):
        t = pq.read_table(f, columns=REQUIRED)
        n = t.num_rows
        agg["rows_raw"] += n
        vs = np.asarray(t["v_sol_bonding_curve_lamports"].to_numpy(zero_copy_only=False), dtype=np.float64)
        vt = np.asarray(t["v_tokens_bonding_curve_raw"].to_numpy(zero_copy_only=False), dtype=np.float64)
        sa = np.asarray(t["sol_amount_lamports"].to_numpy(zero_copy_only=False), dtype=np.float64)
        ta = np.asarray(t["token_amount_raw"].to_numpy(zero_copy_only=False), dtype=np.float64)
        ts = np.asarray(t["event_time_unix_ms"].to_numpy(zero_copy_only=False), dtype=np.int64)
        seq = np.asarray(t["seq"].to_numpy(zero_copy_only=False), dtype=np.int64)
        side = np.array(t["trade_side"].to_pylist())
        isb = bool_col(t, "is_buy", f)
        grad = bool_col(t, "is_graduated", f)
        ven = t["venue"].to_pylist()
        cpd = np.asarray(t["curve_pct_depleted"].to_numpy(zero_copy_only=False), dtype=np.float64)
        may = bool_col(t, "is_mayhem", f)
        n_null_mayhem = int(t["is_mayhem"].null_count)
        mint = t["mint"].to_pylist()
        for v in ven:
            agg["venue"][v] = agg["venue"].get(v, 0) + 1

        good = np.isfinite(vs) & np.isfinite(vt) & (vs > 0) & (vt > 0)
        agg["rows_kept"] += int(good.sum())
        agg["rows_dropped_null_reserves"] += int((~np.isfinite(vs) | ~np.isfinite(vt)).sum())
        agg["rows_dropped_nonpos"] += int((np.isfinite(vs) & np.isfinite(vt) & ((vs <= 0) | (vt <= 0))).sum())
        for m in set(np.asarray(mint)[good].tolist()):
            agg["mints"].add(m)
        lo, hi = int(ts.min()), int(ts.max())
        agg["ts_min"] = lo if agg["ts_min"] is None else min(agg["ts_min"], lo)
        agg["ts_max"] = hi if agg["ts_max"] is None else max(agg["ts_max"], hi)

        # pre-state = post-state inverted by this row's own recorded fill deltas
        delta_tok_raw = ta * TOKEN_SCALE                       # whole tokens -> raw (1e6)
        pre_vs = np.where(isb, vs - sa, vs + sa)
        pre_vt = np.where(isb, vt + delta_tok_raw, vt - delta_tok_raw)
        k_post = vs * vt
        k_pre = pre_vs * pre_vt
        with np.errstate(invalid="ignore", divide="ignore"):
            drift = np.where(k_post > 0, np.abs(k_pre - k_post) / k_post, np.nan)
            # fee correction: pump.fun takes ~1% of the SOL leg; k grows by ~fee*sol_in/vs_pre
            fee_pred = np.where((pre_vs > 0) & (sa > 0) & (k_post > 0),
                                0.01 * sa / pre_vs, 0.0)
            drift_fc = np.abs((k_pre / k_post) * (1.0 + np.where(isb, fee_pred, -fee_pred)) - 1.0)
        sel = np.isfinite(drift)
        agg["k_checked"] += int(sel.sum())
        for tag, tol in (("1e-12", 1e-12), ("1e-09", 1e-9), ("1e-06", 1e-6), ("1e-03", 1e-3), ("1e-02", 1e-2)):
            agg["k_viol"][tag] += int((drift[sel] > tol).sum())
        if agg["k_rel_abs"].__len__() < 4_000_000:
            idx = np.where(sel)[0]
            agg["k_rel_abs"].append(drift[idx[:20000]])
            agg["drift_fc_abs"].append(drift_fc[idx[:20000]])
        # fee beta fit: drift ~ beta * sol_in/vs_pre
        xv = np.where((pre_vs > 0) & (sa > 0), sa / pre_vs, np.nan)
        m2 = sel & np.isfinite(xv) & (xv > 0)
        if m2.sum():
            agg["fee_beta_num"] += float(np.sum(drift[m2] * xv[m2]))
            agg["fee_beta_den"] += float(np.sum(xv[m2] ** 2))
        # price impact of the last fill (bp), causal: uses only this row's own deltas
        with np.errstate(invalid="ignore", divide="ignore"):
            p_pre = pre_vs / pre_vt
            p_post = vs / vt
            imp = np.where((p_pre > 0), (p_post - p_pre) / p_pre * 1e4, np.nan)
        if agg["fill_impact"].__len__() < 4_000_000:
            ii = np.where(np.isfinite(imp))[0]
            agg["fill_impact"].append(imp[ii[:20000]])
        # graduation threshold observation
        gmax = vs[~grad & good].max() if (~grad & good).any() else None
        gmin = vs[grad & good].min() if (grad & good).any() else None
        if gmax is not None:
            agg["grad_thresh_lo"] = gmax if agg["grad_thresh_lo"] is None else max(agg["grad_thresh_lo"], gmax)
        if gmin is not None:
            agg["grad_thresh_hi"] = gmin if agg["grad_thresh_hi"] is None else min(agg["grad_thresh_hi"], gmin)

        real_sol = vs - V0_LAMPORTS
        real_tok = T0_RAW - vt
        regime = np.where(grad, "graduated",
                          np.where(real_sol >= 0.9 * V_GRAD_LAMPORTS, "near_graduation", "bonding"))
        for r in set(regime[good].tolist()):
            agg["regime"][r] = agg["regime"].get(r, 0) + int((regime[good] == r).sum())
        agg["seed_valid_rows"] += int((((k_post / K0) > 0.5) & ((k_post / K0) < 2.0))[good].sum())
        agg["mayhem_rows"] += count_true(may, good, "is_mayhem")
        agg["mayhem_null_masked_rows"] += n_null_mayhem

        out = pa.table({
            "mint": pa.array(np.asarray(mint)[good]),
            "event_time_unix_ms": pa.array(ts[good]),
            "seq": pa.array(seq[good]),
            "venue": pa.array(np.asarray(ven)[good]),
            "is_graduated": pa.array(grad[good]),
            "is_mayhem": pa.array(may[good]),
            "curve_seed_valid": pa.array((k_post[good] / K0 > 0.5) & (k_post[good] / K0 < 2.0)),
            "curve_regime": pa.array(np.asarray(regime)[good]),
            "virtual_sol_reserves_lamports": pa.array(vs[good].astype(np.int64)),
            "virtual_token_reserves_raw": pa.array(vt[good].astype(np.int64)),
            "real_sol_reserves_lamports": pa.array(np.round(real_sol[good]).astype(np.int64)),
            "real_token_reserves_raw": pa.array(np.round(real_tok[good]).astype(np.int64)),
            "pre_virtual_sol_reserves_lamports": pa.array(np.round(pre_vs[good]).astype(np.int64)),
            "pre_virtual_token_reserves_raw": pa.array(np.round(pre_vt[good]).astype(np.int64)),
            "curve_price_lamports_per_raw_token": pa.array(vs[good] / vt[good]),
            "curve_price_lamports_per_token": pa.array(np.round(vs[good] / vt[good] * TOKEN_SCALE).astype(np.int64)),
            "curve_k": pa.array(k_post[good]),
            "curve_k_pre": pa.array(k_pre[good]),
            "curve_k_rel_drift": pa.array(drift[good]),
            "curve_k_rel_drift_fee_corrected": pa.array(drift_fc[good]),
            "curve_progress": pa.array((real_sol[good] / V_GRAD_LAMPORTS)),
            "curve_pct_depleted": pa.array(cpd[good]),
            "last_fill_price_impact_bp": pa.array(imp[good]),
            "sol_amount_lamports": pa.array(sa[good].astype(np.int64)),
            "token_amount_raw": pa.array(ta[good].astype(np.int64)),
            "is_buy": pa.array(isb[good]),
        }, schema=pa.schema([
            ("mint", pa.string()), ("event_time_unix_ms", pa.int64()), ("seq", pa.int64()),
            ("venue", pa.string()), ("is_graduated", pa.bool_()), ("is_mayhem", pa.bool_()),
            ("curve_seed_valid", pa.bool_()), ("curve_regime", pa.string()),
            ("virtual_sol_reserves_lamports", pa.int64()), ("virtual_token_reserves_raw", pa.int64()),
            ("real_sol_reserves_lamports", pa.int64()), ("real_token_reserves_raw", pa.int64()),
            ("pre_virtual_sol_reserves_lamports", pa.int64()), ("pre_virtual_token_reserves_raw", pa.int64()),
            ("curve_price_lamports_per_raw_token", pa.float64()), ("curve_price_lamports_per_token", pa.int64()),
            ("curve_k", pa.float64()), ("curve_k_pre", pa.float64()),
            ("curve_k_rel_drift", pa.float64()), ("curve_k_rel_drift_fee_corrected", pa.float64()),
            ("curve_progress", pa.float64()), ("curve_pct_depleted", pa.float64()),
            ("last_fill_price_impact_bp", pa.float64()),
            ("sol_amount_lamports", pa.int64()), ("token_amount_raw", pa.int64()),
            ("is_buy", pa.bool_()),
        ]))
        pq.write_table(out, os.path.join(a.out, "slinky_reserves_v1_part%04d.parquet" % k), compression="zstd")
        agg["parts"] += 1
        if k % 10 == 0:
            print("part %d/%d rows_kept=%d mints=%d" % (k, len(fs), agg["rows_kept"], len(agg["mints"])), flush=True)

    def cat(lst):
        return np.concatenate(lst) if lst else np.array([])
    kk = cat(agg["k_rel_abs"]); fc = cat(agg["drift_fc_abs"]); im = cat(agg["fill_impact"])
    rep = {
        "source_layer": os.path.join(a.src, "pump_state_v3_compact_*.parquet"),
        "parts": agg["parts"], "rows_raw": agg["rows_raw"], "rows_kept": agg["rows_kept"],
        "rows_dropped_null_reserves": agg["rows_dropped_null_reserves"],
        "rows_dropped_nonpositive": agg["rows_dropped_nonpos"],
        "distinct_mints": len(agg["mints"]),
        "ts_min_ms": agg["ts_min"], "ts_max_ms": agg["ts_max"],
        "venue_counts": agg["venue"], "curve_regime_counts": agg["regime"],
        "curve_seed_valid_rows": agg["seed_valid_rows"],
        "curve_seed_valid_frac": agg["seed_valid_rows"] / max(agg["rows_kept"], 1),
        "is_mayhem_rows": agg["mayhem_rows"],
        "is_mayhem_null_masked_rows": agg["mayhem_null_masked_rows"],
        "lamports_per_sol": LAMPORTS_PER_SOL, "token_scale": TOKEN_SCALE,
        "k0": K0, "v0_lamports": V0_LAMPORTS, "t0_raw": T0_RAW,
        "k_violations": agg["k_viol"], "k_checked": agg["k_checked"],
        "k_rel_drift_median": float(np.median(kk)) if kk.size else None,
        "k_rel_drift_p90": float(np.percentile(kk, 90)) if kk.size else None,
        "k_rel_drift_p99": float(np.percentile(kk, 99)) if kk.size else None,
        "fee_beta_fit": (agg["fee_beta_num"] / agg["fee_beta_den"]) if agg["fee_beta_den"] else None,
        "last_fill_price_impact_bp_p50": float(np.median(im)) if im.size else None,
        "last_fill_price_impact_bp_p90": float(np.percentile(im, 90)) if im.size else None,
        "max_vsol_of_nongraduated": agg["grad_thresh_lo"],
        "min_vsol_of_graduated": agg["grad_thresh_hi"],
        "v_grad_lamports_used": V_GRAD_LAMPORTS,
    }
    json.dump(rep, open(a.report, "w"), indent=1)
    print(json.dumps({k2: v for k2, v in rep.items() if k2 not in ("venue_counts",)}, indent=1, default=str))


if __name__ == "__main__":
    main()
