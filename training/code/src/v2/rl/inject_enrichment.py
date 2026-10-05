#!/usr/bin/env python
"""Inject the enriched state block into the two families that lack it.

SPEC 5: management, forecasting and utility families must carry the same ENRICHED
CANDIDATE STATE / DEV HISTORY block as the entry family, so the policy sees ONE state
format, and every label must cite at least one enriched field (the B5 acceptance rule: a
field the labels never cite is a field the model ignores).

Measured before this pass: decision 100% has it, utility_regression 100% has it,
management 0%, utility_reasoning 0%. Half a state format is worse than none - the model
learns that holders/mcap/dev fields appear only sometimes, which is the corner it will
then ignore at serve time.

Enrichment comes from `C9_ENRICHMENT_FULL.jsonl`, keyed by (mint, t_dec_ms), the same
index the entry family cites.
"""
import argparse
import collections
import json
import os
import re
import sys

SOURCES = {
    "management_replay": "/training/v2/reports/mgmt_c10",
}
ENRICH = "/training/v2/reports/C9_ENRICHMENT_FULL.jsonl"
# Management decision points sit OFF the ledger grid (entry clock + k*30s), so they are
# absent from the c9 index by construction - 0 of 29,770 were present when this was
# measured. This index is the same algorithm re-run over the management clocks.
ENRICH_MGMT = "/training/v2/reports/MGMT_ENRICHMENT_FULL.jsonl"
SPLIT_LEGACY = {"train": "train", "validation": "val", "examination": "test",
                "val": "val", "test": "test"}
RE_TDEC = re.compile(r"t_dec_ms=(\d+)")
RE_MINT = re.compile(r"^MINT:\s*(\S+)", re.M)
FIELDS = (("holders_at_t", "holders_at_t"), ("top1_float_share", "top1_float_share"),
          ("holder_hhi", "holder_hhi"), ("mcap_sol_at_t", "mcap_sol_at_t"),
          ("bundle_wallets", "bundle_wallets"), ("round_trip_wallets", "round_trip_wallets"),
          ("creator_past_launches", "creator_past_launches"), ("creator_known", "creator_known"),
          ("wash_ratio", "wash_ratio"))


def darkish(x):
    return x is None or (isinstance(x, str) and x.strip().lower() in ("", "n/a", "none", "null"))


def load_index():
    idx = {}
    for path in (ENRICH, ENRICH_MGMT):
        if not os.path.exists(path):
            continue
        with open(path) as fh:
            for line in fh:
                r = json.loads(line)
                idx[(r["mint"], int(r["t_dec_ms"]))] = r
    return idx


def key_of(r):
    m = r.get("meta") or {}
    mint = r.get("mint") or m.get("mint")
    t = m.get("decision_time_unix_ms")
    if t is None:
        mm = RE_TDEC.search(r["messages"][1]["content"])
        t = int(mm.group(1)) if mm else None
    if mint is None:
        mm = RE_MINT.search(r["messages"][1]["content"])
        mint = mm.group(1) if mm else None
    if mint is None or t is None:
        return None
    return (mint, int(t))


def block(e):
    parts = [f"{lab}={e[k]}" for k, lab in FIELDS if not darkish(e.get(k))]
    if not parts:
        return None
    return "ENRICHED CANDIDATE STATE: " + " ".join(parts)


def dev(e):
    parts = [f"{k}={e[k]}" for k in ("creator_past_launches", "creator_known",
                                     "bundle_wallets", "wash_ratio")
             if not darkish(e.get(k))]
    return "DEV HISTORY: " + " ".join(parts) if parts else None


def inject(r, idx, stats):
    k = key_of(r)
    e = idx.get(k) if k else None
    if e is None:
        stats["no_enrichment"] += 1
        return None
    b, d = block(e), dev(e)
    if b is None:
        stats["no_usable_fields"] += 1
        return None
    u = r["messages"][1]["content"]
    if "ENRICHED CANDIDATE STATE" in u:
        stats["already_present"] += 1
    else:
        lines = u.rstrip("\n").split("\n")
        add = [b] + ([d] if d else [])
        # place after the last causal-state line: the first line starting with two spaces
        pos = None
        for i, ln in enumerate(lines):
            if ln.startswith("  "):
                pos = i + 1
        if pos is None:
            lines = lines + add
        else:
            lines = lines[:pos] + add + lines[pos:]
        u = "\n".join(lines) + "\n"
        r["messages"][1]["content"] = u
        stats["block_injected"] += 1
    a = r["messages"][2]["content"]
    if "ENRICHMENT CITATION" not in a:
        cite = [f"{k}={e[k]}" for k in ("holders_at_t", "top1_float_share",
                                        "creator_past_launches", "mcap_sol_at_t")
                if not darkish(e.get(k))]
        if not cite:
            stats["no_citable_field"] += 1
            return None
        r["messages"][2]["content"] = a.rstrip("\n") + "\nENRICHMENT CITATION: " + \
            " ".join(cite) + "\n"
        stats["citation_added"] += 1
    m = r["meta"]
    m["enrichment_status"] = "injected"
    m["family"] = m.get("family")
    r["split"] = SPLIT_LEGACY.get(m.get("split"), "train")
    return 0


def verify(r):
    bad = []
    u = r["messages"][1]["content"]
    a = r["messages"][2]["content"]
    if "ENRICHED CANDIDATE STATE:" not in u:
        bad.append("no_enriched_block")
    if "ENRICHMENT CITATION:" not in a:
        bad.append("no_citation")
    if (r["meta"].get("enrichment_status") or "") != "injected":
        bad.append("status_not_set")
    return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--outroot", default="/training/v2")
    ap.add_argument("--verify-only", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    idx = load_index()
    print("enrichment index", len(idx), flush=True)
    report = {"corpus": "c10_enrichment_injection", "suites": {}}
    total_bad = 0
    for family, src in SOURCES.items():
        for split in ("train", "validation", "examination"):
            srcp = os.path.join(src, f"{split}.jsonl")
            if not os.path.exists(srcp):
                continue
            st = collections.Counter()
            bad_rows = collections.Counter()
            outp = os.path.join(args.outroot,
                                f"{os.path.basename(src)}_enr", f"{split}.jsonl")
            fout = None
            if not args.verify_only and not args.dry_run:
                os.makedirs(os.path.dirname(outp), exist_ok=True)
                fout = open(outp, "w")
            with open(srcp if not args.verify_only else outp) as fh:
                for line in fh:
                    r = json.loads(line)
                    if not args.verify_only and not args.dry_run:
                        if inject(r, idx, st) is None:
                            continue
                    for b in verify(r):
                        bad_rows[b] += 1
                        st["rows_with_defect"] += 1
                    st["rows_out"] += 1
                    if fout:
                        fout.write(json.dumps(r, ensure_ascii=False) + "\n")
            if fout:
                fout.close()
            bad = st["rows_with_defect"]
            total_bad += bad
            report["suites"][f"{family}/{split}"] = {"stats": dict(st), "defects": dict(bad_rows)}
            print(f"  {family:<20} {split:<12} out={st['rows_out']:<7} violations={bad}",
                  flush=True)
    out = ("/training/v2/reports/ENRICHMENT_INJECT_VERIFY.json" if args.verify_only
           else "/training/v2/reports/ENRICHMENT_INJECT.json")
    json.dump(report, open(out, "w"), indent=1)
    print("TOTAL_VIOLATIONS", total_bad, "->", out)
    return 1 if total_bad else 0


if __name__ == "__main__":
    sys.exit(main())