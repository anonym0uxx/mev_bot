#!/usr/bin/env python
"""Repair the `utility_regression` family (85,086 rows): the fourth and last family.

THREE DEFECTS, all confirmed on the corpus:

1. Stale cost. The system prompt pins "One-way execution cost is ~180 bp" - the retired
   figure, and a ONE-WAY statement where the family is scored on a round trip. This is the
   source of the 158,058 `180 bp` occurrences in train alone.
2. No state format. The prompt carries a CAUSAL STATE block only. Spec 5 requires the same
   ENRICHED CANDIDATE STATE / DEV HISTORY block the entry family carries, so the policy sees
   ONE state format across families and so a label can cite an enriched field (the B5 rule:
   a field the labels never cite is a field the model ignores).
3. Single-path realised targets. The forecast is the realised forward path, not the
   authority's own 300 s labels. Relabelled from `barrier_labels_c9_300s.jsonl` - the same
   300 s barrier authority the entry family is built on, so the forecasting family and the
   decision family cannot disagree about what a 300 s round trip does.

DEPTH COMES FROM THE ENTRY FAMILY. The utility prompt has no pool block, so the authority
cannot price a clip from it. Rather than invent a depth, the pass joins the entry family's
own `cost_authority` block for the SAME (mint, t_dec) - the two families then state the
same cost for the same clock, which is the whole point of one authority.

Also normalises the two-key row shape to the six-key shape the other families use.
"""
import argparse
import collections
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import cost_authority as CA  # noqa: E402

C8 = "/training/v2/candidate_sft_c8"
ENTRY = "/training/v2/candidate_sft_c10_entry_labeled"
BARRIER = "/training/v2/reports/barrier_labels_c9_300s.jsonl"
ENRICH = "/training/v2/reports/C9_ENRICHMENT_FULL.jsonl"
# c8 already stores the legacy vocabulary in meta.split; accept both so the map
SPLIT_LEGACY_RAW = {"train": "train", "validation": "val", "examination": "test",
                   "val": "val", "test": "test"}
FAMILY = "utility_regression"
CLIP_TIERS = (("SMALL", 0.25), ("MID", 0.50), ("FULL", 1.00))

RE_TDEC = re.compile(r"t_dec_ms=(\d+)")
RE_VENUE = re.compile(r"venue=(\w+)")
RE_RET300 = re.compile(r"ret_300s_bp=(-?[\d.]+)")
RE_NETFLOW = re.compile(r"net_flow_lamports=(-?[\d.]+)")
RE_BUYS = re.compile(r"buy_count=(\d+)")
RE_SELLS = re.compile(r"sell_count=(\d+)")
RE_TOP1 = re.compile(r"top1_trader_share=([\d.n/a]+)")
RE_PRICE = re.compile(r"price_lamports_per_raw_token=([\d.eE+-]+)")


def key_of(meta):
    eid = meta.get("episode_id") or ""
    parts = eid.split(":")
    if len(parts) < 3:
        return None
    try:
        return (parts[1], int(parts[2]))
    except ValueError:
        return None


def darkish(x):
    return x is None or (isinstance(x, str) and x.strip().lower() in ("", "n/a", "none", "null"))


def build_indices():
    """barrier labels + enrichment, keyed by (mint, t_dec_ms)."""
    bar, enr, depth = {}, {}, {}
    with open(BARRIER) as fh:
        for line in fh:
            r = json.loads(line)
            bar[(r["mint"], int(r["t_dec"]))] = r
    with open(ENRICH) as fh:
        for line in fh:
            r = json.loads(line)
            enr[(r["mint"], int(r["t_dec_ms"]))] = r
    for split in ("train", "validation", "examination"):
        p = os.path.join(ENTRY, f"{split}.jsonl")
        if not os.path.exists(p):
            continue
        with open(p) as fh:
            for line in fh:
                r = json.loads(line)
                ca = (r.get("meta") or {}).get("cost_authority") or {}
                # the entry family carries identity at the TOP level, not in meta
                k = key_of({"episode_id": r.get("episode_id")
                            or (r.get("meta") or {}).get("episode_id")})
                if ca and k:
                    depth[k] = {
                        "regime": ca.get("regime"), "depth_sol": ca.get("depth_sol"),
                        "venue": (r["meta"].get("venue") or "unknown"),
                    }
    return bar, enr, depth


