#!/usr/bin/env python
"""Repair the `utility_reasoning` family (10,568 rows) and stop it hiding inside
`decision_action`.

WHAT IS WRONG. The system prompt pins a stale cost model by hand - "pump fee 100 bp each
side, slippage 50 bp, priority 8 bp, failure rate 0.10 charged 220 bp" - and the assistant
trace computes 2*100 + 50 + 8 + 0.10*220 and labels the result "210 bp". It sums to 280.
Every component is also unsupported by the measurements the rest of the corpus uses:

  * "pump fee 100 bp each side"  applied on a pumpswap pool (the venue's own schedule is
    30 bp/side; 94.63 bp is the measured curve figure),
  * "priority 8 bp"              measured at 0.099 bp/leg at 1 SOL,
  * "failure rate 0.10 charged 220 bp"  a failure charge the reward engine does not levy,
    so a prompt that adds it teaches a hurdle the labels never use,
  * quoting infrastructure prices  — the curve state on these rows is graduated/stale and
    the live pool is the AMM.

THE FIX. Read the venue and pool depth off the row's own prompt, price the clip through
`cost_authority` (the same authority as every other family), and render the trace from
that. The row keeps its reasoning shape - cost model, break-even, sizing table, checks,
verdict - but every number now re-derives. `meta.task` becomes `utility_reasoning` so the
family stops inflating `decision_action`.

USAGE
  python patch_utility_reasoning.py --dry-run --limit 300
  python patch_utility_reasoning.py --outdir /training/v2/candidate_sft_c10_utility_reasoning
  python patch_utility_reasoning.py --verify-only --outdir <that dir>
"""
import argparse
import collections
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cost_authority as CA  # noqa: E402

SRC = "/training/v2/candidate_sft_c8"
DEFAULT_OUT = "/training/v2/candidate_sft_c10_utility_reasoning"
SPLITS = ("train", "validation", "examination")

# --- identification: exactly the 10,568 rows the spec counts -------------------------
MARKER_A = "COST MODEL"
MARKER_B = "break-even"

# --- parsing: everything comes off the row's own prompt ------------------------------
RE_VENUE = re.compile(r"venue=(\S+)")
RE_AMM_DEPTH = re.compile(r"quote_reserves_sol=([0-9.eE+-]+)")
RE_CURVE_DEPTH = re.compile(r"v_sol_reserves_sol=([0-9.eE+-]+)")
RE_PRICE = re.compile(r"^price_lamports_per_raw_token=([0-9.eE+-]+)", re.M)
RE_RET30 = re.compile(r"ret_30s_bp=(-?[0-9.eE+-]+)")
RE_VOL30 = re.compile(r"vol_30s_bp=(-?[0-9.eE+-]+)")
RE_FOCUS = re.compile(r"for a ([0-9.]+) SOL position")

TIERS = (("SMALL", 0.25), ("MID", 0.50), ("FULL", 1.00))

SYS_TEXT = (
    "You are the trade-economics module for an on-chain memecoin desk. Given a strictly "
    "causal state snapshot, compute the executable entry economics from the pinned cost "
    "model: " + CA.model_statement() + " Show the arithmetic. Never use any future price: "
    "only the supplied state."
)
# Retired phrases this family must never carry again.
RETIRED = ("failure charge", "failure rate", "2*100", "pump fee 100 bp",
           "priority 8 bp", "slippage 50 bp", "charged 220 bp")


def regime_and_depth(user, stats):
    """Venue and pool depth, off the row's own prompt."""
    m = RE_VENUE.search(user)
    venue = m.group(1) if m else "unknown"
    regime = CA.regime_for(venue)
    if regime == "amm":
        d = RE_AMM_DEPTH.search(user)
        if d:
            stats["depth_from_amm"] += 1
            return regime, venue, float(d.group(1))
    d = RE_CURVE_DEPTH.search(user)
    if d:
        stats["depth_from_curve"] += 1
        return regime, venue, float(d.group(1))
    return regime, venue, None


