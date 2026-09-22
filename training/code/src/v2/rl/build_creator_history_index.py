"""Build the creator-history index for c9 (the 'good vs bad dev' signal).

WHY: the capture window is 4 sessions over 17.6 days. If creator history were computed only
inside the window it would be truncation-biased (a serial rugger with 2 launches visible out of
50 real ones would look benign). Two things make this correct:

  1. The index spans BOTH sessions (Aug 23/24 AND Sep 9/10), so a creator who launched in August
     and again in September IS a visible repeat launcher.
  2. Every count is CAUSAL: for a launch at t, we count only that creator's launches with
     recv_unix_ms strictly < t. No future launches leak into a feature.

Outputs a JSON index: creator -> sorted list of launch times, plus per-launch prior_ca_count.
Then reports how much of the corpus actually gains dev-history signal.
"""
import json
import collections
import datetime
import os

L = "/training/v2/canonical/renormalized_v7/launches.jsonl"
OUT = "/training/v2/reports/CREATOR_HISTORY_INDEX.json"
CORPUS = "/training/v2/candidate_sft_c8/train.jsonl"

launches = []
n = 0
bad = 0
with open(L, encoding="utf-8") as fh:
    for line in fh:
        n += 1
        try:
            r = json.loads(line)
        except Exception:
            bad += 1
            continue
        c = r.get("creator")
        m = r.get("mint")
        t = r.get("recv_unix_ms")
        if c and m and t:
            launches.append((c, m, int(t), bool(r.get("failed"))))

print("launch rows:", f"{n:,}", "| unparseable:", bad, "| usable:", f"{len(launches):,}")

by_creator = collections.defaultdict(list)
for c, m, t, failed in launches:
    by_creator[c].append((t, m, failed))

launch_times = {c: sorted(t for t, _m, _f in v) for c, v in by_creator.items()}
mint_creator = {m: c for c, m, _t, _f in launches}

# causal prior-launch count for every launch
prior_by_launch = {}
for c, m, t, _f in launches:
    ts = launch_times[c]
    lo, hi = 0, len(ts)
    while lo < hi:                      # bisect_left: strictly less than t
        mid = (lo + hi) // 2
        if ts[mid] < t:
            lo = mid + 1
        else:
            hi = mid
    prior_by_launch[m] = lo

# ---- stats
unique_creators = len(by_creator)
repeat = sum(1 for c, v in by_creator.items() if len(v) >= 2)
serial = sum(1 for c, v in by_creator.items() if len(v) >= 5)
print("\nunique creators:", f"{unique_creators:,}")
print("creators with >=2 launches (repeat):", f"{repeat:,} ({100.0*repeat/unique_creators:.1f} pct)")
print("creators with >=5 launches (serial):", f"{serial:,} ({100.0*serial/unique_creators:.1f} pct)")

# session span of the index
allt = [t for _c, _m, t, _f in launches]
f = lambda ms: datetime.datetime.fromtimestamp(ms / 1000, datetime.UTC).strftime('%Y-%m-%d %H:%M')
print("index span:", f(min(allt)), "->", f(max(allt)))

# ---- how much of the CORPUS gains real dev-history signal?
dec_mints = set()
for line in open(CORPUS, encoding="utf-8"):
    r = json.loads(line)
    m = (r.get("meta") or {})
    if m.get("task") == "decision_action" and m.get("mint"):
        dec_mints.add(m["mint"])

have_creator = [m for m in dec_mints if m in mint_creator]
known = [m for m in have_creator if mint_creator[m] in by_creator]
with_prior = [m for m in have_creator if prior_by_launch.get(m, 0) > 0]

print("\ncorpus decision mints:", f"{len(dec_mints):,}")
print("  creator resolvable:", f"{len(have_creator):,} ({100.0*len(have_creator)/max(1,len(dec_mints)):.1f} pct)")
print("  creator has PRIOR launches (real dev history):", f"{len(with_prior):,} ({100.0*len(with_prior)/max(1,len(dec_mints)):.1f} pct)")
print("  no history -> 'Unknown' (correctly not 'good'):", f"{len(dec_mints)-len(with_prior):,}")

json.dump({
    "source": L,
    "launch_rows": len(launches),
    "unique_creators": unique_creators,
    "repeat_creators_ge2": repeat,
    "serial_creators_ge5": serial,
    "index_span": [f(min(allt)), f(max(allt))],
    "corpus_decision_mints": len(dec_mints),
    "creator_resolvable": len(have_creator),
    "creator_with_prior_launches": len(with_prior),
    "causal_rule": "prior_ca_count = launches by same creator with recv_unix_ms strictly < t",
}, open(OUT, "w", encoding="utf-8"), indent=1)
print("\nwrote", OUT)
