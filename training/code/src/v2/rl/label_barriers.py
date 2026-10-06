"""Barrier-triplet labels (R1) for the c9 corpus — reserve-priced, cost-inclusive.

For every ENTRY decision row we do the desk-standard triple-barrier label instead of
the old single realized path:

  * entry fill  : the NEXT executable fill after t_dec, priced from RESERVES
                  (never tape.px). qty comes from the fill itself.
  * horizontal  : TP = +10,000 bp (+100%, the operative fm>2x target), SL = -5,000 bp
    barriers      (-50%, the drawdown target derived from measured MAE in
                  DRAWDOWN_DERIVATION.json).
  * vertical    : horizon 1,800,000 ms — the SAME horizon the reward engine uses,
    barrier       so label and scorer cannot disagree about time.
  * outcome     : the FIRST barrier the EXECUTABLE mark touches, else TIMEOUT.
  * MFE / MAE   : best/worst executable mark over the horizon, in bp vs entry_eff.
  * CENSORING   : right-censored when the tape ends before t_entry+horizon. A
                  censored row is NEVER silently treated as a loss (R5).

Costs enter at BARRIER CONSTRUCTION, not through the realized path: every row records
the round-trip floor at the canonical notional (`cost_floor_bps`, run live — never a
remembered table) so a cost-model change re-scores without re-simulating.

Unit discipline (the 1e9-class trap): `px_exec` is lamports per RAW token while
`sol_out` is SOL. The only self-consistent mark is the realized ratio
`sol_out / qty`. All bp figures are relative to `entry_eff = deploy_sol / qty`.

Deterministic and resumable: jobs are mint-stratified and a row whose key is already
in the output is skipped. Runs under fork so loaded reserves are shared.
"""
import collections
import hashlib
import json
import os
import sys
import time

sys.path.insert(0, "/training/v2/code/src/v2/rl")
import multiprocessing as mp  # noqa: E402

import numpy as np  # noqa: E402

from rl_reward_v3 import (  # noqa: E402
    V3Engine, ReserveRegistry, STALE_ANY, load_canonical_tapes,
    ACCOUNT_CAPITAL_SOL, HORIZON_MS_DEFAULT,
    V3RefusesToPrice, ReserveUnavailable, ReserveStale, ReserveLookahead,
    RegimeMismatch, OrderTooLarge,
)
from exit_mechanics import cost_floor_bps, DEPLOY_SOL_CANONICAL  # noqa: E402

CORPUS = os.environ.get("BARRIER_CORPUS", "/training/v2/candidate_sft_c9")
OUT = os.environ.get("BARRIER_OUT", "/training/v2/reports/barrier_labels_c9.jsonl")

# TP is parameterized so the SAME labeller serves the v1 (10k TP) and v2 (SL-only)
# regimes. BARRIER_TP_BP=0 disables the horizontal TP barrier entirely: the walk
# runs to SL or horizon and mfe/mae/last are measured over the FULL path — under
# v1 the loop broke at the TP touch, so a "tp" row's excursions are truncated at
# the touch and its true forward path is unknown. v2 exists because the exit
# authority is the management policy, not a fixed profit cap (exam-split sweep,
# 2026-09-18: no-TP beats 10k TP by +476 bp/trade mean, mint-clustered 95% LB +279).
TP_BP = float(os.environ.get("BARRIER_TP_BP", "10000"))
TP_ENABLED = TP_BP > 0.0
SCHEMA = "barrier_triplet_v1" if TP_ENABLED else "barrier_triplet_v2_sl_only"
SL_BP = 5_000.0           # -50%: the drawdown target derived from measured MAE
# Horizon is parameterized so the SAME labeller can serve both consumers:
#   * 1,800,000 ms (engine default) -> the entry barrier label / max-drawdown basis
#   *   300,000 ms                  -> the decision_reasoning family, whose system
#     prompt forecasts "the executable 300s round trip", so its targets must be
#     measured over 300 s or the label answers a question the prompt did not ask.
HORIZON_MS = int(os.environ.get("BARRIER_HORIZON_MS", str(int(HORIZON_MS_DEFAULT))))
TICK_MS = int(os.environ.get("BARRIER_TICK_MS", "5000"))
MAX_TICKS = HORIZON_MS // TICK_MS

WORKERS = int(sys.argv[1]) if len(sys.argv) > 1 else 24
LIMIT = int(sys.argv[2]) if len(sys.argv) > 2 else 0

