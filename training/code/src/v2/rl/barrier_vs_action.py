"""Join the barrier labels back to the corpus and ask the only question that matters
about a label set: does the labelled action separate outcomes?

Reports, per corpus action (BUY / WATCH / SKIP / null) and per split:
  * outcome mix (TP / SL / TIMEOUT / CENSORED / unpriced)
  * the realized per-unit-notional return of that path, NET of the measured floor
  * the mean / median of that return, and the share of rows with a positive return

If BUY does not beat SKIP on realized barrier return, the decision grid is noise and
no amount of relabelling fixes it - that has to be said out loud, not decorated.
"""
import collections
import json
import statistics as st
import sys

sys.path.insert(0, "/training/v2/code/src/v2/rl")
from exit_mechanics import cost_floor_bps, DEPLOY_SOL_CANONICAL  # noqa: E402

BAR = "/training/v2/reports/barrier_labels_c9.jsonl"
CORPUS = "/training/v2/candidate_sft_c9"
SPLITS = {"train": "train.jsonl", "validation": "validation.jsonl",
          "examination": "examination.jsonl"}

FLOOR = {r: int(cost_floor_bps(r, DEPLOY_SOL_CANONICAL, 0, False, "p50"))
         for r in ("amm", "bonding_curve")}

# index the corpus by (split, line)
line_action = {}
line_size = {}
for sp, fn in SPLITS.items():
    with open("%s/%s" % (CORPUS, fn), encoding="utf-8") as fh:
        for i, ln in enumerate(fh):
            try:
                m = json.loads(ln)["meta"]
            except Exception:                                    # noqa: BLE001
                continue
            line_action[(sp, i)] = m.get("action")
            sd = m.get("size_dimension")
            line_size[(sp, i)] = (sd.get("size") if isinstance(sd, dict) else None)

agg = collections.defaultdict(lambda: {"r": [], "outcome": collections.Counter(),
                                       "mu_pos": 0, "n": 0})
for line in open(BAR, encoding="utf-8"):
    try:
        b = json.loads(line)
    except Exception:                                            # noqa: BLE001
        continue
    sp, li = b["split"], b["line"]
    act = line_action.get((sp, li))
    key = (sp, act)
    o = b.get("outcome") or b.get("status")
    a = agg[key]
    a["n"] += 1
    a["outcome"][o] += 1
    if o == "tp":
        g = 10_000.0
    elif o == "sl":
        g = -5_000.0
    elif o in ("timeout", "censored"):
        g = float(b["last_bp"])
    else:
        continue
    r = (g - FLOOR.get(b.get("regime") or "amm", 0)) / 1e4
    a["r"].append(r)
    if r > 0:
        a["mu_pos"] += 1

report = {}
for (sp, act), a in sorted(agg.items(), key=lambda kv: (str(kv[0][0]), str(kv[0][1]))):
    rs = a["r"]
    d = {"rows": a["n"], "priced": len(rs),
         "outcome": dict(a["outcome"]),
         "tp_rate_of_priced": round(a["outcome"]["tp"] / max(1, len(rs)), 4),
         "sl_rate_of_priced": round(a["outcome"]["sl"] / max(1, len(rs)), 4),
         "mean_r": round(st.fmean(rs), 6) if rs else None,
         "median_r": round(st.median(rs), 6) if rs else None,
         "share_positive_r": round(a["mu_pos"] / max(1, len(rs)), 4)}
    report["%s|%s" % (sp, act)] = d
    print("%-24s %s" % ("%s|%s" % (sp, act), json.dumps(d)))

# pooled over splits, the number the decision rule has to be defended against
pooled = collections.defaultdict(lambda: {"r": [], "tp": 0, "sl": 0, "n": 0})
for line in open(BAR, encoding="utf-8"):
    try:
        b = json.loads(line)
    except Exception:                                            # noqa: BLE001
        continue
    act = line_action.get((b["split"], b["line"]))
    o = b.get("outcome") or b.get("status")
    p = pooled[act]
    p["n"] += 1
    if o == "tp":
        p["tp"] += 1
    elif o == "sl":
        p["sl"] += 1
    if o == "tp":
        g = 10_000.0
    elif o == "sl":
        g = -5_000.0
    elif o in ("timeout", "censored"):
        g = float(b["last_bp"])
    else:
        continue
    p["r"].append((g - FLOOR.get(b.get("regime") or "amm", 0)) / 1e4)

