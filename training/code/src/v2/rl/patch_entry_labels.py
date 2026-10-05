#!/usr/bin/env python
"""Align the entry family's target header to its label authority.

THE DEFECT. 17,553 of 84,681 rows (20.7%) carry a target whose `DECISION:` token
disagrees with `meta.action`:

    meta.action=WATCH  target DECISION=BUY    7,054
    meta.action=BUY    target DECISION=SKIP   7,038
    meta.action=SKIP   target DECISION=BUY    3,461

WHICH SIDE IS AUTHORITATIVE. Not a judgement call, because the two meta fields are
written by different stages and they AGREE with each other on all 17,553 rows:
`meta.action == meta.size_dimension.decision` in every case. The target text is the
outlier, and the rows' own EVIDENCE bodies argue for the meta side (e.g. a row header
saying BUY whose evidence reads "net_flow is not decisively positive against the 76 bp
round-trip cost"). So: meta wins, the target header is repaired.

WHY IT SURVIVED. The entry family has no label/target consistency gate. The management
family has one (`qa_management_c10.py:100`) and it has never tripped. A family with no
gate ships whatever the renderer produced - which is the actual root cause of a 20.7%
contradiction reaching a "final" corpus.

Also repaired: 5,386 rows put `SIZE:` before `DECISION:`, against the family's own
format contract ("DECISION, then SIZE/PRICE LIMIT (BUY only)").

This changes no label and no prompt - only the header lines of the recorded answer.
"""
import argparse
import collections
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

SRC = "/training/v2/candidate_sft_c10_entry"
ENTRY = ("BUY", "WATCH", "SKIP")
RE_HEAD = re.compile(r"^(?:DECISION:\s*\w+\s*\n)?(?:SIZE:\s*\S+\s*\n)?")


def repair(r, stats):
    m = r["meta"]
    action = m.get("action")
    if action not in ENTRY:
        stats["action_not_in_space"] += 1
        return None
    sd = m.get("size_dimension") or {}
    a = r["messages"][2]["content"]
    old = (re.search(r"DECISION:\s*(\w+)", a) or [None, None])[1]
    if old == action:
        stats["already_consistent"] += 1
    else:
        stats["decision_repaired"] += 1
        stats[f"pair_{old}_to_{action}"] += 1

    # What SIZE belongs on this row, per the label authority.
    if action == "BUY":
        size = sd.get("size")
        if size in (None, "NONE"):
            stats["buy_without_size"] += 1
            return None
    else:
        size = "NONE"

    body = RE_HEAD.sub("", a.lstrip())
    if not re.search(r"^(INVALIDATION|EVIDENCE|COUNTEREVIDENCE|ENRICHMENT|SIZE_BASIS|"
                     r"DECISION|SIZE):", body, re.M):
        stats["body_unrecognised"] += 1
        return None
    head = f"DECISION: {action}\nSIZE: {size}\n"
    r["messages"][2]["content"] = head + body
    sd["decision"] = action
    sd["header_repaired"] = True
    m["size_dimension"] = sd
    return 0


def verify(r):
    bad = []
    m = r["meta"]
    a = r["messages"][2]["content"]
    dec = (re.search(r"^DECISION:\s*(\w+)", a, re.M) or [None, None])[1]
    if dec != m.get("action"):
        bad.append("decision_disagrees_with_meta")
    sd = (re.search(r"^SIZE:\s*(\S+)", a, re.M) or [None, None])[1]
    if sd is None:
        bad.append("no_size_line")
    elif m.get("action") == "BUY" and sd == "NONE":
        bad.append("buy_with_size_none")
    elif m.get("action") != "BUY" and sd != "NONE":
        bad.append("non_buy_with_size")
    i_dec = a.find("DECISION:")
    i_size = a.find("SIZE:")
    if i_dec == -1 or i_size == -1 or i_size < i_dec:
        bad.append("size_before_decision")
    return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--splits", nargs="+",
                    default=["train", "validation", "examination"])
    ap.add_argument("--outdir", default="/training/v2/candidate_sft_c10_entry_labeled")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--verify-only", action="store_true")
    args = ap.parse_args()

    src = args.outdir if args.verify_only else SRC
    if not args.dry_run and not args.verify_only:
        os.makedirs(args.outdir, exist_ok=True)
    report = {"suites": {}, "corpus": "c10_entry_label_alignment"}
    total = 0
    for split in args.splits:
        st = collections.Counter()
        bad_rows = collections.Counter()
        fout = None
        if not args.dry_run and not args.verify_only:
            fout = open(os.path.join(args.outdir, f"{split}.jsonl"), "w")
        with open(os.path.join(src, f"{split}.jsonl")) as fh:
            for line in fh:
                r = json.loads(line)
                if not args.verify_only:
                    if repair(r, st) is None:
                        st["dropped"] += 1
                        continue
                for b in verify(r):
                    bad_rows[b] += 1
                    st["rows_with_defect"] += 1
                st["rows_out"] += 1
                if fout:
                    fout.write(json.dumps(r, ensure_ascii=False) + "\n")
        if fout:
            fout.close()
        total += st["rows_with_defect"]
        report["suites"][split] = {"stats": dict(st), "defects": dict(bad_rows)}
        print(split, "out", st["rows_out"], "repaired", st["decision_repaired"],
              "defects", dict(bad_rows), flush=True)
    out = ("/training/v2/reports/ENTRY_LABEL_ALIGN_VERIFY.json" if args.verify_only
           else "/training/v2/reports/ENTRY_LABEL_ALIGN.json")
    json.dump(report, open(out, "w"), indent=1)
    print("TOTAL_DEFECT_ROWS", total, "->", out)
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())