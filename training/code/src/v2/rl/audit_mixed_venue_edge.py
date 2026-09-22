#!/usr/bin/env python
"""Is the MIXED venue bucket losing tradeable edge, or refusing junk we cannot size?

WHY. audit_pumpfun_edge.py reported that on `venue=mixed` rows the rules refuse
BUY-labeled decisions whose MFE is HIGHER than the admitted ones and which clear the
cost floor MORE often (86.7% vs 78.7%, n=361 vs 1242). MFE is a maximum, not a
realized exit, so that read is suggestive, not a verdict. This script answers the
next question with the corpus's own realized machinery:

  * the realized barrier OUTCOME per row (tp / sl / timeout / censored) and the
    mark at the horizon (meta.barrier_triplet.last_bp), so the comparison is
    net-of-cost returns, not excursions;
  * WHICH rule refused (own-impact veto at the ruled clip vs the curve payout-reserve
    capacity bound vs stale AMM pricing) and at WHICH size;
  * the SIZE counterfactual that matters: a mixed row whose fill lands in a THIN pool
    is vetoed at the pumpswap ruled clip (1.00 SOL). If the same decision were sized
    to the curve clip (0.25 SOL) the own-impact is 4x smaller. How many refused rows
    become admissible, and what did THOSE rows actually realize?

Usage: python audit_mixed_venue_edge.py [--n 0]   (0 = the whole corpus)
"""
import argparse
import collections
import json
import re

CORPUS = "/training/v2/candidate_sft_c12/train.jsonl"
SOL = 1_000_000_000
OWN_IMPACT_VETO_BP = 90
RULED_CLIP_SOL = {"pumpfun": 0.25, "pumpswap": 1.00}
RE_VENUE = re.compile(r"venue=(\S+)")
RE_CURVE = re.compile(
    r"curve_reserves=(\w+)(?: reason=(\S+))?.*?v_sol_reserves_sol=([\d.]+) "
    r"v_tokens_reserves=(\d+) real_sol_reserves_sol=([\d.]+) real_tokens_reserves=(\d+) ")
RE_CURVE_ABSENT = re.compile(r"CURVE STATE \(at decision time\): curve_reserves=absent reason=(\S+)")
RE_AMM_ABSENT = re.compile(r"AMM POOL STATE \(at decision time\): amm_reserves=absent reason=(\S+)")
RE_AMM = re.compile(
    r"AMM POOL STATE \(at decision time\): amm_reserves=present src=\S+ pool=(\S+) quote=(\S+) "
    r"staleness_ms=(\d+) pricing_eligible=(\w+) base_reserves_raw=(\d+) "
    r"quote_reserves_lamports=(\d+) quote_reserves_sol=([\d.]+)")
RE_ACTION = re.compile(r"\b(BUY|WATCH|SKIP)\b")


def parse_row(r):
    try:
        u = r["messages"][1]["content"]
        a = r["messages"][-1]["content"]
    except Exception:
        return None
    out = {"venue": (RE_VENUE.search(u).group(1) if RE_VENUE.search(u) else "unknown"),
           "curve": None, "amm": None, "label": None}
    mc = RE_CURVE.search(u)
    if mc:
        out["curve"] = {"present": mc.group(1) == "present", "v_sol": float(mc.group(3)),
                        "real_sol": float(mc.group(5))}
    else:
        m = RE_CURVE_ABSENT.search(u)
        if m:
            out["curve_absent"] = m.group(1)
    mm = RE_AMM.search(u)
    if mm:
        out["amm"] = {"quote_sol": float(mm.group(7)), "eligible": mm.group(4) == "true"}
    else:
        m = RE_AMM_ABSENT.search(u)
        if m:
            out["amm_absent"] = m.group(1)
    head = a.strip().split("\n")[0]
    act = RE_ACTION.search(head) or RE_ACTION.search(a)
    out["label"] = act.group(1) if act else None
    out["regime_venue"] = "pumpswap" if out["amm"] else "pumpfun"
    out["clip_sol"] = RULED_CLIP_SOL[out["regime_venue"]]
    bt = (r.get("meta") or {}).get("barrier_triplet") or {}
    out["bt"] = bt
    return out


