#!/usr/bin/env python
"""Merge all renormalized capture sessions into the v7 universe.

Sources: renormalized_v6 (Sep-9 x2), renorm_aug23, renorm_aug24, renorm_sep10.
Launches dedupe by mint (earliest recv wins). Trades are kept in full.
"""
import argparse, json, os, sys

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sources", nargs="+", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)

    seen = {}
    n_launch_in = 0
    lf = open(os.path.join(a.out, "launches.jsonl"), "w", buffering=1 << 20)
    for src in a.sources:
        p = os.path.join(src, "launches.jsonl")
        if not os.path.exists(p):
            raise SystemExit(f"FATAL: missing {p}")
        for line in open(p):
            if not line.strip():
                continue
            n_launch_in += 1
            r = json.loads(line)
            m = r["mint"]
            if m not in seen or r["recv_unix_ms"] < seen[m]["recv_unix_ms"]:
                seen[m] = r
    for m, r in seen.items():
        lf.write(json.dumps(r, separators=(",", ":")) + "\n")
    lf.close()

    n_tr = 0
    with open(os.path.join(a.out, "trades.jsonl"), "w", buffering=1 << 20) as tf:
        for src in a.sources:
            p = os.path.join(src, "trades.jsonl")
            if not os.path.exists(p):
                raise SystemExit(f"FATAL: missing {p}")
            with open(p) as f:
                for line in f:
                    if line.strip():
                        tf.write(line if line.endswith("\n") else line + "\n")
                        n_tr += 1
    print(json.dumps({"launch_rows_in": n_launch_in, "distinct_mints": len(seen),
                      "trades_out": n_tr, "out": a.out}, indent=1))
    return 0

if __name__ == "__main__":
    sys.exit(main())
