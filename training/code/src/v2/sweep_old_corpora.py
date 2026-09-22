import json, collections, os

P = ("/mnt/data/mev_bot-artifacts/north_star/aggregation/north_star_training_dataset_v1/"
     "corpus_builder_v1/integrated_data_v1/full_history_closure_v1/economics_v1/"
     "astra_review_v1/revision_candidates")

def sweep(path, label, n=200000):
    if not os.path.exists(path):
        print(f"[MISSING] {path}"); return None
    acts = collections.Counter(); kinds = collections.Counter(); tot = 0
    with open(path) as f:
        for i, line in enumerate(f):
            if i >= n: break
            try: r = json.loads(line)
            except Exception: continue
            tot += 1
            m = r.get("meta") or {}
            acts[m.get("source_action") or m.get("action")] += 1
            kinds[m.get("kind")] += 1
    print(f"=== {label} ({path.split('/')[-1]}) rows<= {n} scanned={tot:,}")
    print(f"    kinds: {dict(kinds.most_common(6))}")
    print(f"    actions: {dict(acts.most_common(10))}")
    return {"file": path, "scanned": tot, "actions": dict(acts), "kinds": dict(kinds)}

out = {}
out["decision_sft_train"] = sweep(f"{P}/decision_sft/decision_sft_train.jsonl", "decision_sft_train")
out["review_sft_train"] = sweep(f"{P}/review_sft/review_sft_train.jsonl", "review_sft_train")

# counterfactual exit reasons (already known, re-confirm from the compact layer)
import glob
cf = sorted(glob.glob("/mnt/data/mev_bot-artifacts/gold/slinky_gold_v3_compact/counterfactual_trade_v3/*.parquet"))
print(f"\ncounterfactual_trade_v3 parts={len(cf)}")
json.dump(out, open("/training/v2/reports/SWEEP_old_corpora.json", "w"), indent=1)