def render(regime, venue, depth, entry, focus, ret30, vol30, stats):
    """The reasoning trace, every number re-derived from the authority."""
    opts = CA.size_options(regime, depth, uniform_venue_fee=False)
    rt = {k: v["round_trip_bp"] for k, v in opts["sizes"].items()}
    rt_focus = int(round(CA.decompose(focus, depth, regime)["round_trip_bp"]))
    venue_bp = opts["sizes"]["MID"]["venue_bp_per_leg"]
    be = entry * (1.0 + rt_focus / 1e4)

    lines = [
        f"COST MODEL: round_trip_bp = 2 * venue ({venue_bp:.2f} bp/side, {venue}, "
        f"measured) + 2 * clip impact (per-leg, constant product) + 2 * tx fee "
        f"(0.099 bp/leg, measured) = {rt_focus} bp for a {focus:.2f} SOL clip",
        f"ENTRY PRICE: {entry!r} lamports per raw token",
        f"BREAK-EVEN PRICE: {entry!r} * (1 + {rt_focus}/10000) = {be:.8f} "
        f"lamports per raw token",
        "SIZING TABLE (round-trip cost and break-even move by clip):",
    ]
    for name, clip in TIERS:
        cost_sol = clip * rt[name] / 1e4
        lines.append(f"  size={clip:.2f} SOL -> round-trip cost {cost_sol:.6f} SOL; "
                     f"move required to break even = {rt[name]} bp")
    if depth:
        lines.append(
            f"LIQUIDITY CHECK: pool depth {depth:.1f} SOL against a {focus:.2f} SOL clip "
            f"= {focus / depth * 1e4:.1f} bp of impact per leg; the clip is "
            f"{'inside' if focus / depth < 0.05 else 'large against'} the book.")
    else:
        lines.append("LIQUIDITY CHECK: no pool depth on the supplied state - impact "
                     "cannot be separated from the venue fee at this clock.")
    lines.append(
        f"VOLATILITY CHECK: price_volatility_30s_bp={vol30} and ret_30s_bp={ret30}; the "
        f"break-even move needed is {rt_focus} bp.")
    obs = max(ret30, 0.0)
    moves = [f"ret_30s_bp={ret30}"]
    if vol30 > rt_focus:
        moves.append(f"price_volatility_30s_bp={vol30}")
    viable = obs > rt_focus or vol30 > rt_focus
    if viable:
        which = "ret_30s_bp" if obs > rt_focus else "price_volatility_30s_bp"
        lines.append(f"ECONOMICS: viable on the supplied state - {which} exceeds the "
                     f"{rt_focus} bp round-trip cost.")
    else:
        lines.append(f"ECONOMICS: not viable on the supplied state - {' and '.join(moves)} "
                     f"does not exceed the {rt_focus} bp round-trip cost.")
    lines.append("EVIDENCE_STATUS: complete")
    return "\n".join(lines), rt, rt_focus, be


def patch_row(r, stats):
    fam = (r.get("meta") or {}).get("family")
    if fam != "decision":
        return None
    msgs = r["messages"]
    blob = "\n".join(m["content"] for m in msgs)
    if MARKER_A not in blob or MARKER_B not in blob:
        return None
    stats["identified"] += 1

    user = msgs[1]["content"]
    regime, venue, depth = regime_and_depth(user, stats)
    if depth is None:
        stats["no_depth"] += 1
        return None
    pm = RE_PRICE.search(user)
    if not pm:
        # the units block names the field without a leading-line anchor
        pm = re.search(r"price_lamports_per_raw_token=([0-9.eE+-]+)", user)
    if not pm:
        stats["no_price"] += 1
        return None
    entry = float(pm.group(1))
    if entry <= 0:
        stats["bad_price"] += 1
        return None
    fm = RE_FOCUS.search(user)
    focus = float(fm.group(1)) if fm else 0.5
    r30 = RE_RET30.search(user)
    ret30 = float(r30.group(1)) if r30 else 0.0
    v30 = RE_VOL30.search(user)
    vol30 = float(v30.group(1)) if v30 else 0.0

    trace, rt, rt_focus, be = render(regime, venue, depth, entry, focus, ret30, vol30,
                                     stats)

    msgs[0]["content"] = SYS_TEXT
    msgs[2]["content"] = trace
    meta = r["meta"]
    meta["task"] = "utility_reasoning"
    meta["cost_authority"] = {
        "regime": regime, "venue": venue, "depth_sol": depth,
        "round_trip_bp": rt, "round_trip_bp_focus": rt_focus,
        "break_even_price_lamports_per_raw": be,
        "authority": "cost_authority.size_options@uniform_venue_fee=False",
    }
    stats["regime_" + regime] += 1
    stats["venue_" + venue] += 1
    return 1


