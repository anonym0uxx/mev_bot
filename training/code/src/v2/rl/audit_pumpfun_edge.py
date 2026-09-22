#!/usr/bin/env python
"""Does the new economics eat the pump.fun edge? Measured on the trained corpus.

WHY THIS EXISTS. The economics added on 2026-09-21 (G4/G5 payability, the 90 bp own-impact
veto, and the payout-reserve capacity bound the gate has always applied) can each refuse a
trade outright. On pump.fun the edge lives in exactly the thin, young curve markets those
rules bite hardest, so 'the rules refuse a lot' is not the question — the question is
whether they refuse the trades the LABELS say are worth taking.

This script joins the two halves of the trained corpus:
  * the INPUTS (venue, curve reserves, pool depth, price) parsed from the user prompt, and
  * the LABEL (action + size) the corpus was trained to emit.
and reports, per venue and per depth decile, how many BUY-labeled rows each rule refuses.

A high refusal rate on SKIP/WATCH rows is the rules working. A high refusal rate on BUY
rows is edge lost, and the number to watch is the curve-venue one.

Run: /home/alon/qwen27b-venv/bin/python audit_pumpfun_edge.py [--n 40000]
"""
import argparse
import collections
import json
import re

CORPUS = "/training/v2/candidate_sft_c12/train.jsonl"
SOL = 1_000_000_000
OWN_IMPACT_VETO_BP = 90          # impact_cap::CHAMPION_MAX_OWN_IMPACT_BPS
RULED_CLIP_SOL = {"pumpfun": 0.25, "pumpswap": 1.00}

RE_VENUE = re.compile(r"venue=(\S+)")
RE_TDEC = re.compile(r"t_dec_ms=(\d+)")
RE_CURVE = re.compile(
    r"curve_reserves=(\w+)(?: reason=(\S+))?.*?v_sol_reserves_sol=([\d.]+) "
    r"v_tokens_reserves=(\d+) real_sol_reserves_sol=([\d.]+) real_tokens_reserves=(\d+) "
    r"curve_price_sol_per_raw_token=([\d.eE+-]+) curve_k=(\d+) curve_progress=([\d.]+) "
    r"curve_regime=(\w+)")
RE_CURVE_ABSENT = re.compile(r"CURVE STATE \(at decision time\): curve_reserves=absent reason=(\S+)")
RE_AMM_ABSENT = re.compile(r"AMM POOL STATE \(at decision time\): amm_reserves=absent reason=(\S+)")
RE_AMM = re.compile(
    r"AMM POOL STATE \(at decision time\): amm_reserves=present src=\S+ pool=(\S+) quote=(\S+) "
    r"staleness_ms=(\d+) pricing_eligible=(\w+) base_reserves_raw=(\d+) "
    r"quote_reserves_lamports=(\d+) quote_reserves_sol=([\d.]+) "
    r"amm_price_sol_per_raw_token=([\d.eE+-]+)")
RE_ACTION = re.compile(r"\b(BUY|WATCH|SKIP)\b")


