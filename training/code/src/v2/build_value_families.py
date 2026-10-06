#!/usr/bin/env python
"""Edge-oriented supervision families.

Family A — utility_regression : causal state -> REALIZED executable utility.
    Kills the 0.93-AUC imitation ceiling: the target is an outcome sample, not
    the argmax of a rule. Graded on calibration, not accuracy. Plan §7.2/§7.1.

Family B — preference_pairs : same causal state, two actions scored by the
    accounting engine against the REALIZED path. Kept only when the realized
    utilities separate by a margin. DPO/GRPO-ready. The market is the judge.

Both take their inputs strictly from the causal state; the realized path is used
ONLY to build the target / to score the pair.
"""
import argparse, json, os, sys, collections
import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build_replay_v2 import (load_mint_series, load_seeds, score_actions,
                             HALF_COST_BP, ADD_FRACTION, ENGINE)

sys_A = ("You are an on-chain opportunity forecaster for pump.fun/pumpswap memecoins. "
         "Estimate the executable utility of a 300s round trip from the causal snapshot. "
         "One-way execution cost is ~180 bp. Answer in the fixed format.")


def fmt(x, nd=1):
    return "n/a" if x is None or not np.isfinite(x) else f"{x:.{nd}f}"


def state_block(s, t_dec):
    return (
        "CAUSAL STATE (strictly prior to the decision):\n"
        f"  t_dec_ms={t_dec}  age_s={fmt(s.get('age_s'),1)}  last_trade_age_s={fmt(s.get('last_trade_age_s'),2)}\n"
        f"  venue={s.get('venue')}  curve_venue_present={s.get('curve_venue_present')}  "
        f"evidence_status={s.get('evidence_status')}\n"
        f"  n_prior_trades={s.get('n_trades_prior')}  buy_count={s.get('buy_count')}  "
        f"sell_count={s.get('sell_count')}  unique_traders={s.get('unique_traders')}\n"
        f"  price_sol_per_raw={s.get('price_sol_per_raw')}\n"
        f"  net_flow_lamports={s.get('net_flow_lamports')}  sol_volume_lamports={s.get('sol_volume_lamports')}\n"
        f"  top1_trader_share={fmt(s.get('top1_trader_share'),3)}  top5_trader_share={fmt(s.get('top5_trader_share'),3)}  "
        f"buyer_seller_ratio={fmt(s.get('buyer_seller_ratio'),3)}\n"
        f"  ret_5s_bp={fmt(s.get('ret_5s_bp'))}  ret_30s_bp={fmt(s.get('ret_30s_bp'))}  "
        f"ret_300s_bp={fmt(s.get('ret_300s_bp'))}  vol_30s_bp={fmt(s.get('price_volatility_30s_bp'))}\n"
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--splits", default="train,val,test")
    ap.add_argument("--pair-margin-bp", type=float, default=150.0)
    a = ap.parse_args()
    want = set(a.splits.split(","))
    os.makedirs(a.out, exist_ok=True)

    lab = {}
    with open("/training/v2/canonical/ledger_v7/labels.jsonl") as f:
        for line in f:
            r = json.loads(line)
            lab[r["episode_id"]] = r

    vf = {s: open(f"{a.out}/value_{s}.jsonl", "w", buffering=1 << 20)
          for s in ("train", "validation", "test")}
    pf = {s: open(f"{a.out}/prefs_{s}.jsonl", "w", buffering=1 << 20)
          for s in ("train", "validation", "test")}
    n_val = 0
    vstats = collections.Counter()
    seeds = []
    with open("/training/v2/canonical/ledger_v7/states.jsonl") as f:
        for line in f:
            r = json.loads(line)
            ident = r["identity"]
            if ident["split"] not in want:
                continue
            out = r["outcome_evidence"]
            net = out.get("net_bp_300s")
            if net is None or not np.isfinite(net):
                continue
            st = r["state"]
            sp = "validation" if ident["split"] == "val" else ident["split"]
            if out.get("exit_executable") and not out.get("right_censored_300s"):
                user = ("Estimate the executable 300s outcome for this opportunity.\n"
                        "Return ONLY the fixed fields.\n\n" + state_block(st, r["decision_clock"]["t_dec_ms"]))
                asst = (f"FORECAST_NET_BP: {net:.1f}\n"
                        f"FORECAST_MFE_BP: {fmt(out.get('mfe_bp'))}\n"
                        f"FORECAST_MAE_BP: {fmt(out.get('mae_bp'))}\n"
                        f"BASIS: net_flow {st.get('net_flow_lamports')} lamports, "
                        f"{st.get('buy_count')} buys vs {st.get('sell_count')} sells, "
                        f"top1 share {fmt(st.get('top1_trader_share'),3)}\n"
                        f"EVIDENCE_STATUS: {st.get('evidence_status')}\n")
                vf[sp].write(json.dumps({"messages": [
                    {"role": "system", "content": sys_A},
                    {"role": "user", "content": user},
                    {"role": "assistant", "content": asst}],
                    "meta": {"family": "utility_regression", "mint": ident["mint"],
                             "episode_id": ident["episode_id"], "split": sp,
                             "truth_type": "realized_sample", "action": None}},
                    separators=(",", ":")) + "\n")
                n_val += 1
                vstats[sp] += 1
            if lab.get(ident["episode_id"], {}).get("action") == "BUY":
                seeds.append((ident["mint"], sp, int(r["decision_clock"]["t_dec_ms"]),
                              ident["episode_id"]))
    for f in vf.values():
        f.close()
    print(f"[VALUE] utility_regression records={n_val:,} {dict(vstats)}")
    print(f"[PREFS] candidate BUY seeds={len(seeds):,}")

    # ---------- Family B: preference pairs scored on the realized path ----------
    names, bounds, allt, allpx = load_mint_series()
    idx = {n: i for i, n in enumerate(names)}
    n_pairs = 0
    pstats = collections.Counter()
    marg = []
    for mint, sp, t_dec, eid in seeds:
        gi = idx.get(mint)
        if gi is None:
            continue
        lo, hi = bounds[gi], bounds[gi + 1]
        tt, pv = allt[lo:hi], allpx[lo:hi]
        j0 = int(np.searchsorted(tt, t_dec, side="right"))
        if j0 >= tt.size:
            continue
        entry_px = pv[j0]
        if not np.isfinite(entry_px) or entry_px <= 0:
            continue
        t_entry = int(tt[j0])
        qty = 0.5 / entry_px
        cash = 0.5
        end = min(t_entry + 1800_000, int(tt[-1]))
        ticks = np.arange(t_entry + 30_000, end + 1, 30_000, dtype=np.int64)
        if ticks.size == 0:
            continue
        npri = np.searchsorted(tt, ticks, side="left")
        act = (ticks - tt[npri - 1]) <= 60_000
        ticks, npri = ticks[act], npri[act]
        if ticks.size == 0:
            continue
        sel = (np.arange(min(8, ticks.size)) * (ticks.size / min(8, ticks.size))).astype(int)
        ticks, npri = ticks[sel], npri[sel]
        for k in range(ticks.size):
            tM, i = int(ticks[k]), int(npri[k])
            if i <= j0:
                continue
            cur = pv[i - 1]
            if not np.isfinite(cur) or cur <= 0 or (tM - t_entry) < 60_000:
                continue
            samples = []
            for hs in (60, 150, 300):
                jh = int(np.searchsorted(tt, tM + hs * 1000, side="right"))
                s2 = pv[max(i, j0):jh]; s2 = s2[np.isfinite(s2)]
                if s2.size:
                    samples.append(float(s2[-1]))
            if not samples:
                continue
            fm, fs = float(np.mean(samples)), float(np.std(samples))
            # realized utility of each action, net of costs, on the real path
            ow = HALF_COST_BP / 1e4
            u = {}
            u["EXIT"] = qty * cur * (1 - ow)
            u["HOLD"] = qty * fm * (1 - ow)
            spend = qty * ADD_FRACTION * cur * (1 + ow)
            u["ADD"] = (qty * (1 + ADD_FRACTION) * fm * (1 - ow) - spend) if spend <= cash + 1e-9 else -np.inf
            u["REDUCE"] = 0.5 * qty * cur * (1 - ow) + 0.5 * qty * fm * (1 - ow)
            order = sorted(u.items(), key=lambda kv: kv[1], reverse=True)
            (ba, bv), (wa, wv) = order[0], order[-1]
            margin_bp = (bv - wv) / max(qty * cur, 1e-18) * 1e4
            if ba == wa or margin_bp < a.pair_margin_bp:
                continue
            prompt = ("Choose the better action for a position you already hold. "
                      "One-way execution cost is ~180 bp.\n"
                      f"MINT: {mint}\nDECISION TIME (unix ms): {tM}\n"
                      f"MARK: {cur:.12g}\nENTRY: {entry_px:.12g}\n"
                      f"UNREALIZED_BP: {(cur/entry_px-1)*1e4:.1f}\n"
                      f"HELD_S: {(tM-t_entry)/1000:.0f}\n"
                      f"INVENTORY: {qty:.6g}\nCASH_SOL: {cash:.6g}")
            pf[sp].write(json.dumps({
                "prompt": prompt,
                "chosen": f"DECISION: {ba}\n",
                "rejected": f"DECISION: {wa}\n",
                "meta": {"family": "preference_pair", "mint": mint, "split": sp,
                         "episode_id": eid, "decision_time_unix_ms": tM,
                         "margin_bp": round(float(margin_bp), 1),
                         "profile": "realized_path_utility", "action": None}},
                separators=(",", ":")) + "\n")
            n_pairs += 1; pstats[sp] += 1; marg.append(margin_bp)
            break
    for f in pf.values():
        f.close()
    print(f"[PREFS] pairs={n_pairs:,} {dict(pstats)} margin_bp median={np.median(marg) if marg else 0:.0f}")
    json.dump({"value_records": int(n_val), "value_by_split": dict(vstats),
               "pairs": int(n_pairs), "pairs_by_split": dict(pstats),
               "pair_margin_bp": a.pair_margin_bp,
               "median_margin_bp": float(np.median(marg)) if marg else None},
              open("/training/v2/reports/EDGE_FAMILIES.json", "w"), indent=1)


if __name__ == "__main__":
    main()
