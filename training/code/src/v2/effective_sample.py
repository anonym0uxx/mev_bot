#!/usr/bin/env python
"""What does the below-band budget actually buy? Measures effective sample size
and label persistence (redundancy) for candidate_final, and confirms the action space."""
import json, collections, math, sys

CAND = sys.argv[1] if len(sys.argv) > 1 else "candidate_final"
SEQLEN = 12288

per_mint = collections.Counter()
acts = collections.Counter()
mint_actions = collections.defaultdict(list)
tok_ctx = 0
recs = 0

with open(f"{CAND}/train.jsonl") as f:
    for line in f:
        r = json.loads(line)
        recs += 1
        a = r["meta"].get("action")
        acts[a] += 1
        if a:
            per_mint[r["mint"]] += 1
            mint_actions[r["mint"]].append(a)

n_mints = len(per_mint)
counts = list(per_mint.values())

# label persistence: P(action_t == action_{t-1}) within a mint
same = 0; tot = 0
for m, seq in mint_actions.items():
    for i in range(1, len(seq)):
        tot += 1
        if seq[i] == seq[i-1]:
            same += 1

persist = same / tot if tot else 0.0
eff_indep = n_mints                      # one independent market situation per mint
eff_nav = 1 + (1 - persist) * (tot)      # crude: innovation = label changes
print(json.dumps({
    "train_records": recs,
    "decision_records": sum(acts[k] for k in acts if k),
    "action_space": sorted([k for k in acts if k]),
    "null_action_records": acts.get(None, 0),
    "mints_with_decisions": n_mints,
    "records_per_mint_mean": round(sum(counts)/max(n_mints,1), 1),
    "records_per_mint_max": max(counts) if counts else 0,
    "label_persistence_within_mint": round(persist, 4),
    "adjacent_tick_pairs": tot,
    "effective_independent_episodes": eff_indep,
    "effective_innovations": int(eff_nav),
    "seq_len": SEQLEN,
}, indent=1))
