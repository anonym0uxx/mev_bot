#!/usr/bin/env python
"""C9 market-cap / regime-tag parity fixture - real c12 rows vs `reserve_view`.

WHAT IS BEING GRADED. `AnnotationState::reserve_view` decides (a) which plane prices the
market cap, (b) the cap itself, and (c) which venue the SIZE OPTIONS depth is measured
against. The authority for all three is the corpus: its CURVE/AMM STATE lines carry the raw
reserves and prices, its C9 enrichment carries the resulting `mcap_sol_at_t` / `mcap_source`,
and its SIZE OPTIONS line carries the depth it measured.

This fixture joins those three sources on real rows. The Rust test rebuilds the view from the
RAW inputs and compares against the corpus's own outputs, so it fails on a wrong precedence
rule (curve vs AMM), a wrong supply multiplier, a wrong depth basis, or a wrong regime tag.

Inputs:  /training/v2/candidate_sft_c8/{train,validation,examination}.jsonl   (rows)
         /training/v2/reports/C9_ENRICHMENT_FULL.jsonl                        (mcap, source)
Output:  parity_rust/c9_mcap_parity.json
"""
import collections
import json
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = "/training/v2/candidate_sft_c8"
SPLITS = {"train": "train.jsonl", "validation": "validation.jsonl", "examination": "examination.jsonl"}
ENRICH = "/training/v2/reports/C9_ENRICHMENT_FULL.jsonl"

CURVE = re.compile(
    r"curve_reserves=(present|absent).*?(?:reason=(\S+))?.*?$")
AMM = re.compile(
    r"amm_reserves=(present|absent).*?(?:reason=(\S+))?.*?$")
NUM = r"(\S+)"


def parse_line(line, kind):
    """Parse a CURVE/AMM STATE line into the raw inputs the Rust annotator needs."""
    if "=absent" in line:
        m = re.search(r"reason=(\S+)", line)
        return {"status": "absent", "reason": m.group(1) if m else None}
    def g(key):
        m = re.search(re.escape(key) + r"=(\S+)", line)
        return m.group(1) if m else None
    if kind == "curve":
        return {
            "status": "present",
            "staleness_ms": int(g("staleness_ms")),
            "v_sol_lamports": int(round(float(g("v_sol_reserves_sol")) * 1e9)),
            "v_tokens": int(g("v_tokens_reserves")),
            "real_sol_lamports": int(round(float(g("real_sol_reserves_sol")) * 1e9)),
            "real_tokens": int(g("real_tokens_reserves")),
            "curve_price_sol_per_raw_token": float(g("curve_price_sol_per_raw_token")),
            "curve_progress": float(g("curve_progress")),
            "curve_regime": g("curve_regime"),
        }
    return {
        "status": "present",
        "pool": g("pool"),
        "staleness_ms": int(g("staleness_ms")),
        "base_reserves_raw": int(g("base_reserves_raw")),
        "quote_reserves_lamports": int(g("quote_reserves_lamports")),
        "amm_price_sol_per_raw_token": float(g("amm_price_sol_per_raw_token")),
        "reserve_slot": int(g("reserve_slot")),
    }


def parse_depth(content):
    m = re.search(r"pool depth (\S+) SOL", content)
    return float(m.group(1)) if m else None


def main():
    rows = []
    for split, fn in SPLITS.items():
        p = os.path.join(BASE, fn)
        if not os.path.isfile(p):
            continue
        with open(p, encoding="utf-8") as f:
            for idx, line in enumerate(f):
                r = json.loads(line)
                msgs = r.get("messages") or []
                if len(msgs) < 2:
                    continue
                content = msgs[1]["content"]
                curve_line = next((l for l in content.split("\n") if l.startswith("CURVE STATE")), None)
                amm_line = next((l for l in content.split("\n") if l.startswith("AMM POOL STATE")), None)
                if curve_line is None or amm_line is None:
                    continue
                t = re.search(r"t_dec_ms=(\d+)", content)
                if not t:
                    continue
                rows.append({
                    "split": split, "row_index": idx, "mint": r.get("mint"),
                    "t_dec_ms": int(t.group(1)),
                    "curve": parse_line(curve_line, "curve"),
                    "amm": parse_line(amm_line, "amm"),
                    "depth_4g": parse_depth(content),
                })

    enrich = {}
    with open(ENRICH, encoding="utf-8") as f:
        for line in f:
            e = json.loads(line)
            enrich[(e["split"], e["row_index"])] = e

    cases = []
    seen_src = collections.Counter()
    for r in rows:
        e = enrich.get((r["split"], r["row_index"]))
        if not e:
            continue
        cases.append({
            "split": r["split"], "row_index": r["row_index"], "mint": r["mint"],
            "t_dec_ms": r["t_dec_ms"], "curve": r["curve"], "amm": r["amm"],
            "expected_mcap_sol": e["mcap_sol_at_t"], "expected_source": e["mcap_source"],
            "expected_depth_4g": r["depth_4g"],
            "expected_depth_exact": (r["amm"].get("quote_reserves_lamports") / 1e9)
            if r["amm"]["status"] == "present" else None,
        })
        seen_src[e["mcap_source"]] += 1

    # Keep the file small but keep every source represented and both regimes present.
    by_src = collections.defaultdict(list)
    for c in cases:
        by_src[c["expected_source"]].append(c)
    picked = []
    for src, lst in sorted(by_src.items()):
        # prefer rows with a distinct mint, cap at 20 per source
        seen = set()
        for c in lst:
            if c["mint"] in seen:
                continue
            seen.add(c["mint"])
            picked.append(c)
            if len(seen) >= 20:
                break
    out = {"schema": "c9-mcap-parity/1",
           "producer": "candidate_sft_c8 rows x C9_ENRICHMENT_FULL.jsonl",
           "n_rows_seen": len(rows), "n_joined": len(cases),
           "source_counts_all": dict(seen_src),
           "source_counts_picked": dict(collections.Counter(c["expected_source"] for c in picked)),
           "cases": picked}
    path = os.path.join(HERE, "c9_mcap_parity.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, sort_keys=True)
    print("wrote %s: %d joined rows, %d picked (%s)"
          % (path, len(cases), len(picked), out["source_counts_picked"]))
    print("all sources:", dict(seen_src))
    for c in picked[:6]:
        print("  %-9s %-5s mcap=%-14s depth4g=%s" % (c["split"], c["expected_source"],
                                                      c["expected_mcap_sol"], c["expected_depth_4g"]))


if __name__ == "__main__":
    main()
