"""Build candidate_sft_c5 = c4 (SOL-only) minus the D4 forward wall.

Two independent filters, both applied, and their agreement is reported:
  (a) episode_id membership in /training/v2/forward_wall_v1/wall_episode_ids.txt
  (b) t_dec_ms parsed out of episode_id  >= wall_ms

LABEL INTEGRITY: lines are copied VERBATIM from c4. `messages` are never
re-serialized, rewritten or padded. c4 is opened READ-ONLY; c5 is a new dir.
"""
import hashlib
import json
import os

C4 = "/training/v2/candidate_sft_c4"
C5 = "/training/v2/candidate_sft_c5"
MANIFEST = "/training/v2/reports/SFT_C5_MANIFEST.json"
WALL_IDS = "/training/v2/forward_wall_v1/wall_episode_ids.txt"
WALL_REPORT = "/training/v2/reports/FORWARD_WALL_V1.json"

wall_report = json.load(open(WALL_REPORT, encoding="utf-8"))
wall_ms = wall_report["wall_ms"]
ids = set(l.strip() for l in open(WALL_IDS, encoding="utf-8") if l.strip())
print(json.dumps({"wall_ms": wall_ms, "wall_ids": len(ids)}), flush=True)

os.makedirs(C5, exist_ok=True)
manifest = {
    "schema": "sft_corpus_v5",
    "rule": "SOL-only pump.fun memecoins, D4 forward wall excluded from every split",
    "source": C4,
    "wall_ms": wall_ms,
    "wall_ids_file": WALL_IDS,
    "splits": {},
}

for split in ("train", "validation", "examination"):
    src = os.path.join(C4, f"{split}.jsonl")
    dst = os.path.join(C5, f"{split}.jsonl")
    h_src, h_dst = hashlib.sha256(), hashlib.sha256()
    n_in = n_out = by_id = by_time = 0
    disagree = 0
    with open(src, "r", encoding="utf-8") as fi, open(dst, "w", encoding="utf-8") as fo:
        for line in fi:
            n_in += 1
            h_src.update(line.encode("utf-8"))
            i = line.find('"episode_id":"')
            eid = ""
            if i >= 0:
                j = line.find('"', i + 14)
                eid = line[i + 14:j]
            a = eid in ids
            try:
                b = int(eid.rsplit(":", 1)[-1]) >= wall_ms
            except Exception:
                b = False
            if a != b:
                disagree += 1
            if a or b:
                by_id += int(a)
                by_time += int(b)
                continue
            fo.write(line)          # VERBATIM
            h_dst.update(line.encode("utf-8"))
            n_out += 1
    manifest["splits"][split] = {
        "rows_in": n_in, "rows_out": n_out, "rows_dropped": n_in - n_out,
        "dropped_by_id": by_id, "dropped_by_time": by_time,
        "filter_disagreements": disagree,
        "sha256": h_dst.hexdigest(), "src_sha256": h_src.hexdigest(),
        "path": dst,
    }
    print(json.dumps({split: manifest["splits"][split]}), flush=True)

json.dump(manifest, open(MANIFEST, "w", encoding="utf-8"), indent=1)
print("WROTE", MANIFEST)