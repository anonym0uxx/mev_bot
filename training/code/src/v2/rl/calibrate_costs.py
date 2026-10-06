#!/usr/bin/env python
"""Calibrate the REAL cost model from our own recorded fills, and FREEZE its quantiles.

The Rust repo is a reference implementation, not the authority (operator ruling,
2026-09-13): Qwen decides trades and Rust ends up as buy/sell paths with optional
stops. So every cost number in the reward must come from OUR data, measured, not
from another codebase's booking constants.

What this measures, per fill, from /training/v2/canonical/renorm_pooltest/trades.jsonl
(91,735 rows with pool-side deltas):
  1. tx fee per fill (fee_lamports = network + priority, from tx meta.fee) - this is
     our REAL per-leg fixed cost, and its bps equivalent at our deployed size.
  2. gross-vs-net: |pool delta| vs |trader delta| for the same fill, which shows
     whether the venue fee sits INSIDE the pool price (already in the reserves) or
     is deducted on top of the trader's leg.
  3. the implied fee rate that the pool deltas support.

WHY THE FREEZE EXISTS. `cost_authority.decompose` used to carry the p90/p99 fixed
cost as INLINE LITERALS (45_000 / 1_005_000) beside a properly named p50 constant.
A cost authority is a sole authority or it is a second opinion: a literal that no
measurement produces is exactly the drift this module exists to remove. The measured
quantiles are now frozen here and READ from the artifact, so a change to either side
is a visible, deliberate act.

Run:
  /home/alon/qwen27b-venv/bin/python calibrate_costs.py                    # report only
  /home/alon/qwen27b-venv/bin/python calibrate_costs.py --freeze --justification "..."
  /home/alon/qwen27b-venv/bin/python calibrate_costs.py --verify-freeze
"""
import argparse
import hashlib
import json
import os
import sys
import time

import numpy as np

P = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
LAMPORTS_PER_SOL = 1_000_000_000
OUT = "/training/v2/reports/FEE_QUANTILES_C16.json"
SCHEMA = "fee-quantiles/1"
# The deployed sizes the bps equivalents are quoted at (the ruled tier ladder's ends).
QUOTE_SIZES_SOL = (0.25, 1.0)


def pct(a, q):
    return float(np.percentile(a, q)) if len(a) else None


