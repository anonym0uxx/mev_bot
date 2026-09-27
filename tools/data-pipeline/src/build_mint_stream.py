#!/usr/bin/env python3
"""Build the time-ordered mint stream that `pq_narrative_resolve` consumes.

This is step 1 of the forward-corpus narrative pipeline:

    build_mint_stream.py  ->  pq_narrative_resolve  ->  narrative_sidecar.jsonl
                                                          |
                          build_decision_sft_identity.py  <--  --narrative

WHY ORDER IS THE POINT. The resolver's alias stage is a ring buffer over the mint
stream, so `t_ms` order is an INPUT, not a detail: the same rows in a different order
produce different stages. This script therefore sorts by `t_ms` and asserts the result
is non-decreasing before writing, so a mis-sorted file fails here rather than silently
producing stages that depend on how the parquet happened to be laid out.

WHY THE UNIT CONVERSION IS SPELLED OUT. `detected_at` is a `datetime64[us]` column in
this corpus. `col.astype("int64") // 1_000_000` looks like microseconds->milliseconds
but actually yields SECONDS on a `us` column, and those seconds then get compared
against millisecond lexicon bounds — every entry computes as long expired and nothing
matches, which reads as a thin vocabulary rather than as a unit bug. Differencing
against the epoch is correct for every storage unit, so it is used rather than a
constant divisor.

Usage:
    build_mint_stream.py --corpus /training/slinky21 --out stream.jsonl
    build_mint_stream.py --corpus <dir> --out stream.jsonl --limit 200000
"""

import argparse
import json
import os
import sys

import pandas as pd

EPOCH = pd.Timestamp("1970-01-01", tz="UTC")


def to_epoch_ms(col):
    """Epoch milliseconds from a timestamp column, whatever its storage unit."""
    if str(col.dtype).startswith("datetime64") or "tz" in str(col.dtype):
        if col.dt.tz is None:
            col = col.dt.tz_localize("UTC")
        return ((col - EPOCH) // pd.Timedelta("1ms")).astype("int64")
    # Already an integer epoch: infer the unit from magnitude, and say so loudly if
    # it is not one of the three we expect rather than guessing silently.
    t = col.astype("int64")
    med = int(t.median())
    if med < 10_000_000_000:            # ~1.7e9  -> seconds
        return t * 1000
    if med < 10_000_000_000_000:        # ~1.7e12  -> milliseconds
        return t
    if med < 10_000_000_000_000_000:    # ~1.7e15  -> microseconds
        return t // 1000
    return t // 1_000_000               # ~1.7e18  -> nanoseconds


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--corpus", required=True,
                    help="directory containing tokens.parquet")
    ap.add_argument("--out", required=True, help="JSONL to write")
    ap.add_argument("--limit", type=int, default=0,
                    help="0 = the whole corpus (default). ANY slice leaves most "
                         "corpus mints with no narrative record at all.")
    a = ap.parse_args()

    src = os.path.join(a.corpus, "tokens.parquet")
    df = pd.read_parquet(src, columns=["mint", "name", "symbol", "detected_at"])
    print(f"rows: {len(df)}  detected_at dtype: {df['detected_at'].dtype}")

    df = df.assign(t_ms=to_epoch_ms(df["detected_at"])).sort_values("t_ms")
    if a.limit:
        df = df.head(a.limit)

    # The contract the resolver depends on. Fail here, not downstream.
    t = df["t_ms"].to_numpy()
    if len(t) and (t[1:] < t[:-1]).any():
        raise SystemExit("FATAL: t_ms is not non-decreasing after sort — refusing to write a "
                         "stream whose alias stages would depend on row order")

    named = 0
    with open(a.out, "w", encoding="utf-8", newline="\n") as f:
        for r in df.to_dict("records"):
            name = "" if r["name"] is None else str(r["name"])
            sym = "" if r["symbol"] is None else str(r["symbol"])
            if name:
                named += 1
            f.write(json.dumps({"t_ms": int(r["t_ms"]), "mint": str(r["mint"]),
                                "name": name, "symbol": sym},
                               ensure_ascii=False) + "\n")

    print(f"wrote {len(df)} rows to {a.out}")
    if len(t):
        print(f"t_ms range: {int(t.min())} -> {int(t.max())}")
    print(f"rows with a name: {named}")
    return 0


if __name__ == "__main__":
    sys.exit(main())