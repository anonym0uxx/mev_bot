#!/usr/bin/env python
"""c12 ENTRY RELABEL: outcome-conditioned action labels + isolated sizing pass.

WHAT THIS DOES (re-runnable, c11 -> c12 staging; c11 is never modified in place)

  1. ACTION RELABEL. For every `decision`-family row, the new action is a function of
     the (stratum, venue) cell's expected net realized return, measured on the c11
     BUY-labelled sub-population of the FIT split (train+validation - the examination
     split is held out so the policy gate below is honest):
        * per-cell EV = IPCW-corrected (cap 20, ipcw.py) mean net barrier return,
          empirical-Bayes shrunk toward the cell's OWN venue grand mean
          (k_shrink = var_within / var_between per venue, exactly the
          relabel_size_kelly.py machinery);
        * BUY   where shrunk EV > 0 and cell BUY support n >= MIN_SUPPORT (50);
        * WATCH where shrunk EV > 0 but n < 50;
        * SKIP  where EV <= 0.
     Rows with no barrier label (barrier_triplet is None / not `labeled`) KEEP their
     c11 action, tagged meta.entry_relabel={status:'no_realized_path', kept:'c11_action'}.
     Realized return per row comes from the row's OWN meta.barrier_triplet
     (barrier_triplet_v1, 1800s): tp +10000bp, sl -5000bp, timeout/censored last_bp,
     net of the triplet's own cost_floor_bps[regime]. Venue = cost_authority.regime
     (venue at decision time - the defensible key; the barrier regime disagrees on
     mints that graduated mid-window).

  2. POLICY GATE (pre-declared; STOP-AND-REPORT on failure, no staging written).
     On the examination split, the relabelled BUY set's realized net mean return must
     have a mint-clustered bootstrap 95% one-sided LB > 0 (resample MINTS, 2000 draws).
     The old-label BUY LB is reported alongside for comparison.

  3. SIZING (ISOLATED, POLICY-SWITCHED). The size label is applied AFTER and
     independently of the action relabel, selected by --sizing-policy so the sizing
     pass can be re-run alone without redoing the relabel or the gate:
        * ev_gated_binary (DEFAULT - operator-approved per KELLY_AUDIT_C12.md):
          vocabulary {NONE, SMALL, FULL}, MID retired. Deterministic from row meta
          alone (train == serve): action != BUY -> NONE; venue amm -> FULL;
          venue bonding_curve -> SMALL. No mu table, no lambda at serve time.
        * kelly_venue (the pre-audit spec, kept runnable): AMM tier =
          argmax_{w in {0,.25,.5,1}} w*mu_shrunk - lambda_amm*w^2 with lambda =
          E[r^2]/(2*1.01^2) derived venue-separated on the NEW BUY fit population;
          curve fixed SMALL satellite (median-negative lottery, never Kelly-sized).
     lambda values are derived and reported under BOTH policies (provenance), but
     under ev_gated_binary they never touch a label.

  4. HEADER/META REWRITE. The assistant target's DECISION:/SIZE: header, meta.action
     and meta.size_dimension are rewritten coherently. PRICE LIMIT survives only on
     BUY rows (BUY-only field per the family contract). The body below the header is
     NOT touched, and messages[0]/messages[1] (prompt) are NEVER touched - enforced
     by hash. All non-`decision` families are copied through byte-identical
     (utility_regression's targets trace to barrier_labels_c9_300s / v3-300s, NOT the
     1800s v1 triplet this relabel conditions on, so it is copy-through, not rebuilt).

Modes: --dry-run (measure everything, write report, no staging), --verify-only
(check an already-written staging corpus and the byte-identity of pass-through rows).
"""
import argparse
import collections
import hashlib
import json
import math
import os
import random
import re
import statistics as st
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import ipcw  # noqa: E402

