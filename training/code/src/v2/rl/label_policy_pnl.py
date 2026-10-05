#!/usr/bin/env python
"""Realized-P&L replay of the label policy - the test the 14 gates do not perform.

Every c10 gate is a consistency or structure gate; none asks whether acting on these labels
makes money. This does: for each entry decision take the action the family teaches, apply
the barrier outcome that actually followed, charge the cost the authority states, aggregate.

WHAT IT IS AND IS NOT. The action labels are the engine's argmax under a causal state; the
barrier outcomes are what the forward path did. So this is "if the brain executed its
labelled action every time, with fills at the recorded prices, what would the outcome
distribution have been" - a ceiling, not a live expectation. It is still the right first
test: a corpus whose own labels lose money on ex-post outcomes cannot be the best possible
dataset for profitability, and no gate would have said so.

`--rust-booking` is a SENSITIVITY, not a data change: it prices the same decisions under the
Rust executor's bookkeeping constants to size the execution-layer divergence. Nothing about
it is written into the corpus.
"""
import argparse
import collections
import json
import os
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import cost_authority as CA  # noqa: E402

ENTRY = "/training/v2/candidate_sft_c10_entry_labeled"
CLIP_BY_TIER = {"SMALL": 0.25, "MID": 0.50, "FULL": 1.00}
FIXED_LAMPORTS_BY_VENUE = {"pumpfun": 25_000, "pumpswap": 5_500}


def size_key(m):
    sd = m.get("size_dimension") or {}
    s = sd.get("size")
    return s if s in CLIP_BY_TIER else "FULL"


def replay(dirpath, splits, venue_curve, venue_amm, priority_lamports, label):
    st = collections.Counter()
    allpnl, buy = [], []
    by_regime = collections.defaultdict(list)
    outcomes = collections.Counter()
    for split in splits:
        p = f"{dirpath}/{split}.jsonl"
        if not os.path.exists(p):
            continue
        for line in open(p):
            r = json.loads(line)
            m = r["meta"]
            act = m.get("action")
            bt = m.get("barrier_triplet") or {}
            ca = m.get("cost_authority") or {}
            # the entry family's vocabulary is supported / unpriced_refused_no_reserves
            if not ca:
                st["skipped_no_authority"] += 1
                continue
            ls = m.get("label_status")
            if ls not in (None, "supported"):
                st["skipped_" + str(ls)] += 1
                continue
            st["rows"] += 1
            if act != "BUY":
                allpnl.append(0.0)
                st[f"no_trade_{act}"] += 1
                continue
            if bt.get("status") != "labeled":
                st["skipped_unlabeled_barrier"] += 1
                continue
            depth = ca.get("depth_sol")
            regime = ca.get("regime", "bonding_curve")
            if not depth:
                st["skipped_no_depth"] += 1
                continue
            clip = CLIP_BY_TIER[size_key(m)]
            d = CA.decompose(clip, depth, regime, uniform_venue_fee=False)
            cur_v = d["venue_bp_per_leg"]
            new_v = venue_curve if regime == "bonding_curve" else venue_amm
            fixed_leg = d["fixed_tx_bp_per_leg"]
            venue_name = "pumpfun" if regime == "bonding_curve" else "pumpswap"
            prio_leg = priority_lamports / 1e9 / clip * 1e4 if priority_lamports else \
                FIXED_LAMPORTS_BY_VENUE[venue_name] / 1e9 / clip * 1e4
            rt = 2.0 * (new_v + prio_leg) + 2.0 * d["impact_bp_per_leg"]
            out = bt.get("outcome")
            outcomes[f"{regime}:{out}"] += 1
            if out == "tp":
                gross = bt["tp_bp"]          # the limit the policy would have exited at
            elif out == "sl":
                gross = -bt["sl_bp"]         # the stop it would have honoured
            else:                            # timeout / censored -> mark to the last print
                gross = bt.get("last_bp") or 0.0
            net = gross - rt
            buy.append(net)
            allpnl.append(net)
            by_regime[regime].append(net)
            st["trades"] += 1
            if net > 0:
                st["wins"] += 1
            st["cost_bp_sum"] += rt
            del cur_v, fixed_leg
    traded = len(buy)
    return {
        "case": label, "rows": st["rows"], "trades": traded,
        "skips": st["no_trade_SKIP"], "watches": st["no_trade_WATCH"],
        "win_rate_pct": round(100.0 * st["wins"] / traded, 2) if traded else None,
        "mean_net_bp": round(statistics.fmean(buy), 2) if traded else None,
        "median_net_bp": round(statistics.median(buy), 2) if traded else None,
        "mean_cost_bp": round(st["cost_bp_sum"] / traded, 1) if traded else None,
        "mean_bp_over_all_rows": round(statistics.fmean(allpnl), 2) if allpnl else None,
        "outcomes": dict(sorted(outcomes.items())),
        "by_regime_mean_bp": {k: round(statistics.fmean(v), 2) for k, v in by_regime.items()},
        "skipped": {k: v for k, v in st.items() if k.startswith("skipped_")},
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default=ENTRY)
    ap.add_argument("--splits", nargs="+",
                    default=["train", "validation", "examination"])
    ap.add_argument("--venue-bp-curve", type=float, default=94.63)
    ap.add_argument("--venue-bp-amm", type=float, default=30.0)
    ap.add_argument("--priority-lamports", type=float, default=0.0,
                    help="0 = use the family's own measured per-venue figures")
    ap.add_argument("--rust-booking", action="store_true",
                    help="sensitivity: 125 bp/side and 150k lamports/leg")
    args = ap.parse_args()
    cases = [replay(args.dir, args.splits, args.venue_bp_curve, args.venue_bp_amm,
                    args.priority_lamports, "corpus_authority")]
    if args.rust_booking:
        cases.append(replay(args.dir, args.splits, 125.0, 30.0, 150_000.0,
                            "rust_booking_sensitivity"))
    json.dump({"entry_family_replay": cases,
               "note": "barrier outcomes are the family's own labelled triplet"},
              open("/training/v2/reports/C10_PNL_REPLAY.json", "w"), indent=1)
    for c in cases:
        print(json.dumps(c, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())