#!/usr/bin/env python
"""Stage 3 — causal state ledger builder (v2). See contracts/DECISION_CONTRACT.md.

Emits one record per decision clock with a strictly-causal `state` block and a
strictly-future `outcome_evidence` block. Ties at exactly t_dec are EXCLUDED
(side='left'), so causality means recv_unix_ms < t_dec.

Outputs:
  <out>/states.jsonl
  reports/LEDGER_RECONCILIATION.json
"""
import argparse, json, hashlib, sys, time, os
import numpy as np
import pyarrow as pa
import pyarrow.json as paj

INTERVAL_S = 30
MIN_PRIOR = 20
HORIZONS = (30, 300, 1800)
RET_HORIZONS = (5, 30, 300)
PUMP_FEE_BPS = 100
SLIPPAGE_BPS = 50
PRIORITY_FEE_BPS = 8
FAILURE_RATE = 0.10
FAILED_COST_BPS = 220
CONC_WINDOW = 2000          # trades used for trader-concentration (causal)
MAX_IDLE_MS = 60_000        # a clock requires a trade within the last 60s
MAX_TICKS_PER_MINT = 240    # 2h of continuous activity at 30s cadence
MIN_SOL_LAMPORTS = 100_000  # 1e-4 SOL: below this it is a pass-through leg,
                            # not a swap -- its price is meaningless (this is
                            # what produced 1e-11 prices and 1e12x moves)
MIN_TOKENS_RAW = 1_000_000  # mirror floor on the TOKEN leg. A rounding-residue
                            # token delta (~1e-3) with a normal SOL leg yields
                            # the 1e9 prices that dominate the ratio tail.


def cost_bp():
    return (2 * PUMP_FEE_BPS) + SLIPPAGE_BPS + PRIORITY_FEE_BPS + FAILURE_RATE * FAILED_COST_BPS