SRC = "/training/v2/candidate_sft_c11"
OUTDIR_DEFAULT = "/training/v2/candidate_sft_c12_entry"
REPORT = "/training/v2/reports/ENTRY_RELABEL_C12.json"
REPORT_VERIFY = "/training/v2/reports/ENTRY_RELABEL_C12_VERIFY.json"
SPLITS = ("train", "validation", "examination")
FIT_SPLITS = ("train", "validation")          # exam held out for the gate
ENTRY = ("BUY", "WATCH", "SKIP")
MIN_SUPPORT = 50
IPCW_CAP = 20.0
CAP_SWEEP = (1.0, 5.0, 20.0, 100.0)
ACC = 1.01
KELLY_TIERS = (0.0, 0.25, 0.5, 1.0)
BOOT_DRAWS = 2000
BOOT_SEED = 20260919
RE_HEADLINE = re.compile(r"^(DECISION|SIZE|PRICE LIMIT):[^\n]*\n?")

# --------------------------------------------------------------------------- strata
def tercile(x, cuts):
    if x is None:
        return "na"
    for i, c in enumerate(cuts):
        if x <= c:
            return "t%d" % (i + 1)
    return "t%d" % (len(cuts) + 1)


def holders_bucket(h):
    if h is None:
        return "na"
    return "thin" if h < 50 else ("mid" if h < 200 else "thick")


def creator_bucket(c):
    if c is None:
        return "unknown"
    return "fresh" if c <= 0 else ("few" if c <= 2 else "serial")


def stratum_of(enr, cuts):
    return "|".join((tercile(enr.get("top1_float_share"), cuts),
                     holders_bucket(enr.get("holders_at_t")),
                     creator_bucket(enr.get("creator_past_launches"))))


# ------------------------------------------------------------------- realized return
def net_r_t(bt):
    """(net_r, t_end_ms, censored_flag) from an embedded barrier_triplet_v1, or None."""
    if not bt or bt.get("status") != "labeled":
        return None
    o = bt.get("outcome")
    if o == "tp":
        g = 10_000.0
    elif o == "sl":
        g = -5_000.0
    elif o in ("timeout", "censored"):
        g = float(bt["last_bp"])
    else:
        return None
    floor = (bt.get("cost_floor_bps") or {}).get(bt.get("regime") or "amm", 60)
    if o in ("tp", "sl"):
        t = bt.get("t_to_hit_ms") or bt.get("observed_ms", 0)
    else:
        t = bt.get("observed_ms", 0)
    return (g - floor) / 1e4, int(t or 0), o in ("timeout", "censored")


# --------------------------------------------------------------------------- loading
def load_decision_rows(splits=SPLITS):
    """Minimal per-row tuples for the fit/assign passes (full rows re-read at emit)."""
    rows = []
    for sp in splits:
        with open(os.path.join(SRC, "%s.jsonl" % sp), encoding="utf-8") as fh:
            for i, ln in enumerate(fh):
                r = json.loads(ln)
                m = r["meta"]
                if m.get("family") != "decision":
                    continue
                e = m.get("c9_enrichment") or {}
                rows.append({
                    "split": sp, "line": i,
                    "venue": (m.get("cost_authority") or {}).get("regime") or "amm",
                    "enr": {"top1_float_share": e.get("top1_float_share"),
                            "holders_at_t": e.get("holders_at_t"),
                            "creator_past_launches": e.get("creator_past_launches")},
                    "old_action": m.get("action"),
                    "rtc": net_r_t(m.get("barrier_triplet")),
                    "mint": m.get("mint"),
                })
    return rows