def enrich_block(e):
    if not e:
        return None
    f = []
    for k, label in (("holders_at_t", "holders_at_t"),
                     ("top1_float_share", "top1_float_share"),
                     ("holder_hhi", "holder_hhi"),
                     ("mcap_sol_at_t", "mcap_sol_at_t"),
                     ("bundle_wallets", "bundle_wallets"),
                     ("round_trip_wallets", "round_trip_wallets"),
                     ("creator_past_launches", "creator_past_launches"),
                     ("creator_known", "creator_known"),
                     ("wash_ratio", "wash_ratio")):
        v = e.get(k)
        if not darkish(v):
            f.append(f"{label}={v}")
    if not f:
        return None
    return "ENRICHED CANDIDATE STATE: " + " ".join(f)


def dev_block(e):
    if not e:
        return None
    f = []
    for k in ("creator_past_launches", "creator_known", "bundle_wallets", "wash_ratio"):
        v = e.get(k)
        if not darkish(v):
            f.append(f"{k}={v}")
    if not f:
        return None
    return "DEV HISTORY: " + " ".join(f)


def patch_row(r, stats, bar, enr, depth):
    meta = r["meta"]
    key = key_of(meta)
    if key is None:
        stats["no_key"] += 1
        return None
    b = bar.get(key)
    if b is None:
        stats["no_barrier_label"] += 1
        return None
    e = enr.get(key)
    if e is None:
        stats["no_enrichment"] += 1
    d = depth.get(key)
    if d is None or not d.get("depth_sol"):
        stats["no_depth_from_entry"] += 1
        return None
    regime = d["regime"] or ("amm" if d["venue"] == "pumpswap" else "bonding_curve")

    u = r["messages"][1]["content"]
    t_dec = key[1]
    opts = CA.size_options(regime, d["depth_sol"], uniform_venue_fee=False)
    cost_line = CA.line(CLIP_TIERS[1][1], d["depth_sol"], regime, uniform_venue_fee=False)

    # --- prompt: identical state format to the entry family, plus the market/cost block
    lines = [ln for ln in u.split("\n")]
    out = []
    placed = False
    for ln in lines:
        out.append(ln)
        if ln.startswith("  ret_") and not placed:
            out.append("")
            out.append(f"MARKET STATE (at decision time): venue={d['venue']} regime={regime} "
                       f"pool_depth_sol={d['depth_sol']:.1f}")
            out.append(cost_line)
            eb = enrich_block(e)
            if eb:
                out.append(eb)
            db = dev_block(e)
            if db:
                out.append(db)
            placed = True
    u2 = "\n".join(out)
    if not placed:
        stats["prompt_block_not_injected"] += 1
        return None
    # size menu, same generator as every other family
    u2 = u2.rstrip("\n") + "\n" + opts["block"] + "\n"

    sys_old = r["messages"][0]["content"]
    sys_new = ("You are an on-chain opportunity forecaster for pump.fun/pumpswap memecoins. "
               "Estimate the executable 300 s outcome from the causal snapshot. "
               + CA.model_statement() + " Answer in the fixed format: FORECAST_NET_BP, "
               "FORECAST_MFE_BP, FORECAST_MAE_BP, FORECAST_OUTCOME, FORECAST_CENSORED, "
               "BASIS, EVIDENCE_STATUS.")
    if "~180 bp" in sys_old or "one-way execution cost" in sys_old:
        stats["system_cost_repaired"] += 1

    # --- target: the 300 s barrier authority's own numbers
    mfe = b.get("mfe_bp")
    mae = b.get("mae_bp")
    net = b.get("last_bp")
    if any(v is None for v in (mfe, mae, net)):
        stats["incomplete_label"] += 1
        return None
    cite = []
    if e:
        for k in ("holders_at_t", "top1_float_share", "creator_past_launches", "mcap_sol_at_t"):
            if not darkish(e.get(k)):
                cite.append(f"{k}={e[k]}")
    basis_parts = []
    nf = RE_NETFLOW.search(u)
    if nf:
        basis_parts.append(f"net_flow_lamports={nf.group(1)}")
    bq = RE_BUYS.search(u)
    sq = RE_SELLS.search(u)
    if bq and sq:
        basis_parts.append(f"{bq.group(1)} buys vs {sq.group(1)} sells")
    basis_parts.extend(cite[:3])
    floor = CA.decompose(CLIP_TIERS[1][1], d["depth_sol"], regime,
                         uniform_venue_fee=False)["round_trip_bp"]
    a2 = (f"FORECAST_NET_BP: {net}\n"
          f"FORECAST_MFE_BP: {mfe}\n"
          f"FORECAST_MAE_BP: {mae}\n"
          f"FORECAST_OUTCOME: {b.get('outcome')}\n"
          f"FORECAST_CENSORED: {str(bool(b.get('censored'))).lower()}\n"
          f"COST_FLOOR_BP: {floor}\n"
          f"BASIS: " + "; ".join(basis_parts) + "\n"
          f"EVIDENCE_STATUS: complete")

    r["messages"][0]["content"] = sys_new
    r["messages"][1]["content"] = u2
    r["messages"][2]["content"] = a2
    meta["family"] = FAMILY
    meta["task"] = "decision_reasoning"
    meta["horizon_ms"] = int(b.get("horizon_ms") or 300000)
    meta["truth_type"] = "barrier_triplet_v3_300s"
    meta["venue"] = d["venue"]
    meta["cost_authority"] = {
        "regime": regime, "depth_sol": d["depth_sol"], "round_trip_bp": floor,
        "venue_bp_per_leg": CA.decompose(CLIP_TIERS[1][1], d["depth_sol"], regime,
                                         uniform_venue_fee=False)["venue_bp_per_leg"],
        "statement": cost_line, "authority": "cost_authority@venue_resolved",
        "depth_source": "entry family, same (mint, t_dec)",
    }
    meta["enrichment_status"] = "injected" if e else "unavailable"
    # six-key row shape
    r["episode_id"] = meta.get("episode_id")
    r["family"] = FAMILY
    r["mint"] = meta.get("mint")
    r["split"] = SPLIT_LEGACY_RAW.get(meta.get("split"), "train")
    stats["emitted"] += 1
    return 0


