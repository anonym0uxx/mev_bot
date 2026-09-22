#!/usr/bin/env python
"""Structural + labeling audit across every family that will go into c10.

Surgical cleanliness means the four families must be one corpus, not four corpora that
happen to sit in the same directory. This checks the things that break integration in
practice rather than the things that are easy to count:

  * schema      - identical required keys, identical message shape
  * vocabulary  - meta.family / meta.task / meta.split drawn from fixed sets
  * identity    - episode_id unique within (family, split); no exact duplicate rows
  * labeling    - the assistant's opening line matches the family's own contract
  * wiring      - every row carries the family's authority block
  * hygiene     - no empty fields, no non-finite numbers, no NaN in metadata

Reports per (family, split). Exit code 1 if any violation.
"""
import glob
import hashlib
import json
import math
import os
import sys

REQUIRED_KEYS = {"episode_id", "family", "messages", "meta", "mint", "split"}
SPLIT_VOCAB = {"train", "val", "test"}
EXPECTED = {
    "decision": "decision_action",
    "management_replay": "next_action_decision",
    "utility_regression": "decision_reasoning",
    "utility_reasoning": "utility_reasoning",
}
# The assistant's opening line is the family's own contract with the model.
OPENERS = {
    "decision": ("DECISION",),
    "management_replay": ("ACTION", "DECISION"),
    "utility_regression": ("FORECAST_NET_BP:", "FORECAST:"),
    "utility_reasoning": ("COST MODEL:",),
}
SOURCES = {
    "decision": "/training/v2/candidate_sft_c10_entry_labeled",
    "management_replay": "/training/v2/reports/mgmt_c10",
    "utility_reasoning": "/training/v2/candidate_sft_c10_utility_reasoning",
    "utility_regression": "/training/v2/candidate_sft_c10_utility_regression",
}


def finite(x):
    return isinstance(x, (int, float)) and not (isinstance(x, float) and not math.isfinite(x))


def walk_scalars(o, path=""):
    if isinstance(o, dict):
        for k, v in o.items():
            yield from walk_scalars(v, f"{path}.{k}")
    elif isinstance(o, list):
        for i, v in enumerate(o):
            yield from walk_scalars(v, f"{path}[{i}]")
    else:
        yield path, o


def audit(family, root, only_family=None):
    res = {}
    for split in ("train", "validation", "examination"):
        f = os.path.join(root, f"{split}.jsonl")
        if not os.path.exists(f):
            continue
        v = dict(rows=0, missing_keys=0, bad_shape=0, empty_content=0, wrong_family=0,
                 wrong_task=0, bad_split=0, dup_episode_id=0, dup_row=0, bad_opener=0,
                 no_authority=0, nonfinite=0)
        ids = set()
        hashes = set()
        with open(f) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                r = json.loads(line)
                m = r.get("meta", {})
                if only_family and m.get("family") != only_family:
                    continue
                v["rows"] += 1
                if not REQUIRED_KEYS <= set(r):
                    v["missing_keys"] += 1
                    continue
                msgs = r["messages"]
                if not (isinstance(msgs, list) and len(msgs) == 3
                        and [x.get("role") for x in msgs] == ["system", "user", "assistant"]):
                    v["bad_shape"] += 1
                    continue
                if any(not str(x.get("content", "")).strip() for x in msgs):
                    v["empty_content"] += 1
                fam = r.get("family") or m.get("family")
                if fam != family:
                    v["wrong_family"] += 1
                if m.get("task") != EXPECTED[family]:
                    v["wrong_task"] += 1
                if r.get("split") not in SPLIT_VOCAB:
                    v["bad_split"] += 1
                eid = r.get("episode_id")
                if family == "management_replay":
                    eid = (eid, m.get("step"))
                if eid in ids:
                    v["dup_episode_id"] += 1
                ids.add(eid)
                h = hashlib.sha1(("\u241f".join(x["content"] for x in msgs)).encode()).hexdigest()
                if h in hashes:
                    v["dup_row"] += 1
                hashes.add(h)
                first = msgs[2]["content"].lstrip().split("\n", 1)[0]
                if not any(first.startswith(p) for p in OPENERS[family]):
                    v["bad_opener"] += 1
                if "cost_authority" not in m:
                    v["no_authority"] += 1
                for p, val in walk_scalars(m):
                    if isinstance(val, float) and not math.isfinite(val):
                        v["nonfinite"] += 1
                        break
        res[split] = v
    return res


def main():
    total_viol = 0
    for family, root in SOURCES.items():
        only = None
        print(f"== {family}  ({os.path.relpath(root, '/training/v2')})")
        for split, v in audit(family, root, only).items():
            viol = sum(x for k, x in v.items() if isinstance(x, int) and k != "rows")
            total_viol += viol
            flag = "OK " if viol == 0 else "FAIL"
            print(f"   {flag} {split:<12} rows={v['rows']:<7} violations={viol}")
            if viol:
                for k, x in v.items():
                    if isinstance(x, int) and k != "rows" and x:
                        print(f"          {k}={x}")
    print()
    print("TOTAL_VIOLATIONS", total_viol)
    return 1 if total_viol else 0


if __name__ == "__main__":
    sys.exit(main())