# ------------------------------------------------------------------ fit: stratum EVs
def fit_table(rows, cap, fit_splits=FIT_SPLITS):
    """(stratum, venue) IPCW+EB-shrunk EV table off the fit population.

    EV population = ALL labeled fit rows in the cell (the counterfactual value of
    buying every row of the cell - the quantity the relabel of every row needs;
    BUY-only EV would inherit the c11 grid's selection bias and mark nearly every
    cell positive). SUPPORT population = the cell's c11 BUY rows, per the rule's
    explicit 'stratum BUY-population support n >= 50'.
    """
    fit = [d for d in rows if d["split"] in fit_splits]
    t1 = sorted(d["enr"]["top1_float_share"] for d in fit
                if d["enr"]["top1_float_share"] is not None)
    cuts = [t1[int(0.33 * len(t1))], t1[int(0.66 * len(t1))]] if t1 else [0.0, 0.0]

    cells = collections.defaultdict(lambda: {"r": [], "t": [], "c": []})
    venue_pool = collections.defaultdict(lambda: {"r": [], "t": [], "c": []})
    buy_support = collections.Counter()
    for d in fit:
        if d["rtc"] is None:
            continue
        r, t, c = d["rtc"]
        key = (stratum_of(d["enr"], cuts), d["venue"])
        cell = cells[key]
        cell["r"].append(r); cell["t"].append(t); cell["c"].append(c)
        vp = venue_pool[d["venue"]]
        vp["r"].append(r); vp["t"].append(t); vp["c"].append(c)
        if d["old_action"] == "BUY":
            buy_support[key] += 1

    table, venue_stats = {}, {}
    for v, vp in venue_pool.items():
        grand = ipcw.ev_ipcw(vp["r"], vp["t"], vp["c"], max_weight=cap)[1] or 0.0
        mus = {s: (ipcw.ev_ipcw(e["r"], e["t"], e["c"], max_weight=cap)[1] or 0.0)
               for (s, vv), e in cells.items() if vv == v}
        var_within = st.pvariance(vp["r"]) if len(vp["r"]) > 1 else 0.0
        var_between = st.pvariance(list(mus.values())) if len(mus) > 1 else 0.0
        k = (var_within / var_between) if var_between > 1e-12 else 0.0
        venue_stats[v] = {"grand_mu_ipcw": round(grand, 6), "n_buy": len(vp["r"]),
                          "var_within": round(var_within, 6),
                          "var_between": round(var_between, 6),
                          "k_shrink": round(k, 4), "n_cells": len(mus)}
        for s, mu in mus.items():
            n = len(cells[(s, v)]["r"])
            w = n / (n + k) if (n + k) > 0 else 0.0
            table[(s, v)] = {"n_labeled": n, "n_buy_support": buy_support[(s, v)],
                             "mu_ipcw": round(mu, 6),
                             "mu_shrunk": round(w * mu + (1.0 - w) * grand, 6),
                             "shrink_weight_own": round(w, 4)}
    return {"cuts": cuts, "table": table, "venue": venue_stats, "cap": cap}


def assign_action(d, ft):
    """(new_action, tag) for one decision row under the fitted table."""
    if d["rtc"] is None and d["old_action"] in ENTRY:
        # no realized path -> keep the c11 action, say so
        return d["old_action"], {"status": "no_realized_path", "kept": "c11_action"}
    s = stratum_of(d["enr"], ft["cuts"])
    cell = ft["table"].get((s, d["venue"]))
    if cell is None:
        ev = (ft["venue"].get(d["venue"]) or {}).get("grand_mu_ipcw", 0.0) or 0.0
        n = 0
    else:
        ev, n = cell["mu_shrunk"], cell["n_buy_support"]
    if ev > 0 and n >= MIN_SUPPORT:
        na = "BUY"
    elif ev > 0:
        na = "WATCH"
    else:
        na = "SKIP"
    return na, {"status": "relabeled", "stratum": s, "venue": d["venue"],
                "ev_shrunk": round(ev, 6), "n_support": n}


# ----------------------------------------------------------------------------- gate
def mint_boot_lb(rows_r_by_mint, draws=BOOT_DRAWS, seed=BOOT_SEED):
    """One-sided 95% lower bound on the mean via mint-clustered bootstrap."""
    mints = sorted(rows_r_by_mint)
    if not mints:
        return None, None, 0, 0
    all_r = [x for m in mints for x in rows_r_by_mint[m]]
    rng = random.Random(seed)
    means = []
    for _ in range(draws):
        acc_s, acc_n = 0.0, 0
        for m in rng.choices(mints, k=len(mints)):
            rs = rows_r_by_mint[m]
            acc_s += sum(rs); acc_n += len(rs)
        if acc_n:
            means.append(acc_s / acc_n)
    means.sort()
    lb = means[int(0.05 * len(means))]
    return st.fmean(all_r), lb, len(all_r), len(mints)


