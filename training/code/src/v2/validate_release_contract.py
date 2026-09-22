import sys, json, collections
sys.path.insert(0, "/training/code/qwen27b")
from release_data import task_bucket

for split in ("train", "validation", "test"):
    cnt = collections.Counter()
    n = 0
    for line in open(f"/training/v2/candidate_sft_final/{split}.jsonl"):
        r = json.loads(line)
        cnt[task_bucket(r)] += 1
        n += 1
    print(f"[VALIDATE] {split}: rows={n:,} task_buckets={dict(cnt)}")
print("[VALIDATE] OK -- trainer's task_bucket() accepts every row")