def parse_row(r):
    try:
        msgs = r["messages"]
        u = msgs[1]["content"]
        a = msgs[-1]["content"]
    except Exception:
        return None
    m_v = RE_VENUE.search(u)
    venue = m_v.group(1) if m_v else "unknown"
    out = {"venue": venue, "mint": r.get("mint") or (r.get("meta") or {}).get("mint"),
           "curve": None, "amm": None, "curve_absent": None, "amm_absent": None,
           "label": None, "clip_sol": None}
    mc = RE_CURVE.search(u)
    if mc:
        out["curve"] = {"present": mc.group(1) == "present",
                        "v_sol": float(mc.group(3)), "real_sol": float(mc.group(5)),
                        "regime": mc.group(10), "progress": float(mc.group(9))}
    else:
        ma = RE_CURVE_ABSENT.search(u)
        if ma:
            out["curve_absent"] = ma.group(1)
    mm = RE_AMM.search(u)
    if mm:
        out["amm"] = {"pool": mm.group(1), "quote": mm.group(2),
                      "quote_sol": float(mm.group(7)), "eligible": mm.group(4) == "true",
                      "staleness_ms": int(mm.group(3))}
    else:
        mabs = RE_AMM_ABSENT.search(u)
        if mabs:
            out["amm_absent"] = mabs.group(1)
    # the label: the corpus's own action + the size it chose
    head = a.strip().split("\n")[0]
    act = RE_ACTION.search(head) or RE_ACTION.search(a)
    size = None
    for tok in ("SMALL", "MID", "FULL", "NONE"):
        if re.search(r"\b%s\b" % tok, a):
            size = tok
            break
    out["label"] = act.group(1) if act else None
    out["label_size"] = size
    # THE RULED CLIP IS BY VENUE OF OUR NEXT FILL, not by the tape's venue label. A 'mixed'
    # market (the tape saw both venues) still has exactly one venue our fill lands in: the
    # pool when one exists, the curve otherwise. Sizing off the tape label alone leaves the
    # mixed rows unsized (8,054 of the first 40,000), which is how a whole class of markets
    # can quietly escape the audit.
    out["regime_venue"] = "pumpswap" if out["amm"] else "pumpfun"
    out["clip_sol"] = RULED_CLIP_SOL[out["regime_venue"]]
    # ---- THE OUTCOME AUTHORITY. `barrier_triplet` is the realized outcome the entry label
    # was derived from (mfe/mae over the 1,800 s horizon + the venue's cost floor), so the
    # question "does this rule cut winners or junk?" is answerable without a simulation.
    bt = (r.get("meta") or {}).get("barrier_triplet") or {}
    out["mfe_bp"] = bt.get("mfe_bp")
    out["mae_bp"] = bt.get("mae_bp")
    out["outcome"] = bt.get("outcome")
    cfs = bt.get("cost_floor_bps") or {}
    out["cost_floor_bp"] = cfs.get(out["regime_venue"] == "pumpswap" and "amm" or "bonding_curve")
    return out


def verdicts(row):
    """Every rule that can refuse this row, with the number that decided it."""
    v = {}
    clip = row["clip_sol"]
    c, am = row["curve"], row["amm"]
    if clip is None:
        v["no_ruled_clip"] = True
        return v
    if am:                                  # an AMM market: depth is the pool's SOL side
        depth = am["quote_sol"]
        v["impact_bp"] = clip / depth * 1e4 if depth else None
        v["impact_veto"] = bool(depth and (clip / depth * 1e4) > OWN_IMPACT_VETO_BP)
        v["capacity_refuse"] = False        # the pool's SOL side is the capacity; it is > 0
        v["amm_stale"] = not am["eligible"]
        v["depth_sol"] = depth
    elif c and c["present"]:
        depth = c["v_sol"]                   # the corpus's own depth notion for the curve
        v["impact_bp"] = clip / depth * 1e4 if depth else None
        v["impact_veto"] = bool(depth and (clip / depth * 1e4) > OWN_IMPACT_VETO_BP)
        # The gate's capacity bound is the PAYOUT reserve (real_sol) as observed BEFORE our
        # buy. On a curve, real_sol -> 0 as the mint is born, so this bound refuses exactly
        # the youngest markets. Recorded separately because it is the one that can cost edge.
        v["capacity_refuse"] = c["real_sol"] < clip
        v["depth_sol"] = depth
    else:
        v["no_reserves"] = True
    return v


