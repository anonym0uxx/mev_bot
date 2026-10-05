#!/usr/bin/env python
"""Measure the DECISION->FILL drift: how much worse the price we actually get is
than the price we saw when we decided.

WHY THIS EXISTS. `simulate_v3` prices every entry at the FIRST tape fill after
the decision clock (rl_reward_v3.simulate_v3 / Tape.first_fill_after) and the RL
prompt states the price at the decision instant (`price_lamports_per_raw_token`
on the DECISION CLOCK line). Those two numbers are NOT the same trade. A decision
is made on a price quote; the fill lands on the next executable print, and for a
pump.fun market where the venue fee and our own size move the price, the next
print is systematically on the adverse side of the quote we acted on. The reward
must charge that gap, and per the operator ruling the magnitude comes from OUR
own real trades, not from a remembered number.

WHAT IS MEASURED, over a stated population and quantile:

  `entry_slippage_bps` - for each decision row (mint, t_dec, decision_price) we
  take the FIRST real trade of that mint at or after t_dec in the trade tape and
  compute drift = (fill_price / decision_price - 1) * 10_000, signed so that a
  POSITIVE value means we paid MORE than the decision price (worse for a buy).
  The shipped value is an UPPER quantile of that signed distribution - the adverse
  side - never the median. The mirror of the exit artifact: a sell charges p25 of
  the signed move, a buy charges the upper tail of the same signed move.

VENUE SPLIT. The tape distinguishes the market a fill printed on (`venue`: pumpfun
= bonding curve, pumpswap = AMM), so the distribution is reported per venue as
well as pooled. The pooled value is what the reward charges when a mint's regime
is unknown at calibration time; the per-venue quantiles say whether one market is
materially worse.

Usage:
    calibrate_fill_drift.py                    # measure + print
    calibrate_fill_drift.py --freeze --justification "..."
    calibrate_fill_drift.py --verify-freeze
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from array import array

import numpy as np

DECISION_FILES = ["/training/v2/candidate_sft_c8/train.jsonl",
                  "/training/v2/candidate_sft_c8/validation.jsonl",
                  "/training/v2/candidate_sft_c8/examination.jsonl"]
TAPE = "/training/v2/canonical/renormalized_v7/trades.jsonl"
OUT = "/training/v2/reports/FILL_DRIFT_C15.json"

# The prompt carries the full-precision decision price on its DECISION CLOCK line
# and a rounded copy under PRICE UNITS. Parse the full-precision one; the rounded
# copy is only a fallback for a row whose clock line is absent.
RE_T_DEC = re.compile(r"t_dec_ms=(\d+)")
RE_PX = re.compile(r"price_lamports_per_raw_token=([0-9.eE+-]+)")
VENUE_CURVE, VENUE_AMM = "pumpfun", "pumpswap"
# A print below this notional is not a tradeable price. Same floor and same reason
# as rl_reward_v3.DEFAULT_NOTIONAL_FLOOR_LAMPORTS: a 1-lamport transfer still prints
# a ratio, and an unfiltered tape makes the measured drift a property of dust (the
# unfiltered mean was 669,351 bp with sd 654,428x - one dust print, not the market).
MIN_NOTIONAL_LAMPORTS = 1_000_000
# Price-scale guard: a fill/decision ratio outside this band is the 1000x unit-error
# class (a lamports-vs-SOL or dp mismatch), not a price move. Counted and reported,
# never silently winsorized into the quantiles.
MAX_PX_RATIO = 1000.0


def pct(a, q):
    return None if not len(a) else float(np.percentile(a, q))


def _load_decisions():
    """Every unique (mint, t_dec_ms, decision_price) from the decision corpus."""
    rows = 0
    missing_clock = 0
    per_file = {}
    seen = {}
    for p in DECISION_FILES:
        n_file = 0
        for line in open(p, encoding="utf-8"):
            if not line.strip():
                continue
            n_file += 1
            rows += 1
            try:
                r = json.loads(line)
            except Exception:
                continue
            try:
                c = r["messages"][1]["content"]
            except (KeyError, IndexError, TypeError):
                continue
            m = RE_T_DEC.search(c)
            if not m:
                missing_clock += 1
                continue
            pxm = RE_PX.search(c)
            if not pxm:
                continue
            mint = r.get("mint") or (r.get("meta") or {}).get("mint")
            try:
                key = (mint, int(m.group(1)), float(pxm.group(1)))
            except (TypeError, ValueError):
                continue
            if mint is None or key[2] <= 0 or not np.isfinite(key[2]):
                continue
            seen[key] = True
        per_file[p] = n_file
    return sorted(seen), rows, missing_clock, per_file


def _load_tape_for(mints, path=TAPE):
    """Single streaming pass: keep only the decision mints' trades, per mint three
    parallel arrays (ts, price in lamports per raw token, venue code).

    Only decisions' mints are kept - the full tape is 15.9M rows / all of pump.fun,
    and 3,161 decision mints carry ~3.2M of those rows. Arrays (not tuples) because
    3.2M python tuples is ~400 MB of interpreter object overhead for no benefit.
    """
    want = set(mints)
    ts_by, px_by, vn_by = {}, {}, {}
    n_rows = 0
    n_kept = 0
    n_skipped = 0
    n_dust = 0
    for line in open(path, encoding="utf-8"):
        if not line.strip():
            continue
        n_rows += 1
        try:
            r = json.loads(line)
        except Exception:
            n_skipped += 1
            continue
        mint = r.get("mint")
        if mint not in want:
            continue
        sol, tok = r.get("sol_lamports"), r.get("tokens_raw")
        t = r.get("recv_unix_ms")
        if not sol or not tok or t is None:
            continue
        sol, tok = abs(int(sol)), abs(int(tok))
        if tok <= 0 or sol <= 0:
            continue
        if sol < MIN_NOTIONAL_LAMPORTS:
            n_dust += 1
            continue
        if mint not in ts_by:
            ts_by[mint] = array("q")
            px_by[mint] = array("d")
            vn_by[mint] = array("b")
        # The recorded leg is signed (+SOL for a sell, -SOL for a buy); the market
        # price is the ratio of magnitudes, in the prompt's own units.
        ts_by[mint].append(int(t))
        px_by[mint].append(sol / tok)
        vn_by[mint].append(0 if str(r.get("venue") or "").startswith(VENUE_CURVE) else 1)
        n_kept += 1
    return ts_by, px_by, vn_by, {"rows": n_rows, "kept": n_kept, "skipped": n_skipped,
                                 "mints": len(ts_by),
                                 "dust_below_notional_floor": n_dust,
                                 "notional_floor_lamports": MIN_NOTIONAL_LAMPORTS}


def measure(decision_files=None, tape=None):
    global DECISION_FILES, TAPE
    if decision_files:
        DECISION_FILES = list(decision_files)
    if tape:
        TAPE = tape
    decisions, rows, missing_clock, per_file = _load_decisions()
    mints = {d[0] for d in decisions}
    ts_by, px_by, vn_by, tape_stats = _load_tape_for(mints)

    ts_s, px_s, vn_s = [], [], []          # pooled signed drift
    gaps = []
    no_fill = 0
    scale_out = 0
    by_venue = {VENUE_CURVE: [], VENUE_AMM: []}
    # Diagnostic only: the first BUY-side print. The primary sample is the first
    # print of ANY side, because that is the price the market was last willing to
    # trade at when we arrived - requiring a buy would discard exactly the quiet,
    # sell-dominated moments where execution is worst.
    for mint, t_dec, dec_px in decisions:
        ts = ts_by.get(mint)
        if ts is None or len(ts) == 0:
            no_fill += 1
            continue
        ts_np = np.frombuffer(ts, dtype=np.int64)
        i = int(np.searchsorted(ts_np, t_dec, side="left"))
        if i >= ts_np.size:
            no_fill += 1
            continue
        fill_px = px_by[mint][i]
        ratio = fill_px / dec_px
        if not np.isfinite(ratio) or ratio <= 0:
            continue
        if ratio > MAX_PX_RATIO or ratio < 1.0 / MAX_PX_RATIO:
            scale_out += 1
            continue
        drift = (ratio - 1.0) * 1e4
        if not np.isfinite(drift):
            continue
        ts_s.append(drift)
        gaps.append(int(ts_np[i]) - int(t_dec))
        v = VENUE_CURVE if vn_by[mint][i] == 0 else VENUE_AMM
        by_venue[v].append(drift)
        vn_s.append(v)

    drift = np.asarray(ts_s, dtype=float)
    gap = np.asarray(gaps, dtype=np.int64)

    def _capped(cap_ms):
        m = gap <= cap_ms
        a = drift[m]
        return {"n": int(a.size),
                "quantiles": {q: pct(a, q) for q in (25, 50, 75, 90, 99)}}

    sd = float(drift.std(ddof=1)) if drift.size > 1 else 0.0
    se = sd / (drift.size ** 0.5) if drift.size else None
    out = {
        "source": TAPE,
        "decision_files": list(DECISION_FILES),
        "decision_rows_read": rows,
        "decision_rows_missing_clock": missing_clock,
        "decision_rows_by_file": per_file,
        "sampled_decisions": int(drift.size),
        "decisions_without_following_trade": no_fill,
        "decisions_dropped_price_scale_guard": scale_out,
        "mints": len(mints),
        "mints_with_trades": tape_stats["mints"],
        "tape": tape_stats,
        "population": {
            "decisions_with_fill": int(drift.size),
            "by_venue": {v: len(a) for v, a in by_venue.items()},
        },
        "quantiles_pooled": {q: pct(drift, q) for q in (10, 25, 50, 75, 90, 95, 99)},
        "quantiles_by_venue": {v: {q: pct(np.asarray(a, dtype=float), q)
                                   for q in (25, 50, 75, 90, 99)}
                               if a else None
                               for v, a in by_venue.items()},
        "latency_ms": {"p50": pct(gap, 50), "p90": pct(gap, 90), "p99": pct(gap, 99),
                       "max": (int(gap.max()) if gap.size else None)},
        "positive_fraction": (float((drift > 0).mean()) if drift.size else None),
        # SIGNIFICANCE OF THE SYSTEMATIC COMPONENT. The headline is that the MEDIAN
        # drift is indistinguishable from zero: the decision price is not a biased
        # estimate of the next fill, it is a noisy one. The mean is reported with its
        # own standard error so an "on average we pay X bp" claim can be refuted here
        # rather than asserted.
        "systematic": {
            "mean_bp": (float(drift.mean()) if drift.size else None),
            "median_bp": pct(drift, 50),
            "sd_bp": sd,
            "stderr_bp": se,
            "t_stat": (float(drift.mean() / se) if se else None),
            "positive_fraction": (float((drift > 0).mean()) if drift.size else None),
        },
        # The ADVERSE side isolated: only the fills that were worse than the decision
        # price. Recorded because the trained value is drawn from the signed
        # distribution's upper tail, and a reader must be able to see how much of the
        # charge is "the worse half", not a separate metric.
        "positive_side_quantiles": {q: pct(drift[drift > 0], q)
                                    for q in (50, 75, 90, 99)},
        # How much of the tail is EXECUTION LATENCY rather than same-instant
        # dispersion. The reward's own fill is the first tape print after t_dec
        # whatever the gap, so the uncapped distribution is the shipped population -
        # but the capped views say whether the p90 is a realistic execution window or
        # a quiet mint matched to a print hours later.
        "latency_capped": {"<=5s": _capped(5_000), "<=30s": _capped(30_000),
                           "<=300s": _capped(300_000)},
    }
    return out


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# The quantile the reward charges. p90 of the SIGNED drift: the adverse side for a
# buy (positive = paid more). p75 would be the exact mirror of the impairment
# artifact's p25 and is recorded but not shipped: at 376 bp it is barely above the
# measurement noise (the distribution is symmetric, so a buy has no systematic
# penalty to charge), and the reward is required to be conservative against the
# realized return. p99 (51,822 bp) is recorded and explicitly NOT shipped - see the
# justification: it is dominated by the tape's coverage artifact (p90 fill latency
# 151 s, p99 8 h), not by a price we would deliberately cross.
TRAIN_QUANTILE = 90


def build(measured, justification=None, replaces=None):
    pooled = measured["quantiles_pooled"]
    v90 = pooled.get(str(TRAIN_QUANTILE)) or pooled.get(TRAIN_QUANTILE)
    by = measured["quantiles_by_venue"]
    def _q(v, q):
        d = by.get(v) or {}
        return d.get(str(q)) if d.get(str(q)) is not None else d.get(q)
    value = max(0.0, float(v90 if v90 is not None else 0.0))
    worst_venue = max((_q(v, TRAIN_QUANTILE) or 0.0) for v in (VENUE_CURVE, VENUE_AMM))
    return {
        "schema": "fill_drift_c15_v1",
        "code_sha256": sha256_file(os.path.abspath(__file__)),
        "accounting": {
            "bps_one": 10_000,
            "population": measured["population"],
            "sampled_decisions": measured["sampled_decisions"],
            "quantile": "p%d of the signed drift (positive = paid MORE than the "
                        "decision price, the adverse side for a buy)" % TRAIN_QUANTILE,
            "sign_convention": "drift_bp = (fill_price/decision_price - 1) * 10000",
            "inputs": [{"path": TAPE, "rows": measured["tape"]["rows"],
                        "rows_for_decision_mints": measured["tape"]["kept"]}]
                      + [{"path": p, "rows": n}
                         for p, n in sorted(measured["decision_rows_by_file"].items())],
        },
        "values": {
            "entry_slippage_bps": int(round(value)),
            # Recorded, not trained: the stress level the readiness gate keys on. It
            # exists so a run that adopts the pooled p90 can be compared with one that
            # charges the worst venue's p90.
            "entry_slippage_bps_stress": int(round(max(0.0, worst_venue))),
        },
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
        # Compared against the PINNED values, like the impairment calibrator: the
        # justification belongs to the freeze, so the check must not re-demand it.
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
    sys.exit(main())
