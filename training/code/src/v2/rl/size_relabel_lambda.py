"""Relabel the phase-1 results at a new lambda, exactly, without re-simulating.

reward_scalar = r_other - lambda*w^2, so the stored per-tier values (taken at the
pinned 0.10) re-score for any lambda by adding (0.10 - lambda)*w^2. Same label
rule as phase 1, including the WATCH tie-break (WATCH/SKIP are both 0.0).
"""
import json, sys

LAM_PINNED = 0.10
LAM_NEW = float(sys.argv[1]) if len(sys.argv) > 1 else 0.20
ACC = 1.01
TIER = {"SMALL": 0.25, "MID": 0.5, "FULL": 1.0}
W = {k: TIER[k] / ACC for k in TIER}
SRC = "/training/v2/reports/size_labels_c7.jsonl"
DST = "/training/v2/reports/size_labels_c8.jsonl"

n = changed = 0
import collections
dec = collections.Counter()
size = collections.Counter()
with open(SRC, encoding="utf-8") as fi, open(DST, "w", encoding="utf-8") as fo:
    for line in fi:
        try:
            d = json.loads(line)
        except Exception:
            continue
        n += 1
        vals = d.get("values")
        if d.get("decision") is None or not vals:
            fo.write(json.dumps(d) + "\n")
            continue
        adj = {t: vals[t] + (LAM_PINNED - LAM_NEW) * W[t] ** 2 for t in vals if t in TIER}
        best, bestv = "SKIP", 0.0
        for t in TIER:
            if t in adj and adj[t] > bestv:
                best, bestv = t, adj[t]
        old = d.get("old")
        if best == "SKIP" and old == "WATCH":
            new_dec, new_size = "WATCH", None
        elif best == "SKIP":
            new_dec, new_size = "SKIP", None
        else:
            new_dec, new_size = "BUY", best
        if new_dec != d.get("decision") or new_size != d.get("size"):
            changed += 1
        dec[new_dec] += 1
        size[new_size or "-"] += 1
        d.update({"decision": new_dec, "size": new_size,
                  "values": {k: round(v, 6) for k, v in adj.items()},
                  "values_at_lambda": LAM_NEW, "lambda_exposure": LAM_NEW,
                  "lambda_rebased_from": LAM_PINNED})
        fo.write(json.dumps(d) + "\n")
print(json.dumps({"rows": n, "changed_vs_lambda_0.10": changed,
                  "decision": dict(dec), "size": dict(size),
                  "lambda": LAM_NEW, "out": DST}, indent=1))