print("\n=== POOLED over splits ===")
pool_report = {}
for act, p in sorted(pooled.items(), key=lambda kv: str(kv[0])):
    rs = p["r"]
    n_res = p["tp"] + p["sl"]
    d = {"rows": p["n"], "priced": len(rs), "tp": p["tp"], "sl": p["sl"],
         "tp_share_of_resolved": round(p["tp"] / max(1, n_res), 4),
         "mean_r": round(st.fmean(rs), 6) if rs else None,
         "median_r": round(st.median(rs), 6) if rs else None,
         "ev_at_full_tier": round(st.fmean(rs), 6) if rs else None}
    pool_report[str(act)] = d
    print("%-10s %s" % (act, json.dumps(d)))

# ---------------------------------------------------------------------------
# M8 — IPCW censoring correction.
#
# TIMEOUT / early-tape-end rows are RIGHT-CENSORED at the horizon: the return recorded for
# them is the mark at censoring, not the process's terminal value, so the plain mean above
# does not sample terminal value. Re-weight each censored row by 1/G(t) with G the
# Kaplan-Meier estimate of the CENSORING survival (ipcw.py). The horizon is NOT changed.
#
# The rows here are EXACTLY the rows mean_r averaged (same outcome filter, same floor), so
# "with and without" are the same sample.
import ipcw  # noqa: E402

ev_rows = collections.defaultdict(lambda: {"r": [], "t": [], "c": []})
for line in open(BAR, encoding="utf-8"):
    try:
        b = json.loads(line)
    except Exception:                                            # noqa: BLE001
        continue
    sp, li = b["split"], b["line"]
    act = line_action.get((sp, li))
    o = b.get("outcome") or b.get("status")
    if o == "tp":
        g = 10_000.0
    elif o == "sl":
        g = -5_000.0
    elif o in ("timeout", "censored"):
        g = float(b["last_bp"])
    else:
        continue
    r = (g - FLOOR.get(b.get("regime") or "amm", 0)) / 1e4
    # observation end time: the barrier hit for an event, the censoring mark otherwise
    if o in ("tp", "sl"):
        t_end = b.get("t_to_hit_ms")
        if t_end is None:
            t_end = b.get("observed_ms", 0)
    else:
        t_end = b.get("observed_ms", 0)
    for key in (("%s|%s" % (sp, act)), ("POOLED|%s" % (act,))):
        e = ev_rows[key]
        e["r"].append(r)
        e["t"].append(int(t_end or 0))
        e["c"].append(o in ("timeout", "censored"))

print("\n=== M8 IPCW (EV with vs without the censoring correction) ===")
CAP_SWEEP = (1.0, 5.0, 20.0, 100.0, 1e6)   # 1.0 = uncorrected; large = effectively uncapped
ipcw_report = {}
for key, e in sorted(ev_rows.items()):
    ev_u, ev_w, diag = ipcw.ev_ipcw(e["r"], e["t"], e["c"])
    d = {"n": diag["n"], "n_censored": diag["n_censored"],
         "censored_fraction": round(diag["censored_fraction"], 4),
         "ev_unweighted": round(ev_u, 6), "ev_ipcw": round(ev_w, 6),
         "ev_delta": round(ev_w - ev_u, 6), "capped_rows": diag["capped_rows"],
         "min_g": (round(diag["min_g"], 6) if diag["min_g"] is not None else None),
         "ev_ipcw_by_cap": {("%g" % c): round(ipcw.ev_ipcw(e["r"], e["t"], e["c"],
                                                           max_weight=c)[1], 6)
                            for c in CAP_SWEEP}}
    ipcw_report[key] = d
    print("%-24s %s" % (key, json.dumps(d)))

# merge into the per-stratum / pooled records without removing any existing key
for key, d in ipcw_report.items():
    if key.startswith("POOLED|"):
        tgt = pool_report.get(key.split("|", 1)[1])
    else:
        tgt = report.get(key)
    if tgt is not None:
        tgt.update(d)

json.dump({"by_split_action": report, "pooled": pool_report,
           "ipcw": ipcw_report,
           "cost_floor_bps": FLOOR, "schema": "barrier_vs_action_v2_ipcw"},
          open("/training/v2/reports/BARRIER_VS_ACTION.json", "w"), indent=1)
print("\nwrote /training/v2/reports/BARRIER_VS_ACTION.json")