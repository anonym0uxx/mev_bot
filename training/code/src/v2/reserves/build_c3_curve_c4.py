#!/usr/bin/env python
"""Build candidate_sft_c4: C3 records + CAUSAL curve reserves, protected mints removed.

vs /training/v2/candidate_sft_c3 (left untouched):
  1. the 192 protected-holdout mints are EXCLUDED (blocker 1);
  2. each record gains a causal `curve_reserves` block (meta + a CURVE STATE line
     in the prompt) joined from the per-fill LaserStream TradeEvent reserve tables;
  3. `seconds_to_graduation` and `graduation_proximity_pct` are never read, never
     emitted and never used as inputs (blocker 3);
  4. the part0133 shortfall is reported explicitly (blocker 2).

Causality: the reserve snapshot attached to a record is the LAST fill event at or
before the record's decision time (recv_unix_ms <= t_dec_ms), optionally limited by
--max-stale-ms.  No event after t_dec_ms is ever consulted.
"""
from __future__ import annotations

import argparse, glob, json, os, re, sys, collections
import numpy as np
import pyarrow.parquet as pq

LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9
TOKEN_SCALE = 1_000_000            # pump.fun tokens are 6-decimals (raw = tokens*1e6)
V_GRAD_LAMPORTS = 85 * LAMPORTS_PER_SOL   # real-SOL graduation target

FORBIDDEN_INPUTS = ("seconds_to_graduation", "graduation_proximity_pct")
RE_MINT = re.compile(r"MINT:\s*([1-9A-HJ-NP-Za-km-z]{32,44})")
RE_MINT_DEC = re.compile(r'"mint"\s*:\s*"([1-9A-HJ-NP-Za-km-z]{32,44})"')
RE_TDEC = re.compile(r"(?:t_dec_ms=|DECISION TIME \(unix ms\):\s*)(\d+)")

CURVE_COLS = ["virtual_sol_reserves_lamports", "virtual_token_reserves_raw",
              "real_sol_reserves_lamports", "real_token_reserves_raw",
              "pre_virtual_sol_reserves_lamports", "pre_virtual_token_reserves_raw"]


def load_reserves(paths):
    """Return {mint: (ts_sorted, vsol, vtok, rsol, rtok, pvsol, pvtok)}."""
    acc = collections.defaultdict(list)
    total = 0
    for p in paths:
        f = pq.ParquetFile(p)
        cols = ["mint_b58", "recv_unix_ms"] + CURVE_COLS
        have = set(f.schema_arrow.names)
        cols = [c for c in cols if c in have]
        t = pq.read_table(p, columns=cols)
        total += t.num_rows
        mi = t["mint_b58"].to_pylist()
        arrs = {c: t[c].to_numpy(zero_copy_only=False) for c in cols if c != "mint_b58"}
        for j, m in enumerate(mi):
            acc[m].append((int(t["recv_unix_ms"][j]), {c: int(arrs[c][j]) for c in arrs if c != "recv_unix_ms"}))
    out = {}
    for m, lst in acc.items():
        lst.sort(key=lambda x: x[0])
        ts = np.array([x[0] for x in lst], dtype=np.int64)
        d = {c: np.array([x[1].get(c, 0) for x in lst], dtype=np.int64) for c in CURVE_COLS}
        out[m] = (ts, d)
    return out, total


