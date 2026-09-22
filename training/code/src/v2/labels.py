#!/usr/bin/env python
"""Stage 4 — outcome/label derivation.

Reads <ledger>/states.jsonl, attaches for each episode:
  truth_type, label_status, action (BUY/WATCH/SKIP), panel_no_buy flag,
  and a deterministic mechanics rationale (operands only, no future numbers).

Threshold-free facts (censoring, executability) come from the outcome block.
The two action thresholds are FITTED ON TRAIN ONLY and frozen here + in
reports/EXECUTION_CALIBRATION.json.

Outputs:
  <out>/labels.jsonl
  reports/EXECUTION_CALIBRATION.json
  reports/LABEL_COVERAGE.json
"""
import argparse, json, sys, os
import numpy as np

PANEL_MS = 300_000          # 5-minute panel bucket for NO_BUY supervision
TARGET_BUY_RATE = 0.20      # top 20% of train net_bp -> BUY
TARGET_SKIP_RATE = 0.35     # bottom 35% of train net_bp -> SKIP


def load(path):
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                yield json.loads(line)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--states", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    os.makedirs("reports", exist_ok=True)

    # ---- pass 1: fit thresholds on TRAIN only ----
    train_net = []
    n_seen = 0
    for r in load(a.states):
        n_seen += 1
        oe = r["outcome_evidence"]
        if r["identity"]["split"] != "train":
            continue
        if oe["right_censored_300s"] or not oe["exit_executable"]:
            continue
        v = oe["net_bp_300s"]
        if v is not None and np.isfinite(v):
            train_net.append(v)
    if n_seen == 0:
        raise SystemExit("FATAL: zero episodes read")
    if len(train_net) < 100:
        raise SystemExit(f"FATAL: only {len(train_net)} usable train labels — refusing")
    tn = np.asarray(train_net, dtype=np.float64)
    buy_thr = float(np.quantile(tn, 1.0 - TARGET_BUY_RATE))
    skip_thr = float(np.quantile(tn, TARGET_SKIP_RATE))

    # ---- pass 2: label ----
    os.makedirs(a.out, exist_ok=True)
    fp = f"{a.out}/labels.jsonl"
    f = open(fp, "w", buffering=1 << 20)
    counts = {"BUY": 0, "WATCH": 0, "SKIP": 0}
    status_c = {"supported": 0, "censored": 0, "unsupported": 0}
    truth_c = {}
    per_split = {"train": {"BUY": 0, "WATCH": 0, "SKIP": 0}, "val": {"BUY": 0, "WATCH": 0, "SKIP": 0},
                 "test": {"BUY": 0, "WATCH": 0, "SKIP": 0}}
    panels = {}
    n_lab = 0
    for r in load(a.states):
        oe = r["outcome_evidence"]
        censored = bool(oe["right_censored_300s"])
        executable = bool(oe["exit_executable"])
        net = oe["net_bp_300s"]
        if censored:
            truth = "unknown_or_censored"; status = "censored"; action = None
        elif not executable:
            truth = "observed_market_path"; status = "unsupported"; action = None
        else:
            truth = "modeled_action_value"; status = "supported"
            if net is None or not np.isfinite(net):
                action = None
            elif net >= buy_thr:
                action = "BUY"
            elif net <= skip_thr:
                action = "SKIP"
            else:
                action = "WATCH"
        status_c[status] += 1
        truth_c[truth] = truth_c.get(truth, 0) + 1
        if action:
            counts[action] += 1
            sp = r["identity"]["split"]
            if sp in per_split:
                per_split[sp][action] += 1
        # panel bucket for NO_BUY supervision
        b = r["decision_clock"]["t_dec_ms"] // PANEL_MS
        panels.setdefault(b, []).append({"episode_id": r["identity"]["episode_id"], "action": action})

        s = r["state"]
        rat = {
            "origin": "deterministic_mechanics",
            "operands": {
                "cost_model_bp": 210.0,
                "price_sol_per_raw": s["price_sol_per_raw"],
                "n_prior_trades": s["n_trades_prior"],
                "buy_sell": [s["buy_count"], s["sell_count"]],
                "net_flow_lamports": s["net_flow_lamports"],
                "trader_concentration_top1": s["top1_trader_share"],
                "last_trade_age_s": s["last_trade_age_s"],
            },
        }
        rec = {
            "episode_id": r["identity"]["episode_id"],
            "mint": r["identity"]["mint"],
            "split": r["identity"]["split"],
            "task": "opportunity_decision",
            "truth_type": truth,
            "label_status": status,
            "action": action,
            "panel_bucket": b,
            "rationale_evidence": rat,
            "net_bp_300s": net,
        }
        f.write(json.dumps(rec, separators=(",", ":")) + "\n")
        n_lab += 1
    f.close()

    no_buy_panels = sum(1 for v in panels.values()
                        if len(v) >= 3 and all(x["action"] != "BUY" for x in v))
    calib = {
        "schema": "north_star_execution_calibration_v2",
        "cost_model": {"pump_fee_bps": 100, "slippage_bps": 50, "priority_fee_bps": 8,
                       "failure_rate": 0.10, "failed_cost_bps": 220,
                       "round_trip_cost_bp": 210.0},
        "buy_threshold_bp": round(buy_thr, 2),
        "skip_threshold_bp": round(skip_thr, 2),
        "fitted_on": "train split, supported+uncensored only",
        "train_label_n": len(train_net),
        "target_buy_rate": TARGET_BUY_RATE, "target_skip_rate": TARGET_SKIP_RATE,
        "realized_buy_rate": round(counts["BUY"] / max(n_lab, 1), 4),
        "thresholds_sha256": _h([round(buy_thr, 4), round(skip_thr, 4)]),
    }
    coverage = {
        "schema": "north_star_label_coverage_v2",
        "episodes_read": n_seen, "labels_written": n_lab,
        "action_counts": counts,
        "action_counts_by_split": per_split,
        "label_status": status_c,
        "truth_type": truth_c,
        "panels_total": len(panels),
        "panels_no_buy_eligible": no_buy_panels,
    }
    json.dump(calib, open("reports/EXECUTION_CALIBRATION.json", "w"), indent=1)
    json.dump(coverage, open("reports/LABEL_COVERAGE.json", "w"), indent=1)
    print(f"[LABELS] read={n_seen:,} written={n_lab:,}")
    print(f"[LABELS] thresholds buy>={buy_thr:.1f}bp  skip<={skip_thr:.1f}bp (train-fitted, n={len(train_net):,})")
    print(f"[LABELS] actions {counts}  by_split={per_split}")
    print(f"[LABELS] status {status_c}")
    print(f"[LABELS] truth {truth_c}")
    print(f"[LABELS] panels={len(panels):,} no_buy_eligible={no_buy_panels:,}")
    return 0


def _h(o):
    import hashlib
    return hashlib.sha256(json.dumps(o, sort_keys=True).encode()).hexdigest()


if __name__ == "__main__":
    sys.exit(main())
