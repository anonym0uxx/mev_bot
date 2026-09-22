#!/usr/bin/env python
"""Build corpus-aligned tapes: the data gap behind the entry-side measurement.

WHY: the pinned SFT corpus makes decisions across 2026-08-23..2026-09-09, but the
tape subset we scored against (canonical/renorm_pooltest) only covers 1,268 mints
from 2026-08-24 onward - of 1,516 corpus mints just 12 had a tape. Entry scoring
therefore had no aligned data. canonical/renormalized_v7/trades.jsonl (7.35 GB,
15.9 M rows) DOES cover the corpus window, so this extracts exactly the rows for
the corpus mints into a compact file that load_canonical_tapes can read.

Method: collect the mint set from the pinned corpus (every record carries a `mint`
field), then stream the 7.35 GB file once with a cheap regex prefilter and only
parse lines whose mint is wanted. Writes a compact JSONL with the 6 fields the
Tape class needs, plus a coverage report.

Usage:
  python build_corpus_mint_tapes.py [--limit-lines N] [--out DIR]
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time

CORPUS = ["/training/v2/candidate_sft_c3/train.jsonl",
          "/training/v2/candidate_sft_c3/validation.jsonl",
          "/training/v2/candidate_sft_c3/examination.jsonl"]
TRADES = "/training/v2/canonical/renormalized_v7/trades.jsonl"
KEEP = ("mint", "recv_unix_ms", "sol_lamports", "tokens_raw", "venue", "side",
        "signature")
MINT_RE = re.compile(r'"mint"\s*:\s*"([^"]+)"')


def collect_mints(paths):
    mints = set()
    rows = 0
    for p in paths:
        if not os.path.isfile(p):
            continue
        for line in open(p, encoding="utf-8"):
            if '"mint"' not in line:
                continue
            m = MINT_RE.search(line)
            if m:
                mints.add(m.group(1))
            rows += 1
    return mints, rows


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default="/training/v2/canonical/renorm_corpus_mints")
    ap.add_argument("--limit-lines", type=int, default=0)
    a = ap.parse_args()

    t0 = time.time()
    mints, corpus_rows = collect_mints(CORPUS)
    if not mints:
        raise SystemExit("REFUSING: no mints collected from the corpus")
    print(json.dumps({"corpus_records": corpus_rows, "corpus_mints": len(mints),
                      "collect_s": round(time.time() - t0, 1)}), flush=True)

    os.makedirs(a.out, exist_ok=True)
    out_path = os.path.join(a.out, "trades.jsonl")
    tmp = out_path + ".partial"
    seen, written, kept_mints = 0, 0, set()
    t1 = time.time()
    with open(TRADES, encoding="utf-8") as fin, open(tmp, "w", encoding="utf-8") as fout:
        for line in fin:
            seen += 1
            if a.limit_lines and seen > a.limit_lines:
                break
            if '"mint"' not in line:
                continue
            m = MINT_RE.search(line)
            if not m or m.group(1) not in mints:
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            fout.write(json.dumps({k: r.get(k) for k in KEEP}) + "\n")
            written += 1
            kept_mints.add(m.group(1))
            if written % 500000 == 0:
                print(json.dumps({"progress_rows": written,
                                  "scan_s": round(time.time() - t1, 1)}), flush=True)
    os.replace(tmp, out_path)
    rep = {"corpus_mints": len(mints), "trades_lines_scanned": seen,
           "rows_written": written, "mints_recovered": len(kept_mints),
           "mints_with_no_trades": len(mints - kept_mints),
           "coverage": round(len(kept_mints) / len(mints), 4),
           "out": out_path, "bytes": os.path.getsize(out_path),
           "scan_s": round(time.time() - t1, 1),
           "total_s": round(time.time() - t0, 1)}
    with open(os.path.join(a.out, "COVERAGE.json"), "w", encoding="utf-8") as fh:
        json.dump(rep, fh, indent=2)
    print(json.dumps(rep, indent=1), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())