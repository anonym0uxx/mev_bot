#!/usr/bin/env python3
"""Gate: does removing the 10k TP survive on TRUE relabelled paths? (M2, 2026-09-18)

The /tmp/theta_oos.py sweep approximated "TP touch" by mfe >= theta on v1 labels,
whose paths are TRUNCATED at the v1 barrier touch. This gate re-prices both policies
on the v2 SL-only relabel, which walks the FULL path:

  TP-policy value (per row)  = v1 outcome: tp -> +TP_BP, sl -> -SL_BP,
                               timeout/censored -> v1 last_bp
                               (v1 outcome IS the first-touch measurement)
  no-TP value (per row)      = v2 outcome: sl -> -SL_BP, else -> v2 last_bp

Both charged the same executable round trip (cost authority p50 + impact). The
population is examination-split BUY rows. Decision statistic: paired per-row gap
(noTP - TP), mint-clustered bootstrap, 95% LB must be > 0 or we stop and ask.
"""
import collections
import hashlib
import json
import random
import re

CORPUS = "/training/v2/candidate_sft_c11"
V2 = "/training/v2/reports/barrier_labels_c11_slonly_1800s.jsonl"
TP_BP = 10_000.0
SL_BP = 5_000.0
COST_BP = 66.0 + 6.0     # AMM round trip + ~3 bp/leg impact
N_BOOT = 4000
SEED = 20260918

def rowkey(rec):
    return hashlib.sha256(json.dumps(rec["messages"][:-1], sort_keys=True,
                                     ensure_ascii=False).encode()).hexdigest()

# v2 labels for the examination split
v2 = {}
with open(V2, encoding="utf-8") as fh:
    for line in fh:
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("split") == "examination" and r.get("status") == "labeled":
            v2[r["key"]] = r

pairs = []            # (mint, v_tp, v_notp)
miss = collections.Counter()
with open(f"{CORPUS}/examination.jsonl", encoding="utf-8") as fh:
    for line in fh:
        try:
            rec = json.loads(line)
        except Exception:
            continue
        m = rec.get("meta") or {}
        if m.get("task") != "decision_action":
            continue
        bt = m.get("barrier_triplet") or {}
        if bt.get("status") != "labeled" and "mfe_bp" not in bt:
            miss["v1_unlabeled"] += 1
            continue
        txt = "".join(x.get("content", "") for x in (rec.get("messages") or [])
                      if x.get("role") == "assistant")
        mm = re.search(r"DECISION\s*[:\-\u2014]\s*\**\s*(\w+)", txt)
        if not mm or mm.group(1).upper() != "BUY":
            continue
        b2 = v2.get(rowkey(rec))
        if b2 is None:
            miss["v2_missing"] += 1
            continue
        o1 = bt.get("outcome")
        if o1 == "tp":
            g_tp = TP_BP
        elif o1 == "sl":
            g_tp = -SL_BP
        else:
            g_tp = float(bt.get("last_bp") or 0.0)
        g_no = -SL_BP if b2["outcome"] == "sl" else float(b2["last_bp"])
        pairs.append((m.get("mint") or "?", g_tp - COST_BP, g_no - COST_BP))

n = len(pairs)
by_mint = collections.defaultdict(list)
for mint, a, b in pairs:
    by_mint[mint].append(b - a)
mints = sorted(by_mint)
gaps = [b - a for _, a, b in pairs]
mean_gap = sum(gaps) / n
mean_tp = sum(a for _, a, _ in pairs) / n
mean_no = sum(b for _, _, b in pairs) / n

rng = random.Random(SEED)
boots = []
for _ in range(N_BOOT):
    s = []
    for _ in range(len(mints)):
        s.extend(by_mint[mints[rng.randrange(len(mints))]])
    boots.append(sum(s) / len(s))
boots.sort()
lb = boots[int(0.05 * N_BOOT)]
ub = boots[int(0.95 * N_BOOT)]

out = {
    "exam_buy_pairs": n, "mints": len(mints), "missing": dict(miss),
    "cost_bp_round_trip": COST_BP,
    "mean_bp_per_trade": {"tp10k_policy": round(mean_tp, 1),
                          "no_tp_policy": round(mean_no, 1)},
    "paired_gap_bp": {"mean": round(mean_gap, 1),
                      "lb95_mint_clustered": round(lb, 1),
                      "p5_p95": [round(lb, 1), round(ub, 1)]},
    "gate": "PASS" if lb > 0 else "FAIL",
}
print(json.dumps(out, indent=1))
with open("/training/v2/reports/GATE_TP_REMOVAL_M2.json", "w") as fh:
    json.dump(out, fh, indent=1)
