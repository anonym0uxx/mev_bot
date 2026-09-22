#!/usr/bin/env python
"""Candidate guard — runs BEFORE the candidate is admitted (EVALUATION_CONTRACT §4).

1. Outcome-leak: does any realized-forward number leak into the supervised span
   (assistant) or into the model input (user/system)?
2. Derivability: can a linear model recover the ACTION token from the input
   numerics alone? Near-0.5 AUC = good; >0.9 = the target is trivially derivable.
3. Class balance of the action token.
"""
import argparse, json, re, sys
import numpy as np

NUM = re.compile(r"([a-z0-9_]+)=(-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)")
ACT = {"BUY": 0, "WATCH": 1, "SKIP": 2, "HOLD": 3, "ADD": 4, "REDUCE": 5, "EXIT": 6}

# management prompts use "<label>: <number>" instead of key=value
NUM2 = re.compile(r"([A-Za-z][A-Za-z0-9_ ]{1,40}):\s*(-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)")


def parse_user(text):
    d = {}
    for k, v in NUM.findall(text):
        try:
            d[k] = float(v)
        except ValueError:
            pass
    for k, v in NUM2.findall(text):
        key = k.strip().lower().replace(" ", "_")
        try:
            d.setdefault(key, float(v))
        except ValueError:
            pass
    return d


def logistic(X, y, epochs=400, lr=0.5, l2=1e-3):
    n, p = X.shape
    X = np.hstack([np.ones((n, 1)), X])
    w = np.zeros(X.shape[1])
    for _ in range(epochs):
        z = X @ w
        pr = 1 / (1 + np.exp(-np.clip(z, -30, 30)))
        g = X.T @ (pr - y) / n + l2 * w
        w -= lr * g
    return w


def auc(score, y):
    pos = score[y == 1]; neg = score[y == 0]
    if pos.size == 0 or neg.size == 0:
        return float("nan")
    order = np.argsort(np.concatenate([pos, neg]))
    ranks = np.empty_like(order); ranks[order] = np.arange(1, order.size + 1)
    rp = ranks[:pos.size].sum()
    return (rp - pos.size * (pos.size + 1) / 2) / (pos.size * neg.size)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidate", default="candidate")
    a = ap.parse_args()

    rows = []
    with open(f"{a.candidate}/train.jsonl") as f:
        for line in f:
            if line.strip():
                rows.append(json.loads(line))
    if len(rows) < 100:
        raise SystemExit(f"FATAL: only {len(rows)} records — refusing to report clean")

    # ---- 1. outcome leakage into supervised/input text ----
    leak_hits = 0
    checked = 0
    feats = []
    labels = []
    for r in rows:
        user = next(m["content"] for m in r["messages"] if m["role"] == "user")
        asst = next(m["content"] for m in r["messages"] if m["role"] == "assistant")
        # forbidden tokens anywhere
        for bad in ("net_bp_300s", "right_censored", "mfe_bp", "mae_bp",
                    "gross_ret_300s", "economic_class", "seconds_to_graduation",
                    "graduation_proximity"):
            if bad in user or bad in asst:
                leak_hits += 1
                break
        checked += 1
        if r.get("meta", {}).get("action") not in ACT:
            continue
        d = parse_user(user)
        feats.append(d)
        labels.append(ACT[r["meta"]["action"]])

    keys = sorted({k for d in feats for k in d})
    X = np.array([[d.get(k, 0.0) for k in keys] for d in feats], dtype=np.float64)
    X = np.nan_to_num(X)
    mu = X.mean(0); sd = X.std(0); sd[sd == 0] = 1
    Xs = (X - mu) / sd
    y = np.array(labels)

    results = {}
    # multiclass one-vs-rest
    for c, name in ACT.items():
        yy = (y == name).astype(float)
        w = logistic(Xs, yy)
        s = np.hstack([np.ones((Xs.shape[0], 1)), Xs]) @ w
        results[name] = {"ovr_auc": round(float(auc(s, yy)), 4),
                         "pos_rate": round(float(yy.mean()), 4)}
    # multi-class accuracy via stacked scores
    W = np.column_stack([logistic(Xs, (y == c).astype(float)) for c in ACT.values()])
    S = np.hstack([np.ones((Xs.shape[0], 1)), Xs]) @ W
    pred = S.argmax(1)
    acc = float((pred == y).mean())
    maj = float(np.bincount(y).max() / y.size)

    out = {
        "records": len(rows),
        "outcome_leak_records": leak_hits,
        "outcome_leak_rate": round(leak_hits / max(checked, 1), 6),
        "input_numeric_features": len(keys),
        "derivability_ovr_auc": results,
        "multiclass_accuracy": round(acc, 4),
        "majority_baseline": round(maj, 4),
        "verdict": ("FAIL: outcome leak" if leak_hits else
                    ("FAIL: action derivable" if max(v["ovr_auc"] for v in results.values()) > 0.9
                     else "PASS")),
    }
    json.dump(out, open("reports/CANDIDATE_GUARD.json", "w"), indent=1)
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
