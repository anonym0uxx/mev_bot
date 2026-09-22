#!/usr/bin/env python3
"""Generate the state-ledger parity fixture from the corpus AUTHORITY.

The expected values are produced by the *same* expressions the corpus builder uses
(`build_states_v2._episode`), with `_ret_bp` imported from that module rather than
re-implemented, so the fixture cannot drift from the corpus by a transcription error.

Usage:
  python3 gen_state_ledger_fixture.py <out.json>
"""
import importlib.util
import json
import sys

import numpy as np

AUTHORITY = "/training/v2/code/src/v2/build_states_v2.py"


def load_authority():
    """Import the corpus authority and hand back its module.

    `build_states_v2` imports pyarrow at module scope for its parquet reader, which this
    fixture never touches (only `_ret_bp` is borrowed). Stub the import so borrowing one
    pure function does not require the whole pipeline dependency set.
    """
    import types

    if "pyarrow" not in sys.modules:
        pa = types.ModuleType("pyarrow")
        pa_json = types.ModuleType("pyarrow.json")
        pa.json = pa_json
        sys.modules["pyarrow"] = pa
        sys.modules["pyarrow.json"] = pa_json
    spec = importlib.util.spec_from_file_location("build_states_v2", AUTHORITY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def synth_tape():
    """A tape that exercises every branch: buys/sells, three traders, a mixed venue,
    a NaN price (non-finite → evidence partial + finite filtering), a zero price
    (the `seg[0] <= 0` guard), dust below the SOL floor, and a 30 s gap."""
    rows = []
    t = 1_700_000_000_000
    # (dt_ms, price, sol_lamports_signed, is_buy, trader, venue)
    rows.append((0, 2.0e-8, -900_000_000, True, 1, "pumpfun"))
    rows.append((400, 2.01e-8, -50_000_000, True, 2, "pumpfun"))
    rows.append((700, 1.99e-8, 40_000_000, False, 1, "pumpfun"))
    rows.append((1200, 2.05e-8, -25_000_000, True, 3, "pumpfun"))
    rows.append((1500, 2.10e-8, 12_000_000, False, 2, "pumpfun"))
    rows.append((2100, 2.20e-8, -300_000_000, True, 1, "pumpfun"))
    rows.append((2600, float("nan"), -70_000_000, True, 2, "pumpfun"))
    rows.append((3000, 2.30e-8, 33_000_000, False, 3, "pumpfun"))
    rows.append((4000, 2.25e-8, -18_000_000, True, 1, "pumpfun"))
    rows.append((5100, 2.40e-8, -140_000_000, True, 2, "pumpswap"))
    rows.append((6000, 0.0, -15_000_000, True, 3, "pumpswap"))
    rows.append((7000, 2.50e-8, 60_000_000, False, 1, "pumpswap"))
    rows.append((8000, 2.55e-8, -22_000_000, True, 2, "pumpswap"))
    rows.append((9500, 2.60e-8, -31_000_000, True, 3, "pumpswap"))
    rows.append((11_000, 2.58e-8, 45_000_000, False, 1, "pumpswap"))
    rows.append((12_500, 2.70e-8, -260_000_000, True, 2, "pumpswap"))
    rows.append((14_000, 2.72e-8, -80_000_000, True, 3, "pumpswap"))
    rows.append((16_000, 2.68e-8, 95_000_000, False, 1, "pumpswap"))
    rows.append((18_000, 2.80e-8, -120_000_000, True, 2, "pumpswap"))
    rows.append((20_000, 2.90e-8, -55_000_000, True, 3, "pumpswap"))
    rows.append((23_000, 3.00e-8, 70_000_000, False, 1, "pumpswap"))
    rows.append((26_000, 3.05e-8, -44_000_000, True, 2, "pumpswap"))
    rows.append((29_000, 3.10e-8, -19_000_000, True, 3, "pumpswap"))
    rows.append((32_000, 3.02e-8, 150_000_000, False, 1, "pumpswap"))
    rows.append((35_000, 3.20e-8, -210_000_000, True, 2, "pumpswap"))
    rows.append((38_000, 3.30e-8, -33_000_000, True, 3, "pumpswap"))
    # A 90 s dead gap (outside every trailing window).
    rows.append((128_000, 3.40e-8, -66_000_000, True, 1, "pumpswap"))
    rows.append((131_000, 3.45e-8, 21_000_000, False, 2, "pumpswap"))
    rows.append((134_000, 3.50e-8, -28_000_000, True, 3, "pumpswap"))
    rows.append((137_000, 3.55e-8, -24_000_000, True, 1, "pumpswap"))
    rows.append((140_000, 3.60e-8, 18_000_000, False, 2, "pumpswap"))
    rows.append((143_000, 3.66e-8, -40_000_000, True, 3, "pumpswap"))
    rows.append((146_000, 3.70e-8, -36_000_000, True, 1, "pumpswap"))
    rows.append((149_000, 3.75e-8, 12_000_000, False, 2, "pumpswap"))
    rows.append((152_000, 3.80e-8, -52_000_000, True, 3, "pumpswap"))
    rows.append((155_000, 3.85e-8, -48_000_000, True, 1, "pumpswap"))
    rows.append((158_000, 3.90e-8, 30_000_000, False, 2, "pumpswap"))
    rows.append((161_000, 3.95e-8, -58_000_000, True, 3, "pumpswap"))
    rows.append((164_000, 4.00e-8, -62_000_000, True, 1, "pumpswap"))
    rows.append((167_000, 4.05e-8, 25_000_000, False, 2, "pumpswap"))

    trades = []
    for dt, price, sol, is_buy, trader, venue in rows:
        trades.append(
            {
                "recv_unix_ms": t + dt,
                "price_sol_per_raw": None if price != price else price,
                "sol_lamports_signed": int(sol),
                "is_buy": bool(is_buy),
                "trader": int(trader),
                "venue": venue,
            }
        )
    return trades


CONC_WINDOW = 2000


def expected_snapshot(tt, pv, sv, sd, tc, vv, i, t_dec, ret_bp):
    """Mirror of `build_states_v2._episode`'s state block, line for line."""
    ph = pv[:i]
    fin = np.isfinite(ph)
    last_fin = int(np.nonzero(fin)[0][-1])
    price = float(ph[last_fin])
    buy = sd[:i]
    vol = np.abs(sv[:i])
    nbuy = int(buy.sum())
    nsell = int(i - nbuy)
    age_s = (t_dec - int(tt[0])) / 1000.0
    last_age = (t_dec - int(tt[i - 1])) / 1000.0
    lo = max(0, i - CONC_WINDOW)
    agg = np.bincount(tc[lo:i], weights=vol[lo:])
    tot = agg.sum()
    uniq = int((agg > 0).sum())
    if tot > 0:
        top = np.sort(agg)[::-1]
        top1 = float(top[:1].sum() / tot)
        top5 = float(top[:5].sum() / tot)
    else:
        top1 = top5 = None

    volat = None
    if i >= 30:
        j30 = int(np.searchsorted(tt, t_dec - 30 * 1000, side="left"))
        seg = pv[j30:i]
        seg = seg[np.isfinite(seg)]
        if seg.size >= 3 and (seg > 0).all():
            volat = float(np.std(np.diff(np.log(seg))) * 10000.0)

    ven = set(vv[max(0, i - 200) : i].tolist())
    venue = "mixed" if len(ven) > 1 else (list(ven)[0] if ven else "unknown")

    return {
        "n_prior_trades": int(i),
        "buy_count": nbuy,
        "sell_count": nsell,
        "unique_traders": uniq,
        "sol_volume_lamports": int(vol.sum()),
        "buy_volume_lamports": int(vol[buy].sum()),
        "sell_volume_lamports": int(vol[~buy].sum()),
        "net_flow_lamports": int(vol[buy].sum()) - int(vol[~buy].sum()),
        "price_sol_per_raw": price,
        "age_s": age_s,
        "last_trade_age_s": last_age,
        "top1_trader_share": top1,
        "top5_trader_share": top5,
        "buyer_seller_ratio": (nbuy / nsell) if nsell else None,
        "price_volatility_30s_bp": volat,
        "venue": venue,
        "evidence_status": "complete" if bool(fin.all()) else "partial",
        "ret_5s_bp": ret_bp(tt, pv, i, price, 5, t_dec),
        "ret_30s_bp": ret_bp(tt, pv, i, price, 30, t_dec),
        "ret_300s_bp": ret_bp(tt, pv, i, price, 300, t_dec),
    }


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/state_ledger_parity.json"
    mod = load_authority()
    trades = synth_tape()
    tt = np.array([t["recv_unix_ms"] for t in trades], dtype=np.int64)
    pv = np.array(
        [np.nan if t["price_sol_per_raw"] is None else t["price_sol_per_raw"] for t in trades],
        dtype=np.float64,
    )
    sv = np.array([t["sol_lamports_signed"] for t in trades], dtype=np.int64)
    sd = np.array([t["is_buy"] for t in trades], dtype=bool)
    vv = np.array([t["venue"] for t in trades], dtype=object)
    tc = np.array([t["trader"] for t in trades], dtype=np.int64)

    clocks = []
    for idx in (20, 26, 39):
        t_dec = int(tt[idx])
        i = int(np.searchsorted(tt, t_dec, side="left"))
        clocks.append({"t_dec_ms": t_dec, "expected": expected_snapshot(tt, pv, sv, sd, tc, vv, i, t_dec, mod._ret_bp)})
    # A clock that lands between trades: strictly-causal selection must agree.
    t_dec = int(tt[25]) + 1
    i = int(np.searchsorted(tt, t_dec, side="left"))
    clocks.append({"t_dec_ms": t_dec, "expected": expected_snapshot(tt, pv, sv, sd, tc, vv, i, t_dec, mod._ret_bp)})

    doc = {"authority": AUTHORITY, "trades": trades, "clocks": clocks}
    with open(out, "w") as f:
        json.dump(doc, f, indent=1)
    print(f"wrote {out}: {len(trades)} trades, {len(clocks)} clocks")


if __name__ == "__main__":
    main()
