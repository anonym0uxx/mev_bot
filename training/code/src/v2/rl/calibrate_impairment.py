#!/usr/bin/env python
"""Measure the EXIT IMPAIRMENT from our own fills - the crash-tail costs the reward
currently ignores.

WHY THIS EXISTS. `simulate_v3` fills every sell at the reserve quote's mark. Real
exits in a distressed market print WORSE than the mark (our own size moves the
book, and a failed first attempt costs the market's move while we retry), and a
position the book cannot absorb is worth nothing like its displayed price. The
reward therefore has to charge an impairment, and per the operator ruling the
magnitudes come from OUR fills, not from another codebase's test fixtures - the
Rust Mode-C numbers (200/300/50 bp) stay as a REFERENCE FLOOR: a measured value
below them is not adopted without justification, because a cheaper exit is the
optimistic direction.

MEASURED, each over a stated population and quantile (the conservative quantile,
p75-adverse, never the median):

  1. `first_sell_penalty_bps` - how much worse the executed sell price is than the
     mint's own trailing price level (the mark the reward assumes). Executed px is
     the ratio of the trader's two legs; the reference is the median of the prior
     20 fill prices of that mint (strictly causal).
  2. `retry_slippage_bps` - the adverse price move across a retry window: the
     consecutive-fill move within 3 s, signed against a seller.
  3. `fee_escalation_lamports` - the congestion adder on the fixed leg:
     p90(fee_lamports) - p50(fee_lamports). Reported in LAMPORTS, not bp: the
     fixed leg already scales as 1/notional in the cost authority, so expressing
     the escalation as bp at one notional would be a remembered number.

Usage:
    calibrate_impairment.py                    # measure + print
    calibrate_impairment.py --freeze --justification "..."
    calibrate_impairment.py --verify-freeze
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys

import numpy as np

TAPE = "/training/v2/canonical/renorm_pooltest/trades.jsonl"
OUT = "/training/v2/reports/EXIT_IMPAIRMENT_C14.json"
LAMPORTS_PER_SOL = 1_000_000_000
# The Rust Mode-C fixtures, kept as a floor. Our measured values must not come out
# CHEAPER than these (see the module docstring).
RUST_REFERENCE = {"first_sell_penalty_bps": 200, "retry_slippage_bps": 300,
                  "fee_escalation_lamports": 50_000}  # 50 bp at the 1 SOL canonical notional


def pct(a, q):
    return None if not len(a) else float(np.percentile(a, q))


def measure():
    by_mint = {}
    fees_by_venue = {}
    rows = 0
    for line in open(TAPE, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        rows += 1
        f = r.get("fee_lamports")
        if f is not None and f > 0:
            fees_by_venue.setdefault(r.get("venue") or "?", []).append(int(f))
        tok, sol = r.get("tokens_raw"), r.get("sol_lamports")
        if not tok or not sol:
            continue
        by_mint.setdefault(r["mint"], []).append(
            (int(r["recv_unix_ms"]), r.get("side"), abs(int(sol)) / abs(int(tok))))

    shortfall_bp, retry_bp = [], []
    for mint, fills in by_mint.items():
        fills.sort(key=lambda x: x[0])
        px_hist = []
        for i, (t, side, px) in enumerate(fills):
            if side == "sell" and len(px_hist) >= 5:
                ref = float(np.median(px_hist[-20:]))
                if ref > 0:
                    # adverse (negative) = we sold below the trailing level
                    shortfall_bp.append((px / ref - 1.0) * 1e4)
            if i and 0 < t - fills[i - 1][0] <= 3_000 and fills[i - 1][2] > 0:
                retry_bp.append(-(px / fills[i - 1][2] - 1.0) * 1e4)
            px_hist.append(px)

    sh = np.asarray(shortfall_bp, dtype=float)
    rt = np.asarray(retry_bp, dtype=float)
    fees = np.asarray([f for v in fees_by_venue.values() for f in v], dtype=float)
    return {
        "source": TAPE,
        "rows": rows,
        "mints": len(by_mint),
        "population": {"sells_with_trailing_mark": int(sh.size),
                       "retry_windows_<=3s": int(rt.size)},
        "measured": {
            # p25 of the signed shortfall = the 25th-percentile (adverse) outcome; we
            # ship its magnitude so the reward charges a bad exit, not an average one.
            "first_sell_penalty_bps": int(round(max(0.0, -(pct(sh, 25) if sh.size else 0.0)))),
            "retry_slippage_bps": int(round(max(0.0, -(pct(rt, 25) if rt.size else 0.0)))),
            "fee_escalation_lamports": int(round((pct(fees, 90) or 0) - (pct(fees, 50) or 0))),
        },
        "diagnostics": {
            "shortfall_bp_p25_p50_p75": [pct(sh, 25), pct(sh, 50), pct(sh, 75)],
            "retry_move_bp_p25_p50_p75": [pct(rt, 25), pct(rt, 50), pct(rt, 75)],
            "fee_lamports_p50_p90_p99": [pct(fees, 50), pct(fees, 90), pct(fees, 99)],
            "fee_by_venue_p50": {v: pct(np.asarray(a, dtype=float), 50)
                                 for v, a in sorted(fees_by_venue.items())},
        },
    }


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def build(measured, justification=None, replaces=None):
    vals = dict(measured["measured"])
    # NEVER CHEAPER THAN THE REFERENCE without an explicit, recorded justification.
    cheaper = {k: vals[k] for k in vals if vals[k] < RUST_REFERENCE[k]}
    if cheaper and not justification:
        raise SystemExit(
            "REFUSING: measured values below the Rust reference floor %s: %s. "
            "Re-run with --justification explaining why a cheaper exit is right."
            % (RUST_REFERENCE, cheaper))
    return {
        "schema": "exit_impairment_c14_v1",
        "code_sha256": sha256_file(os.path.abspath(__file__)),
        "accounting": {
            "notional_sol": 1.0,
            "bps_one": 10_000,
            "population": measured["population"],
            "quantile": "p25 of the signed move (the adverse side)",
        },
        "reference_floor": RUST_REFERENCE,
        "values": vals,
        "measured": measured,
        "justification": justification,
        "replaced": replaces,
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--freeze", action="store_true")
    ap.add_argument("--verify-freeze", action="store_true")
    ap.add_argument("--justification", default=None)
    a = ap.parse_args(argv)

    if a.verify_freeze:
        if not os.path.isfile(OUT):
            print("FREEZE VERIFY FAIL: %s does not exist" % OUT)
            return 1
        pin = json.load(open(OUT, encoding="utf-8"))
        # Verify compares against the PINNED values; the justification for any value
        # below the reference floor lives in the pinned artifact (that is what the
        # freeze is for), so the check must not re-demand it.
        now = build(measure(), justification=pin.get("justification"))
        if pin["values"] != now["values"]:
            print("FREEZE VERIFY FAIL: values drifted\n  pinned: %s\n  now   : %s"
                  % (pin["values"], now["values"]))
            return 1
        if pin["code_sha256"] != sha256_file(os.path.abspath(__file__)):
            print("FREEZE VERIFY FAIL: the measurement code changed (sha mismatch)")
            return 1
        print("FREEZE VERIFY PASS values=%s population=%s"
              % (json.dumps(pin["values"]), json.dumps(pin["accounting"]["population"])))
        return 0

    m = measure()
    print(json.dumps(m, indent=1))
    if a.freeze:
        old = json.load(open(OUT, encoding="utf-8")) if os.path.isfile(OUT) else None
        obj = build(m, justification=a.justification,
                    replaces=(old or {}).get("values"))
        json.dump(obj, open(OUT, "w", encoding="utf-8"), indent=1)
        print("FROZE %s -> %s" % (OUT, json.dumps(obj["values"])))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())