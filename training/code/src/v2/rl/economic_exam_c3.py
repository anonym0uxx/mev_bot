#!/usr/bin/env python
"""C3 REBUILD - FROZEN ECONOMIC EXAM on the CLEAN forward holdout.

Supersedes code/src/v2/economic_exam.py, whose seeds came from the PRE-C3 ledger
chronological split ("test"). That split is no longer the holdout: the C3
re-partition moved it to /training/v2/candidate_sft_c3/examination.jsonl
(the frozen test-split mints PLUS the reserved chronological cohort).

This exam scores policies EXCLUSIVELY on BUY episodes whose episode_id is
present in the examination partition. No train/validation record is examined.

Frozen + deterministic:
  * np.random.seed(7), no wall-clock, no set/dict iteration order in the result
  * seeds are sorted deterministically before scoring
  * --freeze emits reports/EXAM_FREEZE_C3.json pinning the code sha256, the
    seed-set sha256 and the bar, so the number cannot drift silently.

Accounting engine and policy set are byte-for-byte the same as the superseded
exam (CAPITAL=1.0 SOL episode notional, 0.5 SOL entry tranche, HALF_COST_BP=180,
100x implausible-move cap), so the C3 bar is directly comparable to the old
0.4725 SOL rule_base bar. The entry TRANCHE is a strategy parameter, not a
notional: CAPITAL is the canonical trade size and is asserted against
exit_mechanics.DEPLOY_SOL_CANONICAL. Re-running at a 1.0 tranche instead of 0.5
moves this bar 0.46081 -> 0.70280 (+52%) with the seed set and partition
byte-identical, which is why --verify-freeze now refuses on any pinned change.
"""
import argparse, hashlib, json, os, sys, collections, datetime
import numpy as np
import pyarrow.json as paj

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(os.path.dirname(HERE)))          # .../src
sys.path.insert(0, os.path.dirname(HERE))                            # .../src/v2
from build_replay_v2 import (load_mint_series, score_actions,
                             HALF_COST_BP, ADD_FRACTION)

EXAM_JSONL = "/training/v2/candidate_sft_c3/examination.jsonl"
LEDGER_LABELS = "/training/v2/canonical/ledger_v7/labels.jsonl"
LEDGER_STATES = "/training/v2/canonical/ledger_v7/states.jsonl"

from exit_mechanics import DEPLOY_SOL_CANONICAL as _DEPLOY_CANON  # noqa: E402

CAPITAL = _DEPLOY_CANON   # episode notional == the canonical trade size
# Entry TRANCHE within that episode, NOT a notional: the remaining capital is the
# add reserve. Conflating the two is what silently moved this frozen bar 52%
# (0.46081 at tranche 0.5 -> 0.70280 at tranche 1.0) with seed set and partition
# byte-identical. Frozen at 0.5 - the value the archived bar and the live c6
# management-replay labels were both built with.
ENTRY_TRANCHE_SOL = 0.5
assert CAPITAL == _DEPLOY_CANON
POLICIES = ["always_hold", "always_exit", "random", "rule_base", "oracle_tick"]


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def load_exam_episode_ids():
    """episode_ids that physically exist in the examination partition."""
    t = paj.read_json(EXAM_JSONL)
    return set(t.column("episode_id").to_pylist())


def load_exam_seeds(exam_ids):
    """BUY episodes (from the canonical ledger) whose episode_id is in the
    examination partition. Deterministically ordered."""
    lab = {}
    with open(LEDGER_LABELS) as f:
        for line in f:
            r = json.loads(line)
            lab[r["episode_id"]] = r
    seeds = []
    with open(LEDGER_STATES) as f:
        for line in f:
            r = json.loads(line)
            ident = r["identity"]
            eid = ident["episode_id"]
            lb = lab.get(eid)
            if not lb or lb.get("action") != "BUY":
                continue
            if eid not in exam_ids:
                continue
            seeds.append((ident["mint"], ident["split"],
                          int(r["decision_clock"]["t_dec_ms"]), eid))
    seeds.sort(key=lambda s: (s[1], s[0], s[2], s[3]))
    return seeds