REG = ReserveRegistry.build(verbose=False)
ENG = V3Engine(REG, max_reserve_stale_ms=STALE_ANY, deploy_sol=DEPLOY_SOL_CANONICAL)
TAPES = load_canonical_tapes("/training/v2/canonical/renorm_corpus_mints/trades.jsonl",
                             min_notional_lamports=100_000, max_px_ratio=50)

# The cost floor is a FUNCTION of regime + notional + fee quantile; it is NOT a
# constant. Compute it once per regime at the canonical notional so the number
# recorded on every row is the one the engine would charge.
FLOOR_BPS = {}
for _reg, _notional in (("amm", DEPLOY_SOL_CANONICAL), ("bonding_curve", DEPLOY_SOL_CANONICAL)):
    try:
        FLOOR_BPS[_reg] = int(cost_floor_bps(_reg, _notional, 0, False, "p50"))
    except Exception as _e:                                     # noqa: BLE001
        FLOOR_BPS[_reg] = None


def rowkey(rec):
    return hashlib.sha256(json.dumps(rec["messages"][:-1], sort_keys=True,
                                     ensure_ascii=False).encode()).hexdigest()


jobs = []
for split in ("train", "validation", "examination"):
    p = os.path.join(CORPUS, "%s.jsonl" % split)
    if not os.path.isfile(p):
        continue
    with open(p, encoding="utf-8") as fh:
        for ln, line in enumerate(fh):
            try:
                rec = json.loads(line)
            except Exception:                                   # noqa: BLE001
                continue
            m = rec.get("meta") or {}
            if m.get("task") != "decision_action":
                continue
            eid = (rec.get("episode_id") or "").split(":")
            if len(eid) < 3:
                continue
            try:
                t_dec = int(eid[2])
            except ValueError:
                continue
            jobs.append((split, ln, eid[1], t_dec, rowkey(rec)))

jobs.sort(key=lambda j: (j[2], j[3]))
# Round-robin across mints so ANY prefix of the job list is representative.
# Sorting by mint alone clustered a 200-row smoke sample inside a single mint and
# made the refusal rate look like a property of the corpus when it was a property
# of one token.
_by_mint = collections.defaultdict(list)
for _j in jobs:
    _by_mint[_j[2]].append(_j)
_mints = sorted(_by_mint)
jobs = [_by_mint[_m][_i] for _i in range(max(len(v) for v in _by_mint.values()))
        for _m in _mints if _i < len(_by_mint[_m])]
done = set()
if os.path.isfile(OUT):
    with open(OUT, encoding="utf-8") as fh:
        for line in fh:
            try:
                done.add(json.loads(line)["key"])
            except Exception:                                   # noqa: BLE001
                continue
todo = [j for j in jobs if j[4] not in done]
n_todo_all = len(todo)
if LIMIT:
    todo = todo[:LIMIT]
print(json.dumps({"entry_rows": len(jobs), "already_done": len(jobs) - n_todo_all,
                  "todo_pending": n_todo_all, "todo": len(todo), "workers": WORKERS,
                  "tp_bp": TP_BP, "sl_bp": SL_BP, "horizon_ms": HORIZON_MS,
                  "tick_ms": TICK_MS, "floor_bps": FLOOR_BPS}), flush=True)