def verify(r):
    bad = []
    blob = "\n".join(m["content"] for m in r["messages"])
    for phrase in RETIRED:
        if phrase in blob:
            bad.append("retired:" + phrase)
    ca = (r["meta"].get("cost_authority") or {})
    if not ca:
        bad.append("no_cost_authority_block")
        return bad
    if (r["meta"].get("task") != "utility_reasoning"):
        bad.append("not_retagged")
    # every number in the trace must re-derive from the authority
    auth = CA.size_options(ca["regime"], ca["depth_sol"], uniform_venue_fee=False)
    got = {k: v["round_trip_bp"] for k, v in auth["sizes"].items()}
    if got != ca["round_trip_bp"]:
        bad.append("round_trip_not_from_authority")
    for name, clip in TIERS:
        if f"move required to break even = {ca['round_trip_bp'][name]} bp" not in blob:
            bad.append(f"sizing_row_missing:{name}")
    # break-even arithmetic
    exp = ca.get("break_even_price_lamports_per_raw")
    if exp is not None and f"{exp:.8f}" not in blob:
        bad.append("break_even_not_rendered")
    if "EVIDENCE_STATUS: complete" not in blob:
        bad.append("missing_evidence_status")
    return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--splits", nargs="+", default=list(SPLITS))
    ap.add_argument("--outdir", default=DEFAULT_OUT)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--verify-only", action="store_true")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    src = args.outdir if args.verify_only else SRC
    if not args.dry_run and not args.verify_only:
        os.makedirs(args.outdir, exist_ok=True)

    report = {"suites": {}, "corpus_revision": "c10_utility_reasoning_repair"}
    for split in args.splits:
        stats = collections.Counter()
        bad_rows = collections.Counter()
        rows_in = rows_out = 0
        fout = open(os.path.join(args.outdir, f"{split}.jsonl"), "w") \
            if (not args.dry_run and not args.verify_only) else None
        with open(os.path.join(src, f"{split}.jsonl")) as fh:
            for line in fh:
                rows_in += 1
                if args.limit and rows_in > args.limit:
                    break
                r = json.loads(line)
                if not args.verify_only:
                    if patch_row(r, stats) is None:
                        continue
                bad = verify(r)
                if bad:
                    for b in bad:
                        bad_rows[b.split(":")[0]] += 1
                    stats["rows_with_defect"] += 1
                rows_out += 1
                if fout:
                    fout.write(json.dumps(r, separators=(",", ":")) + "\n")
        if fout:
            fout.close()
        stat_out = {k: v for k, v in stats.items() if not k.startswith("venue_")}
        report["suites"][split] = {"rows_in": rows_in, "rows_out": rows_out,
                                   "defects": dict(bad_rows), "stats": stat_out,
                                   "venues": {k[6:]: v for k, v in stats.items()
                                              if k.startswith("venue_")}}
        print(split, rows_in, "->", rows_out, json.dumps(dict(bad_rows)), flush=True)
    out = ("/training/v2/reports/UTILITY_REASONING_VERIFY.json" if args.verify_only
           else "/training/v2/reports/UTILITY_REASONING_PASS.json")
    with open(out, "w") as fh:
        json.dump(report, fh, indent=1)
    tot = sum(sum(v["defects"].values()) for v in report["suites"].values())
    print("TOTAL_DEFECT_ROWS", tot, "->", out)


if __name__ == "__main__":
    main()