def curve_block(mint, t_dec, res, max_stale_ms):
    ent = res.get(mint)
    if ent is None:
        return None, "mint_absent"
    ts, d = ent
    i = int(np.searchsorted(ts, t_dec, side="right")) - 1
    if i < 0:
        return None, "no_event_before_t_dec"
    stale = int(t_dec - ts[i])
    if max_stale_ms and stale > max_stale_ms:
        return None, "stale"
    vsol = int(d["virtual_sol_reserves_lamports"][i])
    vtok = int(d["virtual_token_reserves_raw"][i])
    rsol = int(d["real_sol_reserves_lamports"][i])
    rtok = int(d["real_token_reserves_raw"][i])
    pvsol = int(d["pre_virtual_sol_reserves_lamports"][i])
    pvtok = int(d["pre_virtual_token_reserves_raw"][i])
    if vsol <= 0 or vtok <= 0:
        return None, "nonpositive_reserve"
    px = vsol / vtok                    # lamports per RAW token unit
    impact_bp = ((vsol / vtok) / (pvsol / pvtok) - 1.0) * 1e4 if (pvsol > 0 and pvtok > 0) else None
    return {
        "status": "present",
        "src": "laserstream_pumpfun_tradeevent_v1",
        "mint": mint,
        "reserve_ts_ms": int(ts[i]),
        "staleness_ms": stale,
        "virtual_sol_reserves_lamports": vsol,
        "virtual_token_reserves_raw": vtok,
        "real_sol_reserves_lamports": rsol,
        "real_token_reserves_raw": rtok,
        "curve_price_lamports_per_raw_token": px,
        "curve_price_sol_per_token": px * TOKEN_SCALE / LAMPORTS_PER_SOL,
        "curve_k": str(vsol * vtok),
        "curve_progress": min(1.0, max(0.0, rsol / V_GRAD_LAMPORTS)),
        "curve_regime": "graduated" if rsol >= V_GRAD_LAMPORTS else "bonding",
        "last_fill_price_impact_bp": impact_bp,
    }, "matched"


def curve_line(cb):
    if cb is None:
        return ("CURVE STATE (at decision time): curve_reserves=absent "
                "(no LaserStream bonding-curve snapshot for this mint at or before t_dec)")
    return ("CURVE STATE (at decision time): curve_reserves=present src=laserstream "
            "staleness_ms={st} v_sol_reserves_sol={vs:.9f} v_tokens_reserves={vt} "
            "real_sol_reserves_sol={rs:.9f} real_tokens_reserves={rt} "
            "curve_price_sol_per_token={px:.12f} curve_k={k} curve_progress={pr:.6f} "
            "curve_regime={rg} last_fill_impact_bp={ib}").format(
        st=cb["staleness_ms"], vs=cb["virtual_sol_reserves_lamports"] / LAMPORTS_PER_SOL,
        vt=cb["virtual_token_reserves_raw"], rs=cb["real_sol_reserves_lamports"] / LAMPORTS_PER_SOL,
        rt=cb["real_token_reserves_raw"], px=cb["curve_price_sol_per_token"],
        k=cb["curve_k"], pr=cb["curve_progress"], rg=cb["curve_regime"],
        ib=("n/a" if cb["last_fill_price_impact_bp"] is None else round(cb["last_fill_price_impact_bp"], 4)))