def verify(r):
    bad = []
    m = r["meta"]
    a = r["messages"][2]["content"]
    blob = "\n".join(x["content"] for x in r["messages"])
    for k in ("FORECAST_NET_BP:", "FORECAST_MFE_BP:", "FORECAST_MAE_BP:", "BASIS:",
              "EVIDENCE_STATUS:"):
        if k not in a:
            bad.append("missing_target_field:" + k)
    # Retired CONTEXTS only: a row whose own calibrated floor is 180 bp renders
    # "round trip 180 bp" legitimately. Grepping the bare number flags correct rows -
    # this is the fourth time this exact false positive has had to be removed.
    if "~180 bp" in blob or "one-way execution cost" in blob:
        bad.append("retired_cost_figure")
    ca = m.get("cost_authority")
    if not isinstance(ca, dict):
        bad.append("no_cost_authority")
    else:
        if ca["statement"] not in r["messages"][1]["content"]:
            bad.append("cost_statement_not_in_prompt")
        if f"COST_FLOOR_BP: {ca['round_trip_bp']}" not in a:
            bad.append("floor_not_in_target")
    if "ENRICHED CANDIDATE STATE:" not in r["messages"][1]["content"]:
        bad.append("no_enrichment_block")
    if m.get("family") != FAMILY or m.get("task") != "decision_reasoning":
        bad.append("bad_family_or_task_tag")
    if r.get("family") != FAMILY:
        bad.append("top_level_family_not_set")
    if not m.get("mint") or not m.get("episode_id"):
        bad.append("missing_identity")
    if m.get("enrichment_status") == "injected" and "holders_at_t" not in a:
        bad.append("label_cites_no_enriched_field")
    return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--splits", nargs="+",
                    default=["train", "validation", "examination"])
    ap.add_argument("--outdir", default="/training/v2/candidate_sft_c10_utility_regression")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--verify-only", action="store_true")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    bar, enr, depth = build_indices()
    print(f"indices: barrier={len(bar)} enrichment={len(enr)} entry_depth={len(depth)}",
          flush=True)
    src = args.outdir if args.verify_only else C8
    if not args.dry_run and not args.verify_only:
        os.makedirs(args.outdir, exist_ok=True)
    report = {"suites": {}, "corpus": "c10_utility_regression"}
    total = 0
    for split in args.splits:
        st = collections.Counter()
        bad_rows = collections.Counter()
        fout = None
        if not args.dry_run and not args.verify_only:
            fout = open(os.path.join(args.outdir, f"{split}.jsonl"), "w")
        n_in = 0
        with open(os.path.join(src, f"{split}.jsonl")) as fh:
            for line in fh:
                r = json.loads(line)
                if (r.get("meta") or {}).get("family") not in (FAMILY,) and not args.verify_only:
                    continue
                n_in += 1
                if args.limit and n_in > args.limit:
                    break
                if not args.verify_only:
                    if patch_row(r, st, bar, enr, depth) is None:
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
        report["suites"][split] = {"rows_in": n_in, "stats": dict(st),
                                   "defects": dict(bad_rows)}
        print(split, "in", n_in, "out", st["rows_out"], "defects", dict(bad_rows),
              flush=True)
    out = ("/training/v2/reports/UTILITY_REGRESSION_VERIFY.json" if args.verify_only
           else "/training/v2/reports/UTILITY_REGRESSION.json")
    json.dump(report, open(out, "w"), indent=1)
    print("TOTAL_DEFECT_ROWS", total, "->", out)
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())