# --------------------------------------------------------------------------- sizing
def derive_lambda(rows, actions):
    """lambda = E[r^2]/(2*ACC^2), venue-separated, NEW-BUY fit population."""
    r2 = collections.defaultdict(list)
    for d in rows:
        if d["split"] not in FIT_SPLITS or d["rtc"] is None:
            continue
        if actions[(d["split"], d["line"])][0] != "BUY":
            continue
        r2[d["venue"]].append(d["rtc"][0])
    out = {}
    for v, rs in r2.items():
        e2 = math.fsum(x * x for x in rs) / len(rs)
        out[v] = {"n": len(rs), "E_r2": round(e2, 6),
                  "lambda": round(e2 / (2.0 * ACC * ACC), 6),
                  "mean_r": round(st.fmean(rs), 6)}
    return out


def size_for(action, venue, policy, ft=None, lam=None, strat=None):
    """(size_str, tier_fraction, extras) under the selected sizing policy."""
    if action != "BUY":
        return "NONE", None, {}
    if policy == "ev_gated_binary":
        return (("FULL", 1.0, {}) if venue == "amm" else ("SMALL", 0.25, {}))
    # kelly_venue
    if venue != "amm":
        return "SMALL", 0.25, {"rule": "fixed SMALL satellite (curve, never Kelly-sized)"}
    cell = ft["table"].get((strat, "amm"))
    mu = cell["mu_shrunk"] if cell else (ft["venue"].get("amm") or {}).get("grand_mu_ipcw", 0.0)
    lam_v = (lam.get("amm") or {}).get("lambda", 0.0)
    best_w, best_v = 0.0, 0.0
    for w in KELLY_TIERS:
        val = w * mu - lam_v * w * w
        if val > best_v + 1e-12:
            best_w, best_v = w, val
    name = {0.0: "NONE", 0.25: "SMALL", 0.5: "MID", 1.0: "FULL"}[best_w]
    if name == "NONE":          # Kelly says no position but the action label is BUY:
        name, best_w = "SMALL", 0.25   # floor at SMALL rather than emit BUY/NONE
    return name, best_w, {"stratum_mu": mu, "lambda_exposure": lam_v,
                          "rule": "argmax_w w*mu - lambda_amm*w^2"}


# --------------------------------------------------------------------- row transform
def transform(r, new_action, tag, policy, ft, lam, cuts, stats):
    m = r["meta"]
    d_venue = (m.get("cost_authority") or {}).get("regime") or "amm"
    strat = stratum_of(m.get("c9_enrichment") or {}, cuts)
    pre_hash = hashlib.sha256(
        (r["messages"][0]["content"] + "\x00" + r["messages"][1]["content"])
        .encode("utf-8")).hexdigest()

    size, tier, extras = size_for(new_action, d_venue, policy, ft, lam, strat)

    a = r["messages"][2]["content"].lstrip()
    price_limit = None
    while True:
        mm = RE_HEADLINE.match(a)
        if not mm:
            break
        if mm.group(1) == "PRICE LIMIT":
            price_limit = mm.group(0).split(":", 1)[1].strip()
        a = a[mm.end():]
    if not re.match(r"^(INVALIDATION|EVIDENCE|COUNTEREVIDENCE|ENRICHMENT|SIZE_BASIS)", a):
        stats["body_unrecognised"] += 1
    head = "DECISION: %s\nSIZE: %s\n" % (new_action, size)
    if new_action == "BUY" and price_limit:
        head += "PRICE LIMIT: %s\n" % price_limit
    r["messages"][2]["content"] = head + a

    old_action = m.get("action")
    m["action"] = new_action
    tag = dict(tag)
    tag.update({"schema": "entry_relabel_c12_v1", "old_action": old_action,
                "new_action": new_action, "sizing_policy": policy})
    m["entry_relabel"] = tag
    sd = {"status": "labeled" if new_action == "BUY" else "not_buy",
          "decision": new_action, "size": size if new_action == "BUY" else None,
          "sizing_policy": ("ev_gated_binary_v1" if policy == "ev_gated_binary"
                            else "kelly_venue_c12"),
          "venue": d_venue, "stratum": strat, "relabel": "c12"}
    if new_action == "BUY":
        sd["tier_fraction"] = tier
        sd.update(extras)
    m["size_dimension"] = sd

    post_hash = hashlib.sha256(
        (r["messages"][0]["content"] + "\x00" + r["messages"][1]["content"])
        .encode("utf-8")).hexdigest()
    if pre_hash != post_hash:
        raise RuntimeError("prompt text mutated - refusing")
    stats["pair_%s_to_%s" % (old_action, new_action)] += 1
    return r