def work(j):
    split, ln, mint, t_dec, key = j
    base = {"key": key, "split": split, "line": ln, "mint": mint, "t_dec": t_dec,
            "tp_bp": TP_BP, "sl_bp": SL_BP, "horizon_ms": HORIZON_MS,
            "floor_bps": FLOOR_BPS, "schema": SCHEMA}
    tape = TAPES.get(mint)
    if tape is None:
        base.update({"status": "no_tape"})
        return base
    tt = tape.tt
    if not (int(tt[0]) <= t_dec <= int(tt[-1])):
        base.update({"status": "t_dec_outside_tape"})
        return base

    j0 = tape.first_fill_after(t_dec, t_dec, "entry")
    if j0 is None:
        base.update({"status": "no_entry"})
        return base
    t_entry = int(tt[j0])
    try:
        qb = ENG.quote_buy(tape, t_entry, t_dec, DEPLOY_SOL_CANONICAL)
    except (V3RefusesToPrice, ReserveUnavailable, ReserveStale, ReserveLookahead,
            RegimeMismatch, OrderTooLarge) as e:
        base.update({"status": "refused_no_reserves",
                     "reason": type(e).__name__})
        return base
    if not qb or float(qb.get("tokens") or 0.0) <= 0:
        base.update({"status": "no_fill"})
        return base

    qty = float(qb["tokens"])
    entry_eff = DEPLOY_SOL_CANONICAL / qty
    try:
        _rg = ENG.resolve_regime(tape, t_dec)
        regime_name = getattr(_rg, "value", str(_rg))
    except Exception:                                           # noqa: BLE001
        regime_name = None
    t_end_cap = int(tt[-1])
    t_end = min(t_entry + HORIZON_MS, t_end_cap)
    censored = (t_entry + HORIZON_MS) > t_end_cap

    tp_lvl = entry_eff * (1.0 + TP_BP / 1e4) if TP_ENABLED else None
    sl_lvl = entry_eff * (1.0 - SL_BP / 1e4)

    status = "timeout"
    t_tp = t_sl = None
    mfe_bp = mae_bp = 0.0
    n_marks = 0
    last_bp = 0.0
    t_last = t_entry
    obs_ms = max(0, t_end - t_entry)

    ticks = np.arange(t_entry + TICK_MS, t_end + 1, TICK_MS, dtype=np.int64)
    npri = np.searchsorted(tt, ticks, side="right") - 1
    keep = npri >= j0
    ticks, npri = ticks[keep], npri[keep]
    # Deduplicate by tape index: a sparse tape maps many ticks to one fill and
    # re-quoting the same instant is pure cost with no new information.
    if ticks.size:
        _, first_idx = np.unique(npri, return_index=True)
        ticks, npri = ticks[first_idx], npri[first_idx]

    for k in range(ticks.size):
        tM, i = int(ticks[k]), int(npri[k])
        t_fill = int(tt[i])
        if t_fill <= t_entry:
            continue
        try:
            qm = ENG.quote_sell(tape, t_fill, tM, qty)
        except (ReserveUnavailable, ReserveStale, ReserveLookahead,
                RegimeMismatch, OrderTooLarge):
            continue
        if not qm:
            continue
        try:
            cur = float(qm["sol_out"]) / qty
        except (KeyError, TypeError, ZeroDivisionError):
            continue
        if not np.isfinite(cur) or cur <= 0:
            continue
        n_marks += 1
        t_last = t_fill
        bp = (cur / entry_eff - 1.0) * 1e4
        last_bp = bp
        if bp > mfe_bp:
            mfe_bp = bp
        if bp < mae_bp:
            mae_bp = bp
        if cur <= sl_lvl:
            status, t_sl = "sl", t_fill
            break
        if TP_ENABLED and tp_lvl is not None and cur >= tp_lvl:
            status, t_tp = "tp", t_fill
            break

    t_hit = t_tp if status == "tp" else (t_sl if status == "sl" else None)
    if status == "timeout" and censored:
        status = "censored"

    base.update({
        "status": "labeled",
        "outcome": status,                       # tp | sl | timeout | censored
        "censored": bool(censored),
        "entry_t": t_entry,
        "entry_eff": entry_eff,
        "qty": qty,
        "regime": regime_name,
        "mfe_bp": round(float(mfe_bp), 2),
        "mae_bp": round(float(mae_bp), 2),
        "last_bp": round(float(last_bp), 2),
        "t_to_hit_ms": (int(t_hit - t_entry) if t_hit is not None else None),
        "observed_ms": int(min(obs_ms, max(0, t_last - t_entry))),
        "n_marks": n_marks,
        "costs": {"fee_sol": float(qb.get("fee_sol", 0.0)),
                  "slippage_sol": float(qb.get("slippage_sol", 0.0)),
                  "priority_sol": float(qb.get("priority_sol", 0.0))},
    })
    return base


t0 = time.time()
n = 0
outcome_counter = collections.Counter()
with open(OUT, "a", encoding="utf-8") as fh:
    with mp.Pool(WORKERS) as pool:
        for r in pool.imap_unordered(work, todo, chunksize=8):
            fh.write(json.dumps(r) + "\n")
            n += 1
            outcome_counter[r.get("outcome") or r.get("status")] += 1
            if n % 2000 == 0:
                fh.flush()
                print(json.dumps({"done": n, "of": len(todo),
                                  "rate_s": round((time.time() - t0) / n, 3),
                                  "outcomes": dict(outcome_counter)}), flush=True)
print(json.dumps({"rows": n, "out": OUT, "outcomes": dict(outcome_counter)}), flush=True)