def schedule(tt, pv, t_dec):
    j0 = int(np.searchsorted(tt, t_dec, side="right"))
    if j0 >= tt.size:
        return None
    entry_px = pv[j0]
    if not np.isfinite(entry_px) or entry_px <= 0:
        return None
    t_entry = int(tt[j0])
    end = min(t_entry + 1800_000, int(tt[-1]))
    if end <= t_entry:
        return None
    ticks = np.arange(t_entry + 30_000, end + 1, 30_000, dtype=np.int64)
    if ticks.size == 0:
        return None
    npri = np.searchsorted(tt, ticks, side="left")
    act = (ticks - tt[npri - 1]) <= 60_000
    ticks, npri = ticks[act], npri[act]
    if ticks.size == 0:
        return None
    sel = (np.arange(min(16, ticks.size)) * (ticks.size / min(16, ticks.size))).astype(int)
    return entry_px, t_entry, ticks[sel], npri[sel], j0


def fwd(tt, pv, tM, i, j0):
    s = []
    for hs in (60, 150, 300):
        jh = int(np.searchsorted(tt, tM + hs * 1000, side="right"))
        seg = pv[max(i, j0):jh]
        seg = seg[np.isfinite(seg)]
        if seg.size:
            s.append(float(seg[-1]))
    if not s:
        return None
    return float(np.mean(s)), float(np.std(s))