def refused(r, clip_sol=None):
    """Which rule refuses this row at `clip_sol` (default: the ruled clip)."""
    clip = r["clip_sol"] if clip_sol is None else clip_sol
    am, c = r["amm"], r["curve"]
    v = {}
    if am:
        depth = am["quote_sol"]
        v["impact_bp"] = (clip / depth * 1e4) if depth else None
        v["impact_veto"] = bool(depth and v["impact_bp"] > OWN_IMPACT_VETO_BP)
        v["capacity_refuse"] = False
        v["amm_stale"] = not am["eligible"]
        v["depth_sol"] = depth
    elif c and c["present"]:
        depth = c["v_sol"]
        v["impact_bp"] = (clip / depth * 1e4) if depth else None
        v["impact_veto"] = bool(depth and v["impact_bp"] > OWN_IMPACT_VETO_BP)
        v["capacity_refuse"] = c["real_sol"] < clip
        v["depth_sol"] = depth
    else:
        v["no_reserves"] = True
    return v


def net_bp(r):
    """Realized net-of-cost return of the barrier the row was LABELED from.

    tp -> +tp_bp, sl -> -sl_bp, timeout/censored -> the mark at the horizon.
    Cost is the venue-resolved floor the corpus itself pins (amm 60 / curve 189 bp).
    Returns None when the row has no usable triplet (an unlabeled row).
    """
    bt = r.get("bt") or {}
    if bt.get("status") != "labeled":
        return None
    fl = (bt.get("cost_floor_bps") or {}).get(
        "amm" if r["regime_venue"] == "pumpswap" else "bonding_curve")
    if fl is None:
        return None
    oc = bt.get("outcome")
    if oc == "tp":
        g = float(bt.get("tp_bp") or 0.0)
    elif oc == "sl":
        g = -float(bt.get("sl_bp") or 0.0)
    else:                                   # timeout / censored: the horizon mark
        g = bt.get("last_bp")
        if g is None:
            return None
        g = float(g)
    return g - float(fl)


def mean(a):
    return sum(a) / len(a) if a else None


def med(a):
    import statistics
    return statistics.median(a) if a else None


def pct(a, q):
    import numpy as np
    return float(np.percentile(a, q)) if a else None


def bucket_report(name, rows):
    """Net-of-cost outcome of one group of rows."""
    nets = [n for n in (net_bp(r) for r in rows) if n is not None]
    oc = collections.Counter((r.get("bt") or {}).get("outcome") for r in rows)
    if not nets:
        print("   %-28s n=%5d | no usable outcome" % (name, len(rows)))
        return {"n": len(rows), "n_net": 0}
    wins = [n for n in nets if n > 0]
    return {"n": len(rows), "n_net": len(nets),
            "net_mean_bp": mean(nets), "net_med_bp": med(nets),
            "net_p90_bp": pct(nets, 90), "win_rate": len(wins) / len(nets),
            "tp": oc.get("tp", 0), "sl": oc.get("sl", 0),
            "timeout": oc.get("timeout", 0), "censored": oc.get("censored", 0)}


def show(name, rows):
    s = bucket_report(name, rows)
    if s.get("n_net"):
        print("   %-28s n=%5d | net mean %8.1f med %8.1f p90 %9.1f bp | win %5.1f%% "
              "| tp %4d sl %4d timeout %5d censored %4d"
              % (name, s["n"], s["net_mean_bp"], s["net_med_bp"], s["net_p90_bp"],
                 100 * s["win_rate"], s["tp"], s["sl"], s["timeout"], s["censored"]))
    return s


