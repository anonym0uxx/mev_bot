"""curve_integrate — causal pump.fun curve-reserve reconstruction for the wall window.

WHY THIS EXISTS
---------------
The forward wall (the RL eval set) runs 09-09 13:04 -> 21:04. The curve capture
stream (s0909b) ENDS 09-09 12:39 - 25 minutes before the wall starts. So 354 of
the wall's 475 decision mints (74%, all curve-venue / never-graduated) had no
curve reserve state inside their own evaluation window, and the engine could only
price them from reserves up to 8.4 h stale.

The fills are already on disk (the canonical tape covers 84,307 of their fills
INSIDE the wall window; 354/354 mints have a fill at/after wall_ms). The bonding
curve is deterministic, so the reserve state is RECONSTRUCTIBLE by integration -
no re-capture and no RPC (a live LaserStream cannot reach a 5-day-old window anyway).

THE UPDATE RULE (verified against captured transitions, exactly):
    post_sol   = pre_sol   + sol_amount     (buy)
    post_sol   = pre_sol   - sol_amount     (sell)
    post_token = pre_token - token_amount   (buy)
    post_token = pre_token + token_amount   (sell)
The TAPE stores flows from the trader's side and needs no side-dependent branch:
    pool_sol   -= sol_lamports      (a buy carries sol_lamports < 0 -> pool up)
    pool_token -= tokens_raw
CAUSALITY: a state is emitted only AFTER applying the fill at its own timestamp,
so a lookup at t_dec never sees a state derived from a fill after t_dec.

`--validate` proves the rule against the captured window before anything is scored
with it: integrate the tape from an early captured anchor and require the final
state to equal the captured final state, exactly, per mint.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import sys

import pyarrow.parquet as pq

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from regime_pricing import DictReserveOracle, Regime, ReserveState  # noqa: E402

CURVE_DIRS = ["/training/v2/reserves/s0909a", "/training/v2/reserves/s0909b"]
TAPE = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
SOURCE = "tape_integrated_curve"

_MINT = re.compile(r'"mint"\s*:\s*"([^"]+)"')
_TS = re.compile(r'"recv_unix_ms"\s*:\s*(\d+)')
_SOL = re.compile(r'"sol_lamports"\s*:\s*(-?\d+)')
_TOK = re.compile(r'"tokens_raw"\s*:\s*(-?\d+)')
_VEN = re.compile(r'"venue"\s*:\s*"([^"]+)"')


def load_captured(dirs, mints=None):
    """Per mint: earliest and latest captured state, for anchoring + validation."""
    first, last = {}, {}
    for d in dirs:
        for f in sorted(glob.glob(os.path.join(d, "*.parquet"))):
            t = pq.read_table(f, columns=["mint_b58", "recv_unix_ms",
                                          "pre_virtual_sol_reserves_lamports",
                                          "pre_virtual_token_reserves_raw",
                                          "virtual_sol_reserves_lamports",
                                          "virtual_token_reserves_raw"])
            c = t.to_pydict()
            for i in range(len(c["mint_b58"])):
                m = c["mint_b58"][i]
                if mints is not None and m not in mints:
                    continue
                ts = int(c["recv_unix_ms"][i])
                pre = (int(c["pre_virtual_sol_reserves_lamports"][i]),
                       int(c["pre_virtual_token_reserves_raw"][i]))
                post = (int(c["virtual_sol_reserves_lamports"][i]),
                        int(c["virtual_token_reserves_raw"][i]))
                if m not in first or ts < first[m][0]:
                    first[m] = (ts, pre[0], pre[1])
                if m not in last or ts > last[m][0]:
                    last[m] = (ts, post[0], post[1])
    return first, last


def load_tape_fills(mints):
    fills = {}
    with open(TAPE, encoding="utf-8") as fh:
        for line in fh:
            m = _MINT.search(line)
            if not m:
                continue
            mi = m.group(1)
            if mi not in mints:
                continue
            v = _VEN.search(line)
            if not v or v.group(1) != "pumpfun":
                continue          # ONLY curve fills move the curve reserve; an
                                  # AMM (pumpswap) fill would double-count it
            t, s, k = _TS.search(line), _SOL.search(line), _TOK.search(line)
            if not (t and s and k):
                continue
            fills.setdefault(mi, []).append((int(t.group(1)), int(s.group(1)),
                                            int(k.group(1))))
    for v in fills.values():
        v.sort(key=lambda x: x[0])
    return fills


def integrate(anchor_ts, anchor_sol, anchor_tok, fills, min_ts=None,
              until_ts=None):
    """Yield (ts, sol, tok) after each fill strictly after the anchor and at or
    before until_ts. The upper bound is REQUIRED for validation: integrating past
    the captured window and then comparing against the captured end state
    compares two different times (that bug produced 0/300 the first run)."""
    s, k = anchor_sol, anchor_tok
    out = []
    for ts, sol, tok in fills:
        if ts <= anchor_ts:
            continue
        if min_ts is not None and ts < min_ts:
            continue
        if until_ts is not None and ts > until_ts:
            continue
        s -= sol                      # tape is trader-side; see module docstring
        k -= tok
        out.append((ts, s, k))
    return out


def build_oracle(dirs=None, tape=TAPE, mints=None, verbose=True):
    """Curve oracle for the wall window, integrated from the last captured anchor."""
    first, last = load_captured(dirs or CURVE_DIRS, mints=mints)
    fills = load_tape_fills(set(last) | set(mints or []))
    o = DictReserveOracle(default_source=SOURCE)
    st = {"anchored": 0, "integrated_states": 0, "no_anchor": 0, "mints": 0}
    for m, (ts, s, k) in last.items():
        if m not in fills:
            st["no_anchor"] += 1
            continue
        st["anchored"] += 1
        seq = integrate(ts, s, k, fills[m])
        for (t2, s2, k2) in seq:
            if s2 <= 0 or k2 <= 0:
                continue
            o.add(ReserveState(regime=Regime.BONDING_CURVE, mint=m,
                               sol_lamports=s2, token_raw=k2, ts_unix_ms=t2,
                               venue="pumpfun", account=None, source=SOURCE))
            st["integrated_states"] += 1
        st["mints"] += 1
    if verbose:
        print(json.dumps({"curve_integrate": st}), flush=True)
    return o, st


def validate(dirs=None, tape=TAPE, sample=200):
    """Integrate the tape across the captured window and require EXACT equality
    with the captured end state. Proves the update rule and the sign convention."""
    first, last = load_captured(dirs or CURVE_DIRS)
    fills = load_tape_fills(set(last))
    mints = [m for m in last if m in fills and last[m][0] > first[m][0]][:sample]
    exact = 0
    bad = []
    checked = 0
    for m in mints:
        ats, asol, atok = first[m]
        ets, esol, etok = last[m]
        seq = integrate(ats, asol, atok, fills[m], until_ts=ets)
        if not seq:
            continue
        checked += 1
        got = seq[-1]
        if got[1] == esol and got[2] == etok:
            exact += 1
        else:
            bad.append({"mint": m, "captured": [esol, etok],
                        "integrated": [got[1], got[2]],
                        "d_sol": got[1] - esol, "d_tok": got[2] - etok,
                        "fills_used": len(seq)})
    res = {"checked": checked, "exact": exact,
           "match_rate": round(exact / max(checked, 1), 6),
           "failures": bad[:5], "n_failed": len(bad),
           "verdict": "PASS" if checked and exact == checked else "FAIL"}
    print(json.dumps(res, indent=1), flush=True)
    with open("/training/v2/reports/CURVE_INTEGRATE_VALIDATION.json", "w",
              encoding="utf-8") as fh:
        json.dump(res, fh, indent=1)
    return res


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--validate", action="store_true")
    ap.add_argument("--sample", type=int, default=200)
    a = ap.parse_args(argv)
    if a.validate:
        return 0 if validate(sample=a.sample)["verdict"] == "PASS" else 1
    build_oracle()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