def run_policy(pol, tt, pv, j0, entry_px, t_entry, ticks, npri):
    ow = HALF_COST_BP / 1e4
    qty = ENTRY_TRANCHE_SOL / entry_px
    cash = CAPITAL - ENTRY_TRANCHE_SOL
    for k in range(ticks.size):
        tM, i = int(ticks[k]), int(npri[k])
        if i <= j0 or (tM - t_entry) < 60_000:
            continue
        cur = pv[i - 1]
        if not np.isfinite(cur) or cur <= 0:
            continue
        f = fwd(tt, pv, tM, i, j0)
        if f is None:
            continue
        fm, fs = f
        if pol == "rule_base":
            act = score_actions(qty, float(cur), fm, fs, cash)[0]
        elif pol == "always_hold":
            act = "HOLD"
        elif pol == "always_exit":
            act = "EXIT"
        elif pol == "random":
            act = ["HOLD", "ADD", "REDUCE", "EXIT"][int(np.random.randint(0, 4))]
        elif pol == "oracle_tick":
            u = {"HOLD": qty * fm * (1 - ow), "EXIT": qty * cur * (1 - ow),
                 "REDUCE": 0.5 * qty * cur * (1 - ow) + 0.5 * qty * fm * (1 - ow)}
            spend = qty * ADD_FRACTION * cur * (1 + ow)
            u["ADD"] = (qty * (1 + ADD_FRACTION) * fm * (1 - ow) - spend) if spend <= cash + 1e-9 else -np.inf
            act = max(u, key=lambda kk: u[kk])
        else:
            act = "HOLD"
        if act == "ADD":
            aq = qty * ADD_FRACTION
            sp = aq * cur * (1 + ow)
            if sp <= cash + 1e-12:
                cash -= sp; qty += aq
            else:
                act = "HOLD"
        elif act == "REDUCE":
            q = 0.5 * qty
            cash += q * cur * (1 - ow); qty -= q
        elif act == "EXIT":
            cash += qty * cur * (1 - ow); qty = 0.0
        if qty <= 1e-12:
            break
    mark = pv[npri[-1] - 1] if npri[-1] > 0 else entry_px
    if not np.isfinite(mark):
        mark = entry_px
    return cash + qty * float(mark)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="/training/v2/reports/ECONOMIC_EXAM_C3.json")
    ap.add_argument("--freeze-out", default="/training/v2/reports/EXAM_FREEZE_C3.json")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--max-move", type=float, default=100.0)
    ap.add_argument("--verify-freeze", action="store_true",
                    help="(default) verify the pinned freeze and refuse on ANY "
                         "change to code/driver/partition/seedset/accounting/bar")
    ap.add_argument("--freeze", action="store_true",
                    help="intentionally re-pin the freeze artifact. Requires "
                         "--justification; recorded in the artifact.")
    ap.add_argument("--justification", default="",
                    help="why the freeze is being re-pinned (required with --freeze)")
    a = ap.parse_args()
    if a.freeze and not a.justification:
        ap.error("--freeze requires --justification: a re-pin must state why")
    np.random.seed(7)

    exam_ids = load_exam_episode_ids()
    seeds = load_exam_seeds(exam_ids)
    if a.limit:
        seeds = seeds[:a.limit]
    print(f"[C3-EXAM] partition={EXAM_JSONL}")
    print(f"[C3-EXAM] partition_rows={len(exam_ids):,} seeds(BUY in partition)={len(seeds):,}")

    names, bounds, allt, allpx = load_mint_series()
    idx = {n: i for i, n in enumerate(names)}
    n_excl = n_used = 0
    wealth = {p: [] for p in POLICIES}
    used_mints = set()
    for mint, split, t_dec, eid in seeds:
        gi = idx.get(mint)
        if gi is None:
            continue
        lo, hi = bounds[gi], bounds[gi + 1]
        tt, pv = allt[lo:hi], allpx[lo:hi]
        s = schedule(tt, pv, t_dec)
        if s is None:
            continue
        entry_px, t_entry, ticks, npri, j0 = s
        jh = int(np.searchsorted(tt, min(t_entry + 1800_000, int(tt[-1])), side="right"))
        seg = pv[j0:jh]; seg = seg[np.isfinite(seg) & (seg > 0)]
        if seg.size == 0:
            continue
        if float(seg.max() / entry_px) > a.max_move:
            n_excl += 1
            continue
        n_used += 1
        used_mints.add(mint)
        for p in POLICIES:
            w = run_policy(p, tt, pv, j0, entry_px, t_entry, ticks, npri)
            if np.isfinite(w):
                wealth[p].append(w - CAPITAL)

    out = {}
    for p, w in wealth.items():
        w = np.array(w)
        if w.size == 0:
            out[p] = {"episodes": 0}
            continue
        out[p] = {"episodes": int(w.size),
                  "mean_net_sol": round(float(w.mean()), 5),
                  "median_net_sol": round(float(np.median(w)), 5),
                  "total_net_sol": round(float(w.sum()), 3),
                  "hit_rate": round(float((w > 0).mean()), 4),
                  "p90_net_sol": round(float(np.percentile(w, 90)), 5),
                  "p10_net_sol": round(float(np.percentile(w, 10)), 5)}

    report = {
        "exam": "C3_FROZEN_ECONOMIC_EXAM",
        "partition": EXAM_JSONL,
        "partition_rows": len(exam_ids),
        "partition_sha256": sha256_file(EXAM_JSONL),
        "seeds_requested": len(seeds),
        "episodes_examined": n_used,
        "mints_examined": len(used_mints),
        "excluded_implausible": n_excl,
        "max_move_cap": a.max_move,
        "capital_sol": CAPITAL,
        "entry_tranche_sol": ENTRY_TRANCHE_SOL,
        "half_cost_bp": HALF_COST_BP,
        "bar_policy": "rule_base",
        "bar_mean_net_sol": out.get("rule_base", {}).get("mean_net_sol"),
        "superseded_bar_mean_net_sol": 0.47251,
        "policies": out,
    }
    json.dump(report, open(a.out, "w"), indent=1)
    print(json.dumps(out, indent=1))
    print(f"\n[C3-EXAM] examined={n_used:,} mints={len(used_mints):,} "
          f"excluded_implausible={n_excl:,} (max_move={a.max_move:g}x)")
    if "rule_base" in out:
        print(f"[C3-EXAM] BAR TO BEAT: rule_base mean_net_sol="
              f"{out['rule_base']['mean_net_sol']:.6f} "
              f"median={out['rule_base']['median_net_sol']:.6f} "
              f"total={out['rule_base']['total_net_sol']:.3f}")

    seed_payload = json.dumps([list(s) for s in seeds], separators=(",", ":"))
    freeze = {
        "frozen": True,
        "exam": "C3_FROZEN_ECONOMIC_EXAM",
        "accounting": {
            "capital_sol": CAPITAL,
            "entry_tranche_sol": ENTRY_TRANCHE_SOL,
            "half_cost_bp": HALF_COST_BP,
            "note": ("capital is the canonical trade notional (exit_mechanics."
                     "DEPLOY_SOL_CANONICAL); entry_tranche_sol is a STRATEGY "
                     "parameter, not a notional. The bar is only comparable "
                     "against a matching accounting block."),
        },
        "code_sha256": sha256_file(os.path.abspath(__file__)),
        "driver_sha256": sha256_file(os.path.join(os.path.dirname(HERE), "build_replay_v2.py")),
        "partition": EXAM_JSONL,
        "partition_sha256": report["partition_sha256"],
        "seedset_sha256": hashlib.sha256(seed_payload.encode()).hexdigest(),
        "episodes_examined": n_used,
        "mints_examined": len(used_mints),
        "bar_policy": "rule_base",
        "bar_mean_net_sol": report["bar_mean_net_sol"],
        "bar_median_net_sol": out.get("rule_base", {}).get("median_net_sol"),
        "reproduce": (
            "/home/alon/qwen27b-venv/bin/python "
            "/training/v2/code/src/v2/rl/economic_exam_c3.py"
        ),
        "random_seed": 7,
    }

    # A freeze is only a freeze if something REFUSES when it is violated. Nothing
    # read this file before, which is how an entry-tranche change moved the bar
    # 52% with the seed set and partition byte-identical. Default is now
    # verify-and-refuse; re-pinning is an explicit, justified act.
    if a.freeze:
        freeze["frozen_utc"] = datetime.datetime.now(
            datetime.timezone.utc).isoformat()
        freeze["justification"] = a.justification
        freeze["replaced_bar_mean_net_sol"] = (
            json.load(open(a.freeze_out)).get("bar_mean_net_sol")
            if os.path.exists(a.freeze_out) else None)
        json.dump(freeze, open(a.freeze_out, "w"), indent=1)
        print(f"[C3-EXAM] REPINNED bar -> {a.freeze_out}  "
              f"seedset_sha256={freeze['seedset_sha256'][:16]}... "
              f"code_sha256={freeze['code_sha256'][:16]}...")
        print(f"[C3-EXAM] justification: {a.justification}")
        return 0

    drift = []
    old = None
    if os.path.exists(a.freeze_out):
        old = json.load(open(a.freeze_out))
    if old is None:
        drift.append("no freeze artifact at %s - nothing to verify against" % a.freeze_out)
    else:
        for k in ("code_sha256", "driver_sha256", "partition_sha256",
                  "seedset_sha256", "episodes_examined", "mints_examined",
                  "bar_policy", "bar_mean_net_sol", "bar_median_net_sol"):
            if k not in old:
                continue
            if old[k] != freeze[k]:
                drift.append("%s: frozen=%r now=%r" % (k, old[k], freeze[k]))
        if old.get("accounting") is None:
            drift.append("frozen artifact predates the accounting block: it does not "
                         "pin capital vs entry tranche, so a re-valued bar could not "
                         "be detected (re-pin with --freeze --justification ...)")
    print("\n[C3-EXAM] FREEZE VERIFY:")
    if drift:
        for d in drift:
            print("   DRIFT %s" % d)
        print("[C3-EXAM] FREEZE VERIFY FAILED (%d) - the pinned bar is not "
              "reproducible from this code" % len(drift))
        return 2
    print("   PASS code/driver/partition/seedset/accounting/bar all match the "
          "frozen artifact")
    print("[C3-EXAM] FREEZE VERIFY PASS bar=%.5f" % freeze["bar_mean_net_sol"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