def pct(a, q):
    import numpy as np
    return float(np.percentile(a, q)) if len(a) else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=40000)
    a = ap.parse_args()

    rows = []
    with open(CORPUS, encoding="utf-8") as fh:
        for i, line in enumerate(fh):
            if i >= a.n:
                break
            try:
                r = json.loads(line)
            except Exception:
                continue
            p = parse_row(r)
            if p:
                rows.append(p)

    print("rows parsed: %d" % len(rows))
    by_venue = collections.Counter(r["venue"] for r in rows)
    print("by venue: %s" % dict(by_venue))
    lab = collections.Counter((r["venue"], r["label"]) for r in rows)
    print("label mix (venue, action): %s" % dict(sorted(lab.items())))

    # the rule audit, per venue and per label
    for venue in sorted(by_venue):
        sub = [r for r in rows if r["venue"] == venue]
        if not sub:
            continue
        vs = [verdicts(r) for r in sub]
        clipped = [r for r in sub if r["clip_sol"] is not None]
        print("\n== %s: %d rows, %d with a ruled clip" % (venue, len(sub), len(clipped)))
        for rule in ("impact_veto", "capacity_refuse", "amm_stale", "no_reserves"):
            hits = [r for r, v in zip(sub, vs) if v.get(rule)]
            if not hits:
                continue
            hl = collections.Counter(r["label"] for r in hits)
            print("   %-16s refuses %5d rows  labels=%s"
                  % (rule, len(hits), dict(sorted(hl.items(), key=lambda kv: str(kv[0])))))
        depths = [v["depth_sol"] for v in vs if v.get("depth_sol")]
        if depths:
            print("   depth SOL: p10=%.2f p50=%.2f p90=%.2f"
                  % (pct(depths, 10), pct(depths, 50), pct(depths, 90)))
        imps = [v["impact_bp"] for v in vs if v.get("impact_bp")]
        if imps:
            print("   ruled-clip impact bp: p50=%.1f p90=%.1f p99=%.1f max=%.1f (veto>%d)"
                  % (pct(imps, 50), pct(imps, 90), pct(imps, 99), max(imps), OWN_IMPACT_VETO_BP))

    # the number that matters: BUY-labeled rows the rules refuse, by venue
    print("\n== EDGE AT RISK (BUY-labeled rows refused)")
    for venue in sorted(by_venue):
        buys = [(r, verdicts(r)) for r in rows if r["venue"] == venue and r["label"] == "BUY"]
        if not buys:
            continue
        iv = sum(1 for _, v in buys if v.get("impact_veto"))
        cr = sum(1 for _, v in buys if v.get("capacity_refuse"))
        either = sum(1 for _, v in buys if v.get("impact_veto") or v.get("capacity_refuse"))
        print("   %-9s BUY rows %5d | impact-vetoed %5d | capacity-refused %5d | either %5d (%.1f%%)"
              % (venue, len(buys), iv, cr, either, 100.0 * either / len(buys)))

    # ---- THE PROFITABILITY HALF: what did the refused rows actually do?
    print("\n== DID THE REFUSED ROWS PAY? (BUY rows, realized barrier outcome)")
    for venue in sorted(by_venue):
        buys = [(r, verdicts(r)) for r in rows
                if r["venue"] == venue and r["label"] == "BUY" and r.get("mfe_bp") is not None]
        if not buys:
            continue
        adm = [r for r, v in buys if not (v.get("impact_veto") or v.get("capacity_refuse"))]
        ref = [r for r, v in buys if (v.get("impact_veto") or v.get("capacity_refuse"))]
        for tag, grp in (("admitted", adm), ("refused ", ref)):
            if not grp:
                continue
            mfe = [r["mfe_bp"] for r in grp]
            mae = [r["mae_bp"] for r in grp if r.get("mae_bp") is not None]
            clears = [1 for r in grp
                      if r.get("cost_floor_bp") is not None
                      and r["mfe_bp"] > r["cost_floor_bp"]]
            print("   %-9s %s n=%5d | MFE p50=%9.1f p90=%9.1f | MAE p50=%8.1f | clears floor %5.1f%%"
                  % (venue, tag, len(grp), pct(mfe, 50), pct(mfe, 90), pct(mae, 50),
                     100.0 * len(clears) / len(grp)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
