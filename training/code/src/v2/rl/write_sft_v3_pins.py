"""Write the SFT V3 pin chain: dataset manifest -> launch contract -> unit script.

Level 1  /training/rel/SFT_DATASET_V3.json      (c5 paths + shas + rows + bytes)
Level 2  /training/code/qwen27b/LAUNCH_CONTRACT_SFT_V3.json  (references L1 + its sha)
Level 3  /training/code/qwen27b/run_sft005_unit.sh           (passes L2's sha)

Nothing is mutated in place: V2 files are read only.
Run id is sft-005; run dir /training/runs/sft-005.
"""
import hashlib
import json
import os
import stat
import sys

sys.path.insert(0, "/training/code/qwen27b")
C5 = "/training/v2/candidate_sft_c5"
V2_DATASET = "/training/rel/SFT_DATASET_V2.json"
V2_CONTRACT = "/training/code/qwen27b/LAUNCH_CONTRACT_SFT_V2.json"
V3_DATASET = "/training/rel/SFT_DATASET_V3.json"
V3_CONTRACT = "/training/code/qwen27b/LAUNCH_CONTRACT_SFT_V3.json"
V3_UNIT = "/training/code/qwen27b/run_sft005_unit.sh"
RUN_ID = "sft-005"
RUN_DIR = "/training/runs/sft-005"


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 22), b""):
            h.update(chunk)
    return h.hexdigest()


# ---- level 1 -------------------------------------------------------------
v2 = json.load(open(V2_DATASET, encoding="utf-8"))
splits = {}
for split in ("train", "validation", "examination"):
    p = os.path.join(C5, f"{split}.jsonl")
    n = 0
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for line in f:
            n += 1
            h.update(line)
    splits[split] = {"path": p, "rows": n, "bytes": os.path.getsize(p),
                     "sha256": h.hexdigest()}

# task_targets must equal the bucket set actually present in the train partition
import release_data  # noqa: E402

buckets = set()
with open(splits["train"]["path"], encoding="utf-8") as f:
    for line in f:
        buckets.add(release_data.task_bucket(json.loads(line)))
print("train buckets:", sorted(buckets), "| v2 task_targets:", sorted(v2["task_targets"]))
assert buckets == set(v2["task_targets"]), "c5 train bucket set differs from V2 task_targets"

v3 = {
    "schema": "north_star_sft_dataset_v1",
    "phase": "sft",
    "split_policy": v2["split_policy"],
    "task_targets": v2["task_targets"],
    "supersedes": "SFT_DATASET_V2.json (candidate_sft_c3)",
    "corpus": {
        "dir": C5,
        "rule": "SOL-only pump.fun memecoins (c4) AND decision time strictly before the D4 forward wall",
        "parent": "/training/v2/candidate_sft_c4",
        "wall": "/training/v2/reports/FORWARD_WALL_V1.json",
        "wall_ms": 1788984256517,
        "note": "rows are copied byte-for-byte from c4/c3; labels never rewritten, retargeted or padded",
    },
    "train": [splits["train"]],
    "validation": [splits["validation"]],
    "examination": [splits["examination"]],
}
json.dump(v3, open(V3_DATASET, "w", encoding="utf-8"), indent=1)
ds_sha = sha(V3_DATASET)
print(json.dumps({"wrote": V3_DATASET, "sha256": ds_sha,
                  "rows": {k: splits[k]["rows"] for k in splits}}, indent=1))

# ---- level 2 -------------------------------------------------------------
c = json.load(open(V2_CONTRACT, encoding="utf-8"))
c["run_id"] = RUN_ID
c["run_dir"] = RUN_DIR
c["sft_dataset"] = {"path": V3_DATASET, "sha256": ds_sha}
json.dump(c, open(V3_CONTRACT, "w", encoding="utf-8"), indent=2)
ct_sha = sha(V3_CONTRACT)
print(json.dumps({"wrote": V3_CONTRACT, "sha256": ct_sha}, indent=1))

# ---- level 3 -------------------------------------------------------------
unit = f"""#!/usr/bin/env bash
# sft-005 unit: launch the pinned contract, then HOLD the unit open while the
# trainer lives so systemd's cgroup never reaps the spawned ranks.
set -u
cd /training/code/qwen27b
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True
/home/alon/qwen27b-venv/bin/python launch_native.py \\
  --phase sft \\
  --release_manifest /training/rel/CANDIDATE_RELEASE_LINUX.json \\
  --launch-contract /training/code/qwen27b/LAUNCH_CONTRACT_SFT_V3.json \\
  --contract-sha256 "{ct_sha}" \\
  --execute
ec=$?
if [ $ec -ne 0 ]; then echo "launcher exited $ec; not holding unit" >&2; exit $ec; fi
sleep 20
while pgrep -f 'train_qwen27b\\.py --phase sft' >/dev/null 2>&1; do sleep 60; done
echo "trainer processes gone; unit exiting"
exit 0
"""
with open(V3_UNIT, "w", encoding="utf-8") as f:
    f.write(unit)
os.chmod(V3_UNIT, os.stat(V3_UNIT).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
print("wrote", V3_UNIT)
