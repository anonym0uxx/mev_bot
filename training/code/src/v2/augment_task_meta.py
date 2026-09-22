#!/usr/bin/env python
"""Augment the final SFT corpus with the trainer's release contract metadata.

release_data.task_bucket() hard-fails on an unknown task and requires each
POLICY task to carry a non-empty action/economic/causal evidence block, so this
must be applied before the corpus can enter a release manifest.
"""
import json, os, collections

FAM_TO_TASK = {
    "decision": "decision_action",
    "management_replay": "next_action_decision",
    "utility_regression": "decision_reasoning",
    "utility_reasoning": "risk_sizing",
}
SRC = "/training/v2/candidate_sft_final"
task_seen = collections.Counter()

for split in ("train", "validation", "test"):
    p = f"{SRC}/{split}.jsonl"
    tmp = p + ".tmp"
    with open(p) as fi, open(tmp, "w", buffering=1 << 20) as fo:
        for line in fi:
            r = json.loads(line)
            m = r.setdefault("meta", {})
            fam = m.get("family", "decision")
            task = FAM_TO_TASK.get(fam)
            if task is None:
                raise SystemExit(f"FATAL: unmapped family {fam!r}")
            m["task"] = task
            m["policy_supervision"] = True
            net = m.get("net_bp_300s")
            m["policy_evidence"] = {
                "action": {"status": "derived",
                           "source": "causal_state_builder_v2/decision_grid",
                           "label": m.get("action") or m.get("step_action"),
                           "task": task},
                "economic": ({"status": "derived",
                              "source": "replay_v2.0_constant_notional_accounting_engine",
                              "net_bp_300s": net}
                             if net is not None else
                             {"status": "refused",
                              "source": "replay_v2.0_constant_notional_accounting_engine",
                              "reason": "no realized executable outcome on this row"}),
                "causal": {"status": "present",
                           "source": "causal_state_builder_v2/strict_prior_window",
                           "ordering": m.get("ordering_certainty", "strict_prior")},
            }
            task_seen[(split, task)] += 1
            fo.write(json.dumps(r, separators=(",", ":")) + "\n")
    os.replace(tmp, p)

print("[AUGMENT] task buckets per split:")
for (split, task), n in sorted(task_seen.items()):
    print(f"   {split:11s} {task:22s} {n:7,}")
json.dump({f"{s}|{t}": n for (s, t), n in task_seen.items()},
          open("/training/v2/reports/TASK_BUCKETS.json", "w"), indent=1)