# ---------------------------------------------------------------------------- verify
def verify_row(r, policy):
    bad = []
    m = r["meta"]
    a = r["messages"][2]["content"]
    action = m.get("action")
    dec = (re.match(r"DECISION:\s*(\w+)", a) or [None, None])[1]
    if dec != action:
        bad.append("decision_disagrees_with_meta")
    sz = (re.search(r"^SIZE:\s*(\S+)", a, re.M) or [None, None])[1]
    sd = m.get("size_dimension") or {}
    if sz is None:
        bad.append("no_size_line")
    elif action == "BUY":
        if sz == "NONE":
            bad.append("buy_with_size_none")
        if policy == "ev_gated_binary":
            want = "FULL" if sd.get("venue") == "amm" else "SMALL"
            if sz != want:
                bad.append("binary_policy_size_mismatch")
        if sz != (sd.get("size") or "NONE"):
            bad.append("size_dimension_disagrees_with_header")
    else:
        if sz != "NONE":
            bad.append("non_buy_with_size")
        if "PRICE LIMIT:" in a.split("INVALIDATION:", 1)[0]:
            bad.append("non_buy_with_price_limit")
    if policy == "ev_gated_binary" and sz == "MID":
        bad.append("retired_mid_tier")
    if sd.get("decision") != action:
        bad.append("size_dimension_decision_disagrees")
    if not isinstance(m.get("entry_relabel"), dict):
        bad.append("missing_entry_relabel_tag")
    i_dec, i_size = a.find("DECISION:"), a.find("SIZE:")
    if i_dec != 0 or i_size < i_dec:
        bad.append("header_order")
    return bad


def verify_staging(outdir, policy):
    report = {"suites": {}, "corpus": "c12_entry_relabel_verify", "policy": policy}
    total = 0
    for sp in SPLITS:
        st_c = collections.Counter()
        defects = collections.Counter()
        src_fh = open(os.path.join(SRC, "%s.jsonl" % sp), encoding="utf-8")
        with open(os.path.join(outdir, "%s.jsonl" % sp), encoding="utf-8") as fh:
            for ln, src_ln in zip(fh, src_fh):
                r = json.loads(ln)
                m = r["meta"]
                if m.get("family") != "decision":
                    st_c["passthrough"] += 1
                    if ln != src_ln:
                        defects["passthrough_not_byte_identical"] += 1
                    continue
                st_c["decision"] += 1
                src_r = json.loads(src_ln)
                for k in (0, 1):
                    if r["messages"][k]["content"] != src_r["messages"][k]["content"]:
                        defects["prompt_text_changed"] += 1
                for b in verify_row(r, policy):
                    defects[b] += 1
        src_fh.close()
        total += sum(defects.values())
        report["suites"][sp] = {"stats": dict(st_c), "defects": dict(defects)}
        print(sp, dict(st_c), "defects", dict(defects), flush=True)
    report["total_defects"] = total
    json.dump(report, open(REPORT_VERIFY, "w"), indent=1)
    print("VERIFY total defects", total, "->", REPORT_VERIFY)
    return 1 if total else 0