def _sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _ret_bp(tt, pv, i, price, win_s, t_dec):
    """return vs price at first trade within the trailing win_s window (strictly causal)."""
    j = int(np.searchsorted(tt, t_dec - win_s * 1000, side="left"))
    if j >= i:
        return None
    seg = pv[j:i]
    seg = seg[np.isfinite(seg)]
    if seg.size == 0 or seg[0] <= 0:
        return None
    return float((price - seg[0]) / seg[0] * 10000.0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True)
    ap.add_argument("--split-manifest", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit-mints", type=int, default=0)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    os.makedirs("reports", exist_ok=True)

    sm = json.load(open(a.split_manifest))
    mint_split = {}
    for sp in ("train", "val", "test", "protected_holdout"):
        for m in sm["splits"].get(sp, []):
            mint_split[m] = sp

    t0 = time.time()
    tbl = paj.read_json(f"{a.root}/trades.jsonl")
    need = ["mint", "trader", "side", "venue", "slot", "recv_unix_ms",
            "sol_lamports", "tokens_raw", "status"]
    tbl = tbl.select(need)
    print(f"[LEDGER] read {tbl.num_rows:,} trades in {time.time()-t0:.1f}s")
    import pyarrow.compute as pc

    # C++ dictionary encoding (fast) instead of np.unique on python objects
    dm = pc.dictionary_encode(tbl.column("mint").combine_chunks())
    dt = pc.dictionary_encode(tbl.column("trader").combine_chunks())
    mint_dict = dm.dictionary.to_pylist()
    trader_dict = dt.dictionary.to_pylist()
    mint_codes = np.asarray(dm.indices)
    tcode = np.asarray(dt.indices).astype(np.int64)
    del dm, dt

    side = np.asarray(pc.equal(tbl.column("side"), pa.scalar("buy")))
    venue_l = tbl.column("venue").to_pylist()
    slot = np.asarray(tbl.column("slot"))
    tms = np.asarray(tbl.column("recv_unix_ms")).astype(np.int64)
    sol = np.asarray(tbl.column("sol_lamports")).astype(np.int64)
    tok = np.asarray(tbl.column("tokens_raw")).astype(np.int64)
    del tbl

    allowed = np.array([v in mint_split for v in mint_dict], dtype=bool)
    keep = allowed[mint_codes]
    # value-leg floor: a resolved trader moving 1e11 tokens for 2 lamports is a
    # routing pass-through, not a swap. Its implied price is garbage.
    value_ok = (np.abs(sol) >= MIN_SOL_LAMPORTS) & (np.abs(tok) >= MIN_TOKENS_RAW)
    dropped_dust = int((keep & ~value_ok).sum())
    keep &= value_ok
    n_uni = int(keep.sum())
    print(f"[LEDGER] trades in split universe: {n_uni:,} / {len(keep):,}")
    if n_uni == 0:
        raise SystemExit("FATAL: zero trades matched the split universe")
    if a.limit_mints:
        limited = set(sorted(mint_split.keys())[:a.limit_mints])
        allowed = np.array([(v in mint_split) and (v in limited) for v in mint_dict], dtype=bool)
        keep = allowed[mint_codes]
        n_uni = int(keep.sum())

    mint_codes = mint_codes[keep]; tcode = tcode[keep]; side = side[keep]
    slot = slot[keep]; tms = tms[keep]; sol = sol[keep]; tok = tok[keep]
    venue_l = [v for v, k in zip(venue_l, keep) if k]
    del keep

    with np.errstate(divide="ignore", invalid="ignore"):
        px = np.where(tok != 0,
                      np.abs(sol).astype(np.float64) / np.abs(tok).astype(np.float64),
                      np.nan)

    # remap mint codes to compact 0..K-1 over the surviving universe
    uc, code_inv = np.unique(mint_codes, return_inverse=True)
    code_inv = code_inv.astype(np.int64)
    venue = np.array(venue_l, dtype=object)
    # single global sort by (mint_code, time, slot) -> contiguous mint runs
    order = np.lexsort((slot, tms, code_inv))
    code_inv = code_inv[order]; tms = tms[order]; slot = slot[order]
    venue = venue[order]; px = px[order]; sol = sol[order]; tok = tok[order]
    side = side[order]; tcode = tcode[order]

    mint_names = [mint_dict[i] for i in uc]
    boundaries = np.searchsorted(code_inv, np.arange(uc.size + 1))
    print(f"[LEDGER] grouped {uc.size:,} mints in {time.time()-t0:.1f}s")

    out_path = f"{a.out}/states.jsonl"
    fout = open(out_path, "w", buffering=1 << 20)
    n_ep = 0; n_mint = 0
    rc = {h: 0 for h in HORIZONS}; nt = {h: 0 for h in HORIZONS}
    split_counts = {"train": 0, "val": 0, "test": 0, "protected_holdout": 0}

    for gi in range(uc.size):
        lo, hi = boundaries[gi], boundaries[gi + 1]
        n = hi - lo
        if n < MIN_PRIOR + 1:
            continue
        m = mint_names[gi]
        tt = tms[lo:hi]; pv = px[lo:hi]; sv = sol[lo:hi]; vv = venue[lo:hi]; sd = side[lo:hi]
        tc = tcode[lo:hi]
        # ROBUST BAND: a leg can clear both size floors and still be a
        # mis-resolved account (large SOL against the token minimum), which
        # yields 1e9 prices. Keep only trades within 10x of THIS mint's own
        # median price -- the mint is its own reference, so real moves survive.
        fin = np.isfinite(pv)
        if fin.sum() >= 5:
            med = float(np.median(pv[fin]))
            band = fin & (pv >= med / 10.0) & (pv <= med * 10.0)
            tt = tt[band]; pv = pv[band]; sv = sv[band]; vv = vv[band]
            sd = sd[band]; tc = tc[band]
        if tt.size < MIN_PRIOR + 1:
            continue
        first_t = int(tt[0]); last_t = int(tt[-1])
        if last_t - first_t < INTERVAL_S * 1000:
            continue
        ticks = np.arange(first_t + INTERVAL_S * 1000, last_t + 1, INTERVAL_S * 1000, dtype=np.int64)
        nprior = np.searchsorted(tt, ticks, side="left")
        ok = nprior >= MIN_PRIOR
        if not ok.any():
            continue
        ticks = ticks[ok]; nprior = nprior[ok]
        # ACTIVITY GATE: a decision clock only exists while the market is alive.
        # Without this, a mint seen in two capture sessions (weeks apart) emits a
        # tick every 30s across the dead gap -- pure noise, dwarfs real signals.
        act = (ticks - tt[nprior - 1]) <= MAX_IDLE_MS
        if not act.any():
            continue
        ticks = ticks[act]; nprior = nprior[act]
        if ticks.size > MAX_TICKS_PER_MINT:
            ticks = ticks[:MAX_TICKS_PER_MINT]; nprior = nprior[:MAX_TICKS_PER_MINT]
        sp = mint_split.get(m, "train")
        for k in range(ticks.size):
            rec = _episode(m, sp, int(ticks[k]), tt, sd, pv, sv, vv, tc, int(nprior[k]), n_ep)
            if rec is None:
                continue
            fout.write(json.dumps(rec, separators=(",", ":")) + "\n")
            n_ep += 1; split_counts[sp] += 1
            oe = rec["outcome_evidence"]
            for h in HORIZONS:
                d = oe["by_horizon"][str(h)]
                if d["has_trade_within"]:
                    nt[h] += 1
                if d["right_censored"]:
                    rc[h] += 1
        n_mint += 1
        if n_mint % 1000 == 0:
            el = time.time() - t0
            print(f"  ... mints={n_mint:,} episodes={n_ep:,} elapsed={el:.0f}s rate={n_ep/max(el,1):,.0f}/s")

    fout.close()
    el = time.time() - t0
    if n_ep == 0:
        raise SystemExit("FATAL: zero episodes written (refusing to report clean)")
    recon = {
        "schema": "north_star_ledger_reconciliation_v2",
        "trades_in_universe": n_uni, "mints_processed": n_mint, "episodes": n_ep,
        "episodes_by_split": split_counts,
        "interval_s": INTERVAL_S, "min_prior_trades": MIN_PRIOR,
        "horizon_has_trade_within": {str(h): nt[h] for h in HORIZONS},
        "horizon_right_censored": {str(h): rc[h] for h in HORIZONS},
        "cost_model_bp": round(cost_bp(), 1),
        "elapsed_s": round(el, 1), "output": out_path, "output_sha256": _sha256_file(out_path),
    }
    json.dump(recon, open("reports/LEDGER_RECONCILIATION.json", "w"), indent=1)
    print(f"[LEDGER] episodes={n_ep:,} mints={n_mint:,} in {time.time()-t0:.0f}s -> {out_path}")
    print(f"[LEDGER] dust_value_legs_dropped={dropped_dust:,}")
    print(f"[LEDGER] by split {split_counts}")
    print(f"[LEDGER] right_censored {rc}")
    return 0


def _episode(m, sp, t_dec, tt, sd, pv, sv, vv, tc, i, n_ep):
    ph = pv[:i]
    fin = np.isfinite(ph)
    if not fin.any():
        return None
    last_fin = int(np.nonzero(fin)[0][-1])
    price = float(ph[last_fin])
    if not (price > 0):
        return None
    buy = sd[:i]
    vol = np.abs(sv[:i])
    nbuy = int(buy.sum()); nsell = int(i - nbuy)
    age_s = (t_dec - int(tt[0])) / 1000.0
    last_age = (t_dec - int(tt[i - 1])) / 1000.0
    lo = max(0, i - CONC_WINDOW)
    # concentration + unique traders via bincount on a bounded causal window
    agg = np.bincount(tc[lo:i], weights=vol[lo:])
    tot = agg.sum()
    uniq_traders = int((agg > 0).sum())
    if tot > 0:
        top = np.sort(agg)[::-1]
        top1 = float(top[:1].sum() / tot); top5 = float(top[:5].sum() / tot)
    else:
        top1 = top5 = None

    volat = None
    if i >= 30:  # need enough history to look back 30s
        j30 = int(np.searchsorted(tt, t_dec - 30 * 1000, side="left"))
        seg = pv[j30:i]; seg = seg[np.isfinite(seg)]
        if seg.size >= 3 and (seg > 0).all():
            volat = float(np.std(np.diff(np.log(seg))) * 10000.0)

    ven = set(vv[max(0, i - 200):i].tolist())
    venue = "mixed" if len(ven) > 1 else (list(ven)[0] if ven else "unknown")

    j0 = int(np.searchsorted(tt, t_dec, side="right"))
    entry = float(pv[j0]) if j0 < tt.size and np.isfinite(pv[j0]) else None
    fut = {}
    for H in HORIZONS:
        tH = t_dec + H * 1000
        obs = bool(int(tt[-1]) >= tH)
        jH = int(np.searchsorted(tt, tH, side="right"))
        has = jH > j0
        pwin = pv[j0:jH]; pwin = pwin[np.isfinite(pwin)]
        exit_p = float(pwin[-1]) if pwin.size else None
        gross = None; net = None
        if entry and exit_p and entry > 0:
            gross = float((exit_p - entry) / entry * 10000.0)
            net = float(gross - cost_bp())
        mfe = float((pwin.max() - entry) / entry * 10000.0) if (pwin.size and entry) else None
        mae = float((pwin.min() - entry) / entry * 10000.0) if (pwin.size and entry) else None
        fut[H] = {
            "entry_price_sol_per_raw": entry, "exit_price_sol_per_raw": exit_p,
            "gross_ret_bp": gross, "net_bp": net, "mfe_bp": mfe, "mae_bp": mae,
            "observed_through": obs, "has_trade_within": bool(has),
            "right_censored": (not obs), "exit_executable": bool(has),
        }
    rets = {f"ret_{w}s_bp": _ret_bp(tt, pv, i, price, w, t_dec) for w in RET_HORIZONS}

    return {
        "identity": {"episode_id": f"v2:{m}:{t_dec}", "mint": m,
                     "source_group": "laserstream_capture_v6", "split": sp,
                     "builder_revision": "build_states_v2.0"},
        "decision_clock": {"t_dec_ms": t_dec, "interval_s": INTERVAL_S,
                           "n_prior_trades": i, "ordering_certainty": "strict_prior"},
        "state": {
            "price_sol_per_raw": price, "n_trades_prior": i,
            "buy_count": nbuy, "sell_count": nsell, "unique_traders": uniq_traders,
            "sol_volume_lamports": int(vol.sum()), "buy_volume_lamports": int(vol[buy].sum()),
            "sell_volume_lamports": int(vol[~buy].sum()),
            "net_flow_lamports": int(vol[buy].sum()) - int(vol[~buy].sum()),
            "age_s": age_s, "last_trade_age_s": last_age,
            "venue": venue, "curve_venue_present": venue != "unknown",
            "top1_trader_share": top1, "top5_trader_share": top5,
            "buyer_seller_ratio": (nbuy / nsell) if nsell else None,
            "price_volatility_30s_bp": volat,
            "evidence_status": "complete" if bool(fin.all()) else "partial",
            **rets,
        },
        "action_space": ["BUY", "WATCH", "SKIP"],
        "outcome_evidence": {
            "entry_price_sol_per_raw": entry,
            "exit_price_300s": fut[300]["exit_price_sol_per_raw"],
            "gross_ret_300s_bp": fut[300]["gross_ret_bp"],
            "net_bp_300s": fut[300]["net_bp"],
            "mfe_bp": fut[300]["mfe_bp"], "mae_bp": fut[300]["mae_bp"],
            "right_censored_300s": fut[300]["right_censored"],
            "observed_through_300s": fut[300]["observed_through"],
            "has_trade_within_300s": fut[300]["has_trade_within"],
            "exit_executable": fut[300]["exit_executable"],
            "by_horizon": {str(h): fut[h] for h in HORIZONS},
        },
    }


if __name__ == "__main__":
    sys.exit(main())