def code_sha():
    with open(os.path.abspath(__file__), "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def measure():
    """The measurement. Deterministic, offline, from our own fills only."""
    fees, ratios, by_venue = [], [], {}
    n_rows = 0
    for line in open(P):
        try:
            r = json.loads(line)
        except Exception:
            continue
        n_rows += 1
        f = r.get("fee_lamports")
        if f is not None and f > 0:
            fees.append(int(f))
        ps, ts = r.get("pool_sol_lamports"), r.get("sol_lamports")
        if ps is None or ts is None or ps == 0 or ts == 0:
            continue
        # gross (what the pool moved) vs net (what the trader's balance moved)
        gross, net = abs(int(ps)), abs(int(ts))
        if gross <= 0:
            continue
        ratios.append(net / gross)
        v = r.get("venue", "?")
        by_venue.setdefault(v, []).append(int(f or 0))

    fees_a = np.asarray(fees, dtype=float)
    rat_a = np.asarray(ratios, dtype=float)
    out = {
        "rows": n_rows,
        "fills_with_fee": int(fees_a.size),
        "tx_fee_lamports": {
            "p10": pct(fees_a, 10), "p50": pct(fees_a, 50), "p90": pct(fees_a, 90),
            "p99": pct(fees_a, 99), "mean": float(fees_a.mean()) if fees_a.size else None,
            "max": float(fees_a.max()) if fees_a.size else None,
        },
        "tx_fee_bps_at_1_sol": (pct(fees_a, 50) / LAMPORTS_PER_SOL * 10_000
                                if fees_a.size else None),
        "tx_fee_bps_at_0p1_sol": (pct(fees_a, 50) / (0.1 * LAMPORTS_PER_SOL) * 10_000
                                  if fees_a.size else None),
        "gross_vs_net_ratio": {
            "n": int(rat_a.size), "p01": pct(rat_a, 1), "p50": pct(rat_a, 50),
            "p99": pct(rat_a, 99),
        },
        "implied_deduction_bps_p50": (float((1.0 - pct(rat_a, 50)) * 10_000)
                                      if rat_a.size else None),
        "venues": {v: {"n": len(vals),
                       "tx_fee_p50": float(np.percentile(vals, 50)) if vals else None}
                   for v, vals in sorted(by_venue.items())},
    }
    return out


def frozen_values(m):
    """The three numbers the cost authority reads, rounded to whole lamports.

    Only the FIXED leg is taken from here: the venue fee is a rate the pool deltas
    support and the impact is `clip/depth`, both measured elsewhere. This artifact
    owns exactly one term.
    """
    q = m["tx_fee_lamports"]
    return {
        "p50": int(round(q["p50"])) if q["p50"] is not None else None,
        "p90": int(round(q["p90"])) if q["p90"] is not None else None,
        "p99": int(round(q["p99"])) if q["p99"] is not None else None,
    }


def bps_at(lamports, size_sol):
    return (float(lamports) / (size_sol * LAMPORTS_PER_SOL)) * 10_000.0


def build(m, justification):
    vals = frozen_values(m)
    return {
        "schema": SCHEMA,
        "frozen_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "code_sha256": code_sha(),
        "inputs": {"path": P, "rows": m["rows"], "fills_with_fee": m["fills_with_fee"]},
        "fixed_lamports_per_leg": vals,
        "bps_at_quote_sizes": {
            "%.6g" % s: {k: (bps_at(v, s) if v else None) for k, v in vals.items()}
            for s in QUOTE_SIZES_SOL
        },
        "measured_distribution": m["tx_fee_lamports"],
        "venue_p50": {v: d["tx_fee_p50"] for v, d in m["venues"].items()},
        # The compute-unit term is NOT in this data (fee_lamports is the tx fee the
        # chain charged, not a CU price), so it is declared rather than measured,
        # exactly as the exit-impairment artifact declares its terminal rule.
        "declared_not_measured": {
            "cu_term": "not carried: `fee_lamports` is the charged tx fee (network + priority); "
                       "no per-CU price is recorded in this tape, so the (tip + CU) split of the "
                       "M5 leg cost is declared, not measured, and is NOT used to shrink the "
                       "p50/p90/p99 ladder.",
        },
        "justification": justification,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--freeze", action="store_true")
    ap.add_argument("--verify-freeze", action="store_true")
    ap.add_argument("--justification", default="")
    a = ap.parse_args()

    m = measure()
    if a.verify_freeze:
        if not os.path.isfile(OUT):
            print("VERIFY FAIL: no frozen artifact at %s" % OUT)
            return 1
        old = json.load(open(OUT))
        fresh = build(m, old.get("justification", ""))
        if old.get("code_sha256") != fresh["code_sha256"]:
            print("VERIFY FAIL: the measurement code changed (sha mismatch)\n  frozen %s\n  live   %s"
                  % (old.get("code_sha256"), fresh["code_sha256"]))
            return 1
        if old.get("fixed_lamports_per_leg") != fresh["fixed_lamports_per_leg"]:
            print("VERIFY FAIL: the measured quantiles moved\n  frozen %s\n  live   %s"
                  % (old.get("fixed_lamports_per_leg"), fresh["fixed_lamports_per_leg"]))
            return 1
        print("VERIFY OK: %s matches the live measurement (%s)"
              % (OUT, old["fixed_lamports_per_leg"]))
        return 0

    if a.freeze:
        if not a.justification:
            print("REFUSED: --freeze requires --justification (a value nobody justified is a "
                  "value nobody owns)")
            return 2
        art = build(m, a.justification)
        with open(OUT, "w", encoding="utf-8") as fh:
            json.dump(art, fh, indent=1, sort_keys=False)
            fh.write("\n")
        print("froze %s: %s (rows %d, fills_with_fee %d)"
              % (OUT, art["fixed_lamports_per_leg"], m["rows"], m["fills_with_fee"]))
        return 0

    print(json.dumps(m, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
