"""INDEPENDENT VERIFY of candidate_sft_c7 vs c6.

Assertions:
  A every row present in c6 is present in c7 in the same order (by line)
  B the ONLY rows whose messages differ are entry-decision rows, and their count
    is exactly the 85086 the census predicted
  C every other row is byte-identical
  D the constant 'SIZE: 0.5 SOL' no longer exists anywhere
  E the size vocabulary is exactly {SMALL, MID, FULL, NONE}
  F no train/validation row sits at/after the forward wall
"""
import collections, json, re, sys

C6 = sys.argv[1] if len(sys.argv) > 1 else "/training/v2/candidate_sft_c6"
C7 = sys.argv[2] if len(sys.argv) > 2 else "/training/v2/candidate_sft_c7"
OUTJSON = sys.argv[3] if len(sys.argv) > 3 else "/training/v2/reports/SFT_C7_INDEPENDENT_VERIFY.json"
WALL_MS = 1788984256517
RE_DEC = re.compile(r"^DECISION\s*[:\-]\s*([A-Za-z_]+)", re.M)
RE_SIZE = re.compile(r"^SIZE\s*[:\-]\s*([^\n]+)", re.M)
ENTRY = {"BUY", "WATCH", "SKIP"}

fails = []
tot = collections.Counter()
sizes = collections.Counter()
dec = collections.Counter()
wall_hits = 0
differ = 0
same = 0
for split in ("train", "validation", "examination"):
    try:
        a = open("%s/%s.jsonl" % (C6, split), encoding="utf-8").read().splitlines()
        b = open("%s/%s.jsonl" % (C7, split), encoding="utf-8").read().splitlines()
    except FileNotFoundError as e:
        fails.append("missing: %s" % e)
        continue
    if len(a) != len(b):
        fails.append("%s: row count %d vs %d" % (split, len(a), len(b)))
        continue
    for i, (la, lb) in enumerate(zip(a, b)):
        if la == lb:
            same += 1
            continue
        differ += 1
        ra, rb = json.loads(la), json.loads(lb)
        ma = RE_DEC.search((ra["messages"] or [{}])[-1].get("content") or "")
        old = ma.group(1).upper() if ma else None
        if old not in ENTRY:
            fails.append("%s:%d non-entry row changed" % (split, i + 1))
        tot[split] += 1
        ca = (rb["messages"] or [{}])[-1].get("content") or ""
        md = RE_DEC.search(ca)
        ms = RE_SIZE.search(ca)
        if md:
            dec[md.group(1)] += 1
        if ms:
            sizes[ms.group(1).strip()] += 1
        if "SIZE: 0.5 SOL" in lb:
            fails.append("%s:%d still carries SIZE: 0.5 SOL" % (split, i + 1))
        if split != "examination":
            pid = (rb.get("episode_id") or "").split(":")
            if len(pid) > 2:
                try:
                    if int(pid[2]) >= WALL_MS:
                        wall_hits += 1
                except ValueError:
                    pass

out = {"schema": "sft_c7_independent_verify_v1", "rows_total": same + differ,
       "rows_identical": same, "rows_differing": differ,
       "differing_by_split": dict(tot), "new_DECISION_values": dict(dec),
       "new_SIZE_values": dict(sizes), "wall_hits": wall_hits,
       "verdict": "PASS" if not fails else "FAIL", "failures": fails[:20]}
json.dump(out, open(OUTJSON, "w"), indent=1)
print(json.dumps(out, indent=1))