# ------------------------------------------------------------------------------ main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--outdir", default=OUTDIR_DEFAULT)
    ap.add_argument("--sizing-policy", choices=("ev_gated_binary", "kelly_venue"),
                    default="ev_gated_binary")
    ap.add_argument("--ipcw-cap", type=float, default=IPCW_CAP)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--verify-only", action="store_true")
    ap.add_argument("--keep-actions", action="store_true",
                    help="DECISION_FAMILY_C12 ruling: pass c11 actions through "
                         "unchanged (relabel gate failed); apply sizing policy only")
    args = ap.parse_args()

    if args.verify_only:
        return verify_staging(args.outdir, args.sizing_policy)

    rows = load_decision_rows()
    ft = fit_table(rows, args.ipcw_cap)
    print("fit: cuts=%s venues=%s" % (ft["cuts"], json.dumps(ft["venue"])), flush=True)

    actions = {}
    trans = collections.Counter()
    dist = collections.Counter()
    venue_dist = collections.Counter()
    for d in rows:
        if args.keep_actions:
            na, tag = d["old_action"], {"status": "kept_c11_action",
                                        "reason": "relabel gate failed; Option A ruling"}
        else:
            na, tag = assign_action(d, ft)
        actions[(d["split"], d["line"])] = (na, tag)
        trans[(d["old_action"], na)] += 1
        dist[na] += 1
        venue_dist[(d["venue"], na)] += 1

    # ---- policy gate on the examination split ---------------------------------
    def exam_buy_returns(select_new):
        by_mint = collections.defaultdict(list)
        for d in rows:
            if d["split"] != "examination" or d["rtc"] is None:
                continue
            act = actions[(d["split"], d["line"])][0] if select_new else d["old_action"]
            if act == "BUY":
                by_mint[d["mint"]].append(d["rtc"][0])
        return by_mint

    mean_new, lb_new, n_new, mints_new = mint_boot_lb(exam_buy_returns(True))
    mean_old, lb_old, n_old, mints_old = mint_boot_lb(exam_buy_returns(False))
    gate_passed = lb_new is not None and lb_new > 0
    gate = {"rule": "exam new-BUY realized net mean: mint-clustered bootstrap 95% LB > 0",
            "draws": BOOT_DRAWS, "seed": BOOT_SEED,
            "new_buy": {"mean_r": round(mean_new, 6) if mean_new is not None else None,
                        "lb95": round(lb_new, 6) if lb_new is not None else None,
                        "n_rows": n_new, "n_mints": mints_new},
            "old_buy": {"mean_r": round(mean_old, 6) if mean_old is not None else None,
                        "lb95": round(lb_old, 6) if lb_old is not None else None,
                        "n_rows": n_old, "n_mints": mints_old},
            "passed": gate_passed}
    print("GATE", json.dumps(gate), flush=True)

    # ---- lambda (venue-separated, NEW-BUY fit population; provenance always) ---
    lam = derive_lambda(rows, actions)
    print("lambda(new-BUY, fit)", json.dumps(lam), flush=True)

    # ---- IPCW cap sensitivity: refit + reassign + re-gate at each cap ----------
    sens = {}
    for cap in CAP_SWEEP:
        ftc = fit_table(rows, cap) if cap != args.ipcw_cap else ft
        dc = collections.Counter()
        by_mint = collections.defaultdict(list)
        for d in rows:
            na, _ = assign_action(d, ftc)
            dc[na] += 1
            if d["split"] == "examination" and d["rtc"] is not None and na == "BUY":
                by_mint[d["mint"]].append(d["rtc"][0])
        mn, lb, nn, nm = mint_boot_lb(by_mint)
        sens["%g" % cap] = {"action_distribution": dict(dc),
                            "exam_buy_mean_r": round(mn, 6) if mn is not None else None,
                            "exam_buy_lb95": round(lb, 6) if lb is not None else None,
                            "exam_buy_rows": nn, "exam_buy_mints": nm}
    print("cap sensitivity", json.dumps(sens), flush=True)

    # ---- diagnostic ONLY: the size-kelly precedent fits strata on ALL splits.
    # Doing that here makes the exam gate circular (the table sees exam outcomes),
    # so it can never satisfy the gate - but it is recorded so the comparison is
    # explicit rather than silent.
    ft_leaky = fit_table(rows, args.ipcw_cap, fit_splits=SPLITS)
    by_mint_leak = collections.defaultdict(list)
    for d in rows:
        if d["split"] == "examination" and d["rtc"] is not None:
            if assign_action(d, ft_leaky)[0] == "BUY":
                by_mint_leak[d["mint"]].append(d["rtc"][0])
    mn_l, lb_l, nn_l, nm_l = mint_boot_lb(by_mint_leak)
    leaky = {"note": ("fit pooled over ALL splits incl. exam (relabel_size_kelly "
                      "precedent) - NOT a valid gate, exam outcomes leak into the "
                      "table; diagnostic only"),
             "exam_buy_mean_r": round(mn_l, 6) if mn_l is not None else None,
             "exam_buy_lb95": round(lb_l, 6) if lb_l is not None else None,
             "exam_buy_rows": nn_l, "exam_buy_mints": nm_l}
    print("leaky-fit diagnostic", json.dumps(leaky), flush=True)

    # ---- EV-population interpretation sensitivity: the per-cell EV above pools
    # ALL labeled fit rows (counterfactual value of buying the cell). The
    # alternative reading fits on c11-BUY rows only (inherits the c11 grid's
    # selection). Run both so the gate verdict does not hinge on the reading.
    def fit_table_buy_only(cap):
        fitr = [d for d in rows if d["split"] in FIT_SPLITS
                and d["old_action"] == "BUY" and d["rtc"] is not None]
        cells = collections.defaultdict(lambda: {"r": [], "t": [], "c": []})
        vp = collections.defaultdict(lambda: {"r": [], "t": [], "c": []})
        for d in fitr:
            r, t, c = d["rtc"]
            key = (stratum_of(d["enr"], ft["cuts"]), d["venue"])
            cells[key]["r"].append(r); cells[key]["t"].append(t); cells[key]["c"].append(c)
            vp[d["venue"]]["r"].append(r); vp[d["venue"]]["t"].append(t)
            vp[d["venue"]]["c"].append(c)
        tab, ven = {}, {}
        for v, p in vp.items():
            grand = ipcw.ev_ipcw(p["r"], p["t"], p["c"], max_weight=cap)[1] or 0.0
            mus = {s: (ipcw.ev_ipcw(e["r"], e["t"], e["c"], max_weight=cap)[1] or 0.0)
                   for (s, vv), e in cells.items() if vv == v}
            vw = st.pvariance(p["r"]) if len(p["r"]) > 1 else 0.0
            vb = st.pvariance(list(mus.values())) if len(mus) > 1 else 0.0
            k = (vw / vb) if vb > 1e-12 else 0.0
            ven[v] = {"grand_mu_ipcw": grand}
            for s, mu in mus.items():
                n = len(cells[(s, v)]["r"])
                w = n / (n + k) if (n + k) > 0 else 0.0
                tab[(s, v)] = {"n_labeled": n, "n_buy_support": n,
                               "mu_shrunk": w * mu + (1.0 - w) * grand}
        return {"cuts": ft["cuts"], "table": tab, "venue": ven, "cap": cap}

    ft_b = fit_table_buy_only(args.ipcw_cap)
    by_b = collections.defaultdict(list)
    dist_b = collections.Counter()
    for d in rows:
        na_b, _ = assign_action(d, ft_b)
        dist_b[na_b] += 1
        if d["split"] == "examination" and d["rtc"] is not None and na_b == "BUY":
            by_b[d["mint"]].append(d["rtc"][0])
    mn_b, lb_b, nn_b, nm_b = mint_boot_lb(by_b)
    ev_pop_sens = {"buy_only_ev_pool": {
        "action_distribution": dict(dist_b),
        "exam_buy_mean_r": round(mn_b, 6) if mn_b is not None else None,
        "exam_buy_lb95": round(lb_b, 6) if lb_b is not None else None,
        "exam_buy_rows": nn_b, "exam_buy_mints": nm_b}}
    print("EV-population sensitivity", json.dumps(ev_pop_sens), flush=True)

    report = {
        "schema": "entry_relabel_c12_v1", "src": SRC, "outdir": args.outdir,
        "mode": "keep_actions" if args.keep_actions else "ev_relabel",
        "sizing_policy": args.sizing_policy, "ipcw_cap": args.ipcw_cap,
        "min_support": MIN_SUPPORT, "fit_splits": list(FIT_SPLITS),
        "fit": {"cuts": ft["cuts"], "venue": ft["venue"],
                "cells": {"%s|%s" % k: v for k, v in sorted(ft["table"].items())}},
        "action_transition_matrix": {"%s->%s" % k: v for k, v in sorted(trans.items())},
        "new_action_distribution": dict(dist),
        "per_venue_action_distribution": {"%s|%s" % k: v
                                          for k, v in sorted(venue_dist.items())},
        "gate": gate, "lambda_new_buy_fit": lam,
        "leaky_all_splits_fit_diagnostic": leaky,
        "ev_population_sensitivity": ev_pop_sens,
        "lambda_note": ("labels use sizing_policy=%s; under ev_gated_binary lambda is "
                        "recorded for provenance only (KELLY_AUDIT_C12 verdict), no mu "
                        "table or lambda at serve time" % args.sizing_policy),
        "ipcw_cap_sensitivity": sens,
    }

    if not gate_passed:
        report["stopped"] = "policy gate failed - staging NOT written"
        json.dump(report, open(REPORT, "w"), indent=1)
        print("STOP-AND-REPORT: gate failed, no staging written ->", REPORT)
        return 2

    # ---- emit staging (or dry-run) ---------------------------------------------
    stats_all = {}
    size_trans = collections.Counter()
    if not args.dry_run:
        os.makedirs(args.outdir, exist_ok=True)
    for sp in SPLITS:
        stc = collections.Counter()
        fout = (open(os.path.join(args.outdir, "%s.jsonl" % sp), "w", encoding="utf-8")
                if not args.dry_run else None)
        with open(os.path.join(SRC, "%s.jsonl" % sp), encoding="utf-8") as fh:
            for i, ln in enumerate(fh):
                r = json.loads(ln)
                if r["meta"].get("family") != "decision":
                    stc["passthrough"] += 1
                    if fout:
                        fout.write(ln)
                    continue
                old_sd = (r["meta"].get("size_dimension") or {})
                old_size = old_sd.get("size") or "NONE"
                old_venue = (r["meta"].get("cost_authority") or {}).get("regime")
                na, tag = actions[(sp, i)]
                r = transform(r, na, tag, args.sizing_policy, ft, lam, ft["cuts"], stc)
                new_size = (r["meta"]["size_dimension"].get("size") or "NONE")
                if r["meta"].get("entry_relabel", {}).get("old_action") == "BUY":
                    size_trans[(old_venue, old_size, new_size)] += 1
                for b in verify_row(r, args.sizing_policy):
                    stc["defect_" + b] += 1
                stc["decision_out"] += 1
                if fout:
                    fout.write(json.dumps(r, ensure_ascii=False) + "\n")
        if fout:
            fout.close()
        stats_all[sp] = dict(stc)
        print(sp, dict(stc), flush=True)

    report["emit_stats"] = stats_all
    report["size_transition_old_buy_rows"] = {"%s|%s->%s" % k: v
                                              for k, v in sorted(size_trans.items())}
    report["dry_run"] = args.dry_run
    json.dump(report, open(REPORT, "w"), indent=1)
    print("wrote", REPORT)
    return 0


if __name__ == "__main__":
    sys.exit(main())