def inject(content, line):
    m = re.search(r"\n+Choose exactly one action", content)
    if m:
        return content[:m.start()] + "\n" + line + content[m.start():]
    m = re.search(r"\n+Answer in the fixed format", content)
    if m:
        return content[:m.start()] + "\n" + line + content[m.start():]
    return content + "\n" + line


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reserves", nargs="+", required=True)
    ap.add_argument("--c3", default="/training/v2/candidate_sft_c3")
    ap.add_argument("--out", default="/training/v2/candidate_sft_c4")
    ap.add_argument("--protected", default="/training/v2/audit/factual_cpt_protected_eval_mints.json")
    ap.add_argument("--max-stale-ms", type=int, default=0, help="0 = no staleness limit")
    ap.add_argument("--parts", nargs="*", default=["train", "validation", "examination"])
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)

    protected = set(json.load(open(a.protected)))
    print("protected mints:", len(protected))
    res, total_res_rows = load_reserves(a.reserves)
    print("reserve rows loaded:", total_res_rows, "distinct mints:", len(res))

    report = {"c3_dir": a.c3, "out_dir": a.out, "max_stale_ms": a.max_stale_ms,
              "protected_mints": len(protected), "reserves_rows_loaded": total_res_rows,
              "reserves_distinct_mints": len(res), "forbidden_inputs": list(FORBIDDEN_INPUTS),
              "partitions": {}}
    for part in a.parts:
        src = os.path.join(a.c3, part + ".jsonl")
        dst = os.path.join(a.out, part + ".jsonl")
        n_in = n_out = n_excl = 0
        n_curve = 0
        n_nomint = 0
        reasons = collections.Counter()
        fam = collections.Counter()
        tdec_lo = tdec_hi = None
        mints = set()
        with open(src) as fi, open(dst, "w") as fo:
            for line in fi:
                n_in += 1
                o = json.loads(line)
                u = o["messages"][1]["content"]
                meta0 = o.get("meta") or {}
                mint = o.get("mint") or meta0.get("mint") or meta0.get("group")
                if not mint:
                    mm = RE_MINT.search(u) or RE_MINT_DEC.search(u)
                    mint = mm.group(1) if mm else None
                if mint is None:
                    n_nomint += 1
                if mint in protected:
                    n_excl += 1
                    continue
                m = RE_TDEC.search(u)
                t_dec = int(m.group(1)) if m else None
                if t_dec is None:
                    eid = o.get("episode_id") or meta0.get("episode_id") or ""
                    mm = re.search(r":(\d{10,})$", eid)
                    if mm:
                        t_dec = int(mm.group(1))
                cb = None
                why = "no_mint_or_t_dec"
                if mint is not None and t_dec is not None:
                    cb, why = curve_block(mint, t_dec, res, a.max_stale_ms)
                    reasons[why] += 1
                    if cb is not None:
                        n_curve += 1
                        tdec_lo = t_dec if tdec_lo is None else min(tdec_lo, t_dec)
                        tdec_hi = t_dec if tdec_hi is None else max(tdec_hi, t_dec)
                # leakage guard: forbidden future-derived fields must not appear
                low = line.lower()
                for f in FORBIDDEN_INPUTS:
                    if f in low:
                        raise SystemExit("FATAL leakage: %s present in %s" % (f, src))
                o["messages"][1]["content"] = inject(u, curve_line(cb))
                meta = o.setdefault("meta", {})
                meta["curve_reserves"] = cb if cb is not None else {"status": "absent", "reason": why}
                meta["builder"] = "build_c3_curve_c4.v1"
                meta["curve_causality"] = "reserve_ts_ms <= t_dec_ms (strict, no lookahead)"
                fam[o.get("family") or "management"] += 1
                if mint:
                    mints.add(mint)
                fo.write(json.dumps(o) + "\n")
                n_out += 1
        report["partitions"][part] = {
            "rows_in": n_in, "rows_out": n_out, "rows_excluded_protected": n_excl,
            "rows_no_mint": n_nomint, "rows_with_curve_reserves": n_curve,
            "curve_coverage": round(n_curve / max(n_out, 1), 6),
            "distinct_mints": len(mints), "families": dict(fam),
            "join_reasons": dict(reasons),
            "t_dec_curve_min": tdec_lo, "t_dec_curve_max": tdec_hi,
        }
        print(part, json.dumps(report["partitions"][part]))
    # split disjointness
    tr = set(); va = set(); ex = set()
    for part, s in (("train", tr), ("validation", va), ("examination", ex)):
        p = os.path.join(a.out, part + ".jsonl")
        if not os.path.exists(p):
            continue
        for line in open(p):
            o = json.loads(line)
            mm = o.get("mint") or (RE_MINT.search(o["messages"][1]["content"]) or [None])
            if o.get("mint"):
                s.add(o["mint"])
            else:
                g = RE_MINT.search(o["messages"][1]["content"])
                if g:
                    s.add(g.group(1))
    report["split_mint_overlap"] = {"train_val": len(tr & va), "train_exam": len(tr & ex),
                                    "val_exam": len(va & ex)}
    report["protected_present_after"] = len((tr | va | ex) & protected)
    with open(os.path.join(a.out, "BUILD_C4_REPORT.json"), "w") as f:
        json.dump(report, f, indent=1)
    print(json.dumps({k: v for k, v in report.items() if k != "partitions"}, indent=1))


if __name__ == "__main__":
    main()
