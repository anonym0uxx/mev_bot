"""Label-integrity guard for the SOL-only SFT rebuild.

Two invariants the rebuild must satisfy:

  I1. c3 (the contaminated corpus) is NEVER mutated. It is the audit baseline; the
      rebuild writes a NEW file (c4). Verify by recomputing the pinned sha256.
  I2. Filtering selects RECORDS by mint and preserves each record's `messages`
      byte-for-byte - labels are never rewritten, retargeted, or padded. Verify by
      comparing the label bytes of surviving records against the source.

This script checks I1 now, and provides `verify_subset` for I2 after the rebuild.
"""
import hashlib
import json
import sys

PINS = json.load(open("/training/rel/SFT_DATASET_V2.json", encoding="utf-8"))


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 22), b""):
            h.update(chunk)
    return h.hexdigest()


def label_bytes(rec):
    """The assistant turn(s) - the label - as canonical bytes."""
    ms = rec.get("messages") or []
    lab = [m for m in ms
           if (m.get("role") or m.get("from")) == "assistant"]
    return json.dumps(lab, sort_keys=True, ensure_ascii=False).encode("utf-8")


def verify_subset(src_path, sub_path, max_check=200000):
    """I2: every line in `sub` must exist BYTE-FOR-BYTE in `src`.

    DO NOT key this on episode_id - it is NOT unique. One episode yields several
    records via task_targets (decision_action / next_action_decision /
    decision_reasoning), each with a different assistant label but the SAME
    episode_id. Keying on it compares a record against a sibling's label and
    reports ~60% false mismatches (reproduced: 107,339/175,471 "failures" on a
    dataset that is in fact a byte-exact subset).

    Line-level set containment is the correct invariant for a filtered corpus and
    is the strongest statement that no label was rewritten or padded.
    """
    src = {}
    with open(src_path, encoding="utf-8") as f:
        for line in f:
            h = hashlib.sha256(line.encode("utf-8")).digest()
            src[h] = src.get(h, 0) + 1
    n = ok = bad = 0
    with open(sub_path, encoding="utf-8") as f:
        for line in f:
            n += 1
            h = hashlib.sha256(line.encode("utf-8")).digest()
            if src.get(h, 0) > 0:
                src[h] -= 1
                ok += 1
            else:
                bad += 1
            if n >= max_check:
                break
    return {"subset_rows_checked": n, "matched_in_source": ok,
            "unmatched": bad,
            "verdict": "LABELS INTACT (byte-exact subset)" if bad == 0
            else "INTEGRITY FAIL"}


if __name__ == "__main__":
    out = {"invariant": "I1 - c3 corpus unmutated"}
    for split in ("train", "validation", "examination"):
        for entry in PINS.get(split, []):
            p, want = entry["path"], entry["sha256"]
            got = sha256_file(p)
            out[split] = {"path": p, "pinned": want[:16], "actual": got[:16],
                          "match": got == want,
                          "rows": entry.get("rows")}
    print(json.dumps(out, indent=1))
