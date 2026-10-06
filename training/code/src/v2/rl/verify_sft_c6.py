#!/usr/bin/env python
"""INDEPENDENT verifier for candidate_sft_c6. Does NOT import the builder.

Proves, by merge-joining the c3 source and the c6 output in file order (which also
proves order preservation):
  1. LABELS: a c6 label is byte-identical to its c3 label, OR differs ONLY inside the
     audited unit phrase ("SOL per raw token" -> "lamports per raw token"). Any other
     difference fails the row. No number, decision or rationale byte may move.
  2. PROMPTS: stripping the injected lines from the c6 prompt must reproduce the c3
     prompt with the ONE declared rename applied. Nothing else may differ.
  3. The rename did not alter a single digit: the value after
     price_lamports_per_raw_token= must equal the value after price_sol_per_raw= in c3.
  4. No forbidden (lookahead) inputs appear in any prompt.
  5. Wall-clean: no row at/after wall_ms. SOL-only: no dropped mint present.
  6. Row accounting: c6 rows are a subsequence of c3 rows; join is exact.
"""
import collections
import json
import os
import re
import sys

C3 = "/training/v2/candidate_sft_c3"
C6 = "/training/v2/candidate_sft_c6"
DROP = "/training/v2/reports/SOL_DROPSET_V2.json"
WALL_MS = 1788984256517
OUT = "/training/v2/reports/SFT_C6_INDEPENDENT_VERIFY.json"

UNIT_WRONG = "SOL per raw token"
UNIT_RIGHT = "lamports per raw token"
INJECTED = ("CURVE STATE", "AMM POOL STATE", "PRICE UNITS")
FORBIDDEN = ("seconds_to_graduation", "graduation_proximity_pct")
RE_OLD = re.compile(r"price_sol_per_raw=([0-9eE.+\-]+)")
RE_NEW = re.compile(r"price_lamports_per_raw_token=([0-9eE.+\-]+)")

drop = set(json.load(open(DROP, encoding="utf-8"))["drop"])

fails = collections.Counter()
samples = []
res = {"schema": "sft_c6_independent_verify_v1", "splits": {}}


def note(tag, detail):
    fails[tag] += 1
    if len(samples) < 12:
        samples.append({"tag": tag, "detail": detail[:240]})


for split in ("train", "validation", "examination"):
    c3p, c6p = os.path.join(C3, f"{split}.jsonl"), os.path.join(C6, f"{split}.jsonl")
    if not os.path.isfile(c6p):
        note("missing_split", c6p)
        continue
    n3 = n6 = joined = 0
    label_fixed = label_same = prompt_injected = prompt_plain = 0
    stat = collections.Counter()
    with open(c3p, encoding="utf-8") as f3, open(c6p, encoding="utf-8") as f6:
        l3 = f3.readline()
        for l6 in f6:
            n6 += 1
            try:
                r6 = json.loads(l6)
            except Exception:
                note("c6_unparseable", l6)
                continue
            cid6 = (r6.get("meta") or {}).get("candidate_id")
            # advance the source until the candidate_id matches (proves subsequence)
            while l3:
                try:
                    r3 = json.loads(l3)
                except Exception:
                    n3 += 1
                    l3 = f3.readline()
                    continue
                n3 += 1
                if (r3.get("meta") or {}).get("candidate_id") == cid6:
                    break
                l3 = f3.readline()
            if not l3:
                note("c6_row_not_in_c3", str(cid6))
                continue
            joined += 1
            m3, m6 = r3["messages"], r6["messages"]
            p3 = m3[1].get("content") or ""
            p6 = m6[1].get("content") or ""
            a3 = m3[-1].get("content") or ""
            a6 = m6[-1].get("content") or ""

            # ---- 1. label integrity
            if a6 == a3:
                label_same += 1
            elif a6.replace(UNIT_RIGHT, "") == a3.replace(UNIT_WRONG, ""):
                label_fixed += 1
                stat["unit_fix_rows"] += 1
                stat["unit_fix_occurrences"] += a3.count(UNIT_WRONG)
            else:
                note("label_changed_beyond_unit", str(cid6))

            # ---- 2. prompt additivity
            lines = p6.split("\n")
            kept = [x for x in lines if not x.startswith(INJECTED)]
            p6_stripped = "\n".join(kept)
            p3_renamed = p3.replace("price_sol_per_raw=",
                                    "price_lamports_per_raw_token=")
            if p6_stripped == p3_renamed:
                if len(kept) == len(lines):
                    prompt_plain += 1
                else:
                    prompt_injected += 1
            else:
                note("prompt_not_additive", str(cid6))

            # ---- 3. rename preserved the digits
            v3 = RE_OLD.search(p3)
            v6 = RE_NEW.search(p6)
            if bool(v3) != bool(v6):
                note("price_field_presence_mismatch", str(cid6))
            elif v3 and v3.group(1) != v6.group(1):
                note("price_value_changed_by_rename", f"{v3.group(1)}!={v6.group(1)}")

            # ---- 4. no forbidden inputs
            for f in FORBIDDEN:
                if f in p6:
                    note("forbidden_input_present", f"{f} in {cid6}")

            # ---- 5. wall-clean + SOL-only
            t = r6.get("meta", {}).get("t_dec_ms") or r6.get("t_dec_ms")
            if t is not None and int(t) >= WALL_MS:
                note("wall_violation", str(cid6))
            if r6.get("mint") in drop:
                note("dropped_mint_present", str(r6.get("mint")))

    res["splits"][split] = {
        "c3_lines_scanned": n3, "c6_rows": n6, "joined": joined,
        "labels_byte_identical": label_same, "labels_unit_fix_only": label_fixed,
        "prompts_annotated": prompt_injected, "prompts_plain": prompt_plain,
        **{k: v for k, v in stat.items()},
    }
    print(json.dumps({split: res["splits"][split]}), flush=True)

res["failures"] = dict(fails)
res["samples"] = samples
res["verdict"] = "PASS" if not fails else "FAIL"
json.dump(res, open(OUT, "w"), indent=1)
print(json.dumps({"failures": dict(fails), "verdict": res["verdict"],
                  "samples": samples[:6]}, indent=1))
print("WROTE", OUT)
sys.exit(0 if not fails else 1)