def impact_round_trip_bp(r, clip_sol):
    """Our OWN price impact, entry + exit, at `clip_sol` against the row's depth.

    The barrier triplet is the MARKET's path: it is what the tape did, not what it
    does when we take 0.25/1.00 SOL out of a thin pool. Charging this is the
    difference between 'the veto loses edge' and 'the veto prices edge the triplet
    cannot see' - so every recovered-edge claim below is reported WITH this charge.
    """
    d = None
    if r["amm"]:
        d = r["amm"]["quote_sol"]
    elif r["curve"] and r["curve"]["present"]:
        d = r["curve"]["v_sol"]
    if not d:
        return None
    return 2.0 * (clip_sol / d) * 1e4


def curve_tradeable(r):
    """Is the CURVE a usable fallback venue for this row at the SMALL clip?"""
    c = r["curve"]
    if not (c and c["present"]):
        return None
    clip = RULED_CLIP_SOL["pumpfun"]
    if c["real_sol"] < clip:                       # payout-reserve capacity bound
        return None
    d = c["v_sol"]
    if not d:
        return None
    imp = (clip / d) * 1e4
    return {"impact_bp": imp, "capacity_ok": True, "veto": imp > OWN_IMPACT_VETO_BP}


def deep_report(venue, rows):
    """The two recovery hypotheses, each charged its own impact."""
    print("\n== %s: DEEP (own-impact charged, entry+exit) ==" % venue)
    buys = [r for r in rows if r["label"] == "BUY"]
    for rule in ("impact_veto", "amm_stale"):
        sel = [(r, refused(r)) for r in buys]
        grp = [r for r, x in sel if x.get(rule) and not x.get("no_reserves")]
        if not grp:
            print("   %s: none" % rule)
            continue
        print("   -- refused by %s (n=%d) --" % (rule, len(grp)))
        for clip_name, clip in (("ruled clip", None), (".25 clip", 0.25)):
            rows_charged = []
            for r in grp:
                nb = net_bp(r)
                imp = impact_round_trip_bp(r, r["clip_sol"] if clip is None else clip)
                if nb is None or imp is None:
                    continue
                rows_charged.append((nb, nb - imp, imp,
                                     (r.get("bt") or {}).get("outcome")))
            if not rows_charged:
                continue
            gross = [x[0] for x in rows_charged]
            netc = [x[1] for x in rows_charged]
            imps = [x[2] for x in rows_charged]
            print("      @%s: n=%4d  gross mean %7.1f | impact mean %6.1f bp "
                  "| NET-after-impact mean %7.1f med %7.1f  win %5.1f%%"
                  % (clip_name, len(netc), mean(gross), mean(imps), mean(netc),
                     med(netc), 100 * len([x for x in netc if x > 0]) / len(netc)))
            if clip_name == "ruled clip":
                # CENSORED-excluded: a censored row's horizon mark is an artifact of
                # where the capture stopped, so the claim must survive without it.
                kn = [x[1] for x in rows_charged if x[3] != "censored"]
                if kn:
                    print("          censored EXCLUDED: n=%4d  NET-after-impact mean "
                          "%7.1f med %7.1f  win %5.1f%%"
                          % (len(kn), mean(kn), med(kn),
                             100 * len([x for x in kn if x > 0]) / len(kn)))
    # amm_stale: is the CURVE a legitimate fallback for those rows?
    stale = [r for r in buys if refused(r).get("amm_stale")]
    if stale:
        ok = [r for r in stale if (curve_tradeable(r) or {}).get("capacity_ok")
              and not (curve_tradeable(r) or {}).get("veto")]
        print("   -- amm_stale rows recoverable ON THE CURVE at .25 (capacity + impact "
              "both OK): %d/%d" % (len(ok), len(stale)))
        if ok:
            nets, imp_adj = [], []
            for r in ok:
                nb, imp = net_bp(r), impact_round_trip_bp(r, 0.25)
                if nb is not None and imp is not None:
                    nets.append(nb)
                    imp_adj.append(nb - imp)
            if nets:
                print("      realized: gross mean %7.1f med %7.1f | NET-after-impact "
                      "mean %7.1f med %7.1f | win-after %5.1f%%"
                      % (mean(nets), med(nets), mean(imp_adj), med(imp_adj),
                         100 * len([x for x in imp_adj if x > 0]) / len(imp_adj)))
    # censored contamination, stated rather than hidden
    cen = [r for r in buys if (r.get("bt") or {}).get("outcome") == "censored"]
    print("   censored BUY rows (folded in above as the horizon mark): %d/%d"
          % (len(cen), len(buys)))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=0, help="0 = whole corpus")
    a = ap.parse_args()
    rows = []
    with open(CORPUS, encoding="utf-8") as fh:
        for i, line in enumerate(fh):
            if a.n and i >= a.n:
                break
            try:
                r = json.loads(line)
            except Exception:
                continue
            p = parse_row(r)
            if p and p["label"]:
                rows.append(p)
    print("rows with a label: %d" % len(rows))
    print("labels: %s" % dict(collections.Counter(r["label"] for r in rows)))

    for venue in ("mixed", "pumpfun", "pumpswap"):
        sub = [r for r in rows if r["venue"] == venue]
        if not sub:
            continue
        buys = [r for r in sub if r["label"] == "BUY"]
        print("\n== %s: %d rows, %d BUY" % (venue, len(sub), len(buys)))
        if not buys:
            continue
        vs = [(r, refused(r)) for r in buys]
        v = collections.Counter()
        for _, x in vs:
            for k in ("impact_veto", "capacity_refuse", "amm_stale", "no_reserves"):
                if x.get(k):
                    v[k] += 1
        print("   refusals on BUY rows: %s" % dict(v))
        dec = [r for r, x in vs if not any(x.get(k) for k in
                                          ("impact_veto", "capacity_refuse", "amm_stale", "no_reserves"))]
        ref_any = [r for r, x in vs if any(x.get(k) for k in
                                          ("impact_veto", "capacity_refuse", "amm_stale", "no_reserves"))]
        print("   -- realized outcome, net of the venue cost floor --")
        show("admitted", dec)
        show("refused (any rule)", ref_any)
        show("refused: impact veto", [r for r, x in vs if x.get("impact_veto")])
        show("refused: capacity", [r for r, x in vs if x.get("capacity_refuse")])
        show("refused: amm stale", [r for r, x in vs if x.get("amm_stale")])

        # ---- the SIZE counterfactual: does sizing to the curve clip admit them?
        small = [(r, refused(r, clip_sol=RULED_CLIP_SOL["pumpfun"])) for r, _ in vs]
        newly = [r for r, x in small
                 if not any(x.get(k) for k in ("impact_veto", "capacity_refuse",
                                               "amm_stale", "no_reserves"))
                 and any(y.get(k) for k in ("impact_veto", "capacity_refuse")
                         for y in [refused(r)])]
        print("   -- counterfactual: same decisions sized at the CURVE clip 0.25 SOL --")
        show("newly admissible @0.25", newly)
        if newly:
            d = [(x.get("depth_sol") or 0) for r, x in small
                 for _ in [0] if r in newly and x.get("depth_sol")]
            if d:
                print("      their depth SOL: p10=%.2f p50=%.2f p90=%.2f"
                      % (pct(d, 10), pct(d, 50), pct(d, 90)))
        # what the 1.00 SOL clip demanded of those pools
        depths_ref = [x.get("depth_sol") for r, x in vs if x.get("depth_sol")]
        if depths_ref:
            print("      pool/curve depth SOL, BUY rows: p10=%.2f p50=%.2f p90=%.2f"
                  % (pct(depths_ref, 10), pct(depths_ref, 50), pct(depths_ref, 90)))
    for venue in ("mixed", "pumpfun", "pumpswap"):
        sub = [r for r in rows if r["venue"] == venue]
        if sub:
            deep_report(venue, sub)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
