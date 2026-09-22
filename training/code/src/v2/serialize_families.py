#!/usr/bin/env python
"""Stage 5 (final) — multi-family candidate builder.

Families (per LOSS_BUDGET.md §2):
  decision          : one action per causal state (BUY/WATCH/SKIP)
  utility_reasoning : deterministic entry-economics reasoning over the same causal
                      state (cost floor, break-even, sizing, invalidation)

No family may cite a future quantity. Both are built from the same causal state;
neither inspects the realized forward return.

Outputs: <out>/{train,validation,test}.jsonl  +  reports/TOKEN_CENSUS_ACTUAL.json
"""
import argparse, json, hashlib, sys, os, collections

INPUT_KEYS = ["mint_tag", "t_dec_ms", "interval_s", "n_prior_trades", "price_sol_per_raw",
              "age_s", "last_trade_age_s", "buy_count", "sell_count", "unique_traders",
              "sol_volume_lamports", "buy_volume_lamports", "sell_volume_lamports",
              "net_flow_lamports", "ret_5s_bp", "ret_30s_bp", "price_volatility_30s_bp",
              "top1_trader_share", "top5_trader_share", "buyer_seller_ratio", "venue",
              "curve_venue_present", "evidence_status"]

SYS_DECISION = ("You are an on-chain opportunity assessor for pump.fun / pumpswap memecoins. "
                "You receive a strictly causal state snapshot measured at a decision time and "
                "must choose exactly one action: BUY, WATCH or SKIP. "
                "Round-trip cost (fees + slippage + priority + failure charge) is 210 bp. "
                "A BUY is only correct if the expected move can clear that cost. "
                "Answer in the fixed format: DECISION, then SIZE/PRICE LIMIT (BUY only), "
                "INVALIDATION, EVIDENCE (2-4 facts citing supplied fields), COUNTEREVIDENCE, "
                "EVIDENCE_STATUS.")

SYS_UTIL = ("You are the trade-economics module for an on-chain memecoin desk. "
            "Given a strictly causal state snapshot, compute the executable entry economics "
            "from the pinned cost model: pump fee 100 bp each side, slippage 50 bp, priority "
            "8 bp, failure rate 0.10 charged 220 bp. Show the arithmetic. "
            "Never use any future price: only the supplied state.")


class St:
    def __init__(self, s, dc):
        self.t_dec_ms = dc["t_dec_ms"]; self.interval_s = dc["interval_s"]
        self.n_prior_trades = dc["n_prior_trades"]
        self.price = s["price_sol_per_raw"]; self.age_s = s["age_s"]
        self.last_age = s["last_trade_age_s"]; self.nbuy = s["buy_count"]; self.nsell = s["sell_count"]
        self.uniq = s["unique_traders"]; self.solvol = s["sol_volume_lamports"]
        self.buyvol = s["buy_volume_lamports"]; self.sellvol = s["sell_volume_lamports"]
        self.netflow = s["net_flow_lamports"]; self.r5 = s["ret_5s_bp"]; self.r30 = s["ret_30s_bp"]
        self.vol = s["price_volatility_30s_bp"]; self.top1 = s["top1_trader_share"]
        self.top5 = s["top5_trader_share"]; self.bs = s["buyer_seller_ratio"]
        self.venue = s["venue"]; self.cutv = s["curve_venue_present"]; self.estat = s["evidence_status"]

    def to_input(self, tag):
        return {"mint_tag": tag, "t_dec_ms": self.t_dec_ms, "interval_s": self.interval_s,
                "n_prior_trades": self.n_prior_trades, "price_sol_per_raw": self.price,
                "age_s": rd(self.age_s, 1), "last_trade_age_s": rd(self.last_age, 2),
                "buy_count": self.nbuy, "sell_count": self.nsell, "unique_traders": self.uniq,
                "sol_volume_lamports": self.solvol, "buy_volume_lamports": self.buyvol,
                "sell_volume_lamports": self.sellvol, "net_flow_lamports": self.netflow,
                "ret_5s_bp": rd(self.r5), "ret_30s_bp": rd(self.r30),
                "price_volatility_30s_bp": rd(self.vol), "top1_trader_share": rd(self.top1),
                "top5_trader_share": rd(self.top5), "buyer_seller_ratio": rd(self.bs),
                "venue": self.venue, "curve_venue_present": self.cutv,
                "evidence_status": self.estat}


def rd(x, n=5):
    return None if x is None else round(float(x), n)


def fnum(x):
    if x is None:
        return "n/a"
    a = abs(x)
    if a >= 1e9:
        return f"{x/1e9:.2f}e9"
    if a >= 1e6:
        return f"{x/1e6:.2f}e6"
    if a >= 1e3:
        return f"{x:.0f}"
    return f"{x:.6g}"


def render(inp):
    f = fnum
    return "\n".join([
        f"t_dec_ms={inp['t_dec_ms']}  age_s={inp['age_s']}  last_trade_age_s={inp['last_trade_age_s']}",
        f"venue={inp['venue']}  curve_present={inp['curve_venue_present']}  evidence_status={inp['evidence_status']}",
        f"n_prior_trades={inp['n_prior_trades']}  buy_count={inp['buy_count']}  sell_count={inp['sell_count']}  unique_traders={inp['unique_traders']}",
        f"price_sol_per_raw={inp['price_sol_per_raw']}  ret_5s_bp={f(inp['ret_5s_bp'])}  ret_30s_bp={f(inp['ret_30s_bp'])}  vol_30s_bp={f(inp['price_volatility_30s_bp'])}",
        f"buy_volume_lamports={inp['buy_volume_lamports']}  sell_volume_lamports={inp['sell_volume_lamports']}  net_flow_lamports={inp['net_flow_lamports']}",
        f"top1_trader_share={f(inp['top1_trader_share'])}  top5_trader_share={f(inp['top5_trader_share'])}  buyer_seller_ratio={f(inp['buyer_seller_ratio'])}",
    ])


COST = 210.0


def decision_completion(action, inp):
    net = inp["net_flow_lamports"] or 0
    flow = "inflow" if net > 0 else "outflow"
    conc = inp["top1_trader_share"]
    age = inp["last_trade_age_s"] or 0
    if action == "BUY":
        body = [
            "DECISION: BUY",
            "SIZE: 0.5 SOL",
            f"PRICE LIMIT: {inp['price_sol_per_raw']}",
            f"INVALIDATION: exit if net_flow_lamports turns negative or last_trade_age_s > {max(3.0, age*3):.1f}",
            f"EVIDENCE: round-trip cost floor 210 bp must be cleared; sustained {flow} of {inp['net_flow_lamports']} lamports "
            f"with {inp['buy_count']} buys vs {inp['sell_count']} sells; top1 share {conc} "
            f"({'concentrated' if (conc or 0) > 0.5 else 'distributed'})",
            "COUNTEREVIDENCE: single large seller can exhaust the book; memecoin reversals are common",
            "EVIDENCE_STATUS: " + inp["evidence_status"],
        ]
    elif action == "WATCH":
        body = [
            "DECISION: WATCH",
            "INVALIDATION: re-evaluate when net_flow_lamports > 0 while buy_count > sell_count, or last_trade_age_s > 5",
            f"EVIDENCE: net_flow_lamports={inp['net_flow_lamports']} is not decisively positive against the 210 bp cost floor; "
            f"{inp['buy_count']} buys vs {inp['sell_count']} sells; last trade {inp['last_trade_age_s']}s ago",
            "COUNTEREVIDENCE: quiet books can re-rate quickly; waiting risks missing the move",
            "EVIDENCE_STATUS: " + inp["evidence_status"],
        ]
    else:
        body = [
            "DECISION: SKIP",
            "INVALIDATION: none for this snapshot; only a new snapshot with positive net flow changes it",
            f"EVIDENCE: net_flow_lamports={inp['net_flow_lamports']} with {inp['sell_count']} sells vs {inp['buy_count']} buys; "
            f"top1 share {conc} indicates {'concentration risk' if (conc or 0) > 0.5 else 'thin participation'}; "
            f"the 210 bp cost floor is not reachable from this state",
            "COUNTEREVIDENCE: none material",
            "EVIDENCE_STATUS: " + inp["evidence_status"],
        ]
    return "\n".join(body)


def utility_completion(inp):
    p = inp["price_sol_per_raw"]
    if not p or p <= 0:
        p = 0.0
    be = p * (1 + COST / 10000.0)
    lines = [
        "COST MODEL: round_trip_bp = 2*100 (pump fees) + 50 (slippage) + 8 (priority) + 0.10*220 (failure charge) = 210 bp",
        f"ENTRY PRICE: {p} SOL per raw token",
        f"BREAK-EVEN PRICE: {p} * (1 + 210/10000) = {be:.8f} SOL per raw token",
        "SIZING TABLE (break-even and worst-case loss if stopped on the cost floor):",
    ]
    for size in (0.1, 0.5, 1.0):
        fee = size * COST / 10000.0
        lines.append(f"  size={size} SOL -> round-trip cost {fee:.4f} SOL; move required to break even = 210 bp")
    lines.append(f"LIQUIDITY CHECK: prior volume {inp['sol_volume_lamports']} lamports over "
                 f"{inp['n_prior_trades']} trades; a 0.5 SOL order must not exceed the cost budget of "
                 f"{0.5*COST/10000.0:.4f} SOL in adverse slippage.")
    lines.append(f"FLOW CHECK: buy_volume_lamports={inp['buy_volume_lamports']} vs sell_volume_lamports={inp['sell_volume_lamports']} "
                 f"-> net_flow_lamports={inp['net_flow_lamports']}; "
                 f"top1_trader_share={inp['top1_trader_share']} vs top5={inp['top5_trader_share']}.")
    lines.append(f"VOLATILITY CHECK: price_volatility_30s_bp={inp['price_volatility_30s_bp']} and "
                 f"ret_30s_bp={inp['ret_30s_bp']}; the break-even move needed is 210 bp.")
    verdict = ("ECONOMICS: viable — 30s drift and flow are consistent with clearing 210 bp."
               if (inp["ret_30s_bp"] or 0) > 210 and (inp["net_flow_lamports"] or 0) > 0
               else "ECONOMICS: not viable on the supplied state — the observable move does not exceed the 210 bp floor.")
    lines.append(verdict)
    lines.append("EVIDENCE_STATUS: " + inp["evidence_status"])
    return "\n".join(lines)


def sha(s):
    return hashlib.sha256(s.encode()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--states", required=True)
    ap.add_argument("--labels", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-per-mint", type=int, default=100000)
    ap.add_argument("--families", default="decision,utility")
    ap.add_argument("--utility-every", type=int, default=4,
                    help="emit the utility_reasoning family for 1 episode in K (scope control)")
    a = ap.parse_args()
    fams = set(a.families.split(","))
    os.makedirs(a.out, exist_ok=True)
    os.makedirs("reports", exist_ok=True)

    labels = {}
    with open(a.labels) as f:
        for line in f:
            if line.strip():
                r = json.loads(line)
                labels[r["episode_id"]] = r

    by_mint = collections.defaultdict(list)
    n_read = 0
    with open(a.states) as f:
        for line in f:
            if not line.strip():
                continue
            r = json.loads(line); n_read += 1
            lb = labels.get(r["identity"]["episode_id"])
            if not lb or lb["action"] is None:
                continue
            by_mint[r["identity"]["mint"]].append((r, lb))
    if n_read == 0:
        raise SystemExit("FATAL: zero states read")

    files = {s: open(f"{a.out}/{s}.jsonl", "w", buffering=1 << 20) for s in ("train", "validation", "test")}
    counts = collections.Counter(); famcount = collections.Counter(); acount = collections.Counter()
    per_mint = []

    for mint, eps in by_mint.items():
        eps.sort(key=lambda x: sha(x[0]["identity"]["episode_id"]))
        chosen = eps[:a.max_per_mint]
        per_mint.append(len(chosen))
        tag = "mint_" + sha(mint)[:10]
        for (r, lb) in chosen:
            st = St(r["state"], r["decision_clock"])
            inp = st.to_input(tag)
            assert set(inp.keys()) == set(INPUT_KEYS)
            sp = "validation" if lb["split"] == "val" else ("test" if lb["split"] == "test" else "train")
            user_common = "DECISION CLOCK — assess this opportunity.\n" + render(inp) + "\n\n"
            recs = []
            if "decision" in fams:
                recs.append(("decision", SYS_DECISION,
                             user_common + "Choose exactly one action: BUY, WATCH, SKIP. Respect the 210 bp "
                                           "round-trip cost. Answer in the fixed format.",
                             decision_completion(lb["action"], inp)))
            if "utility" in fams and (int(hashlib.sha256(lb["episode_id"].encode()).hexdigest(), 16) % max(a.utility_every, 1) == 0):
                recs.append(("utility_reasoning", SYS_UTIL,
                             user_common + "Compute the executable entry economics for a 0.5 SOL position: "
                                           "cost floor, break-even price, sizing, and whether the observed move "
                                           "can clear the cost. Show the arithmetic.",
                             utility_completion(inp)))
            for fam, sysm, usr, asst in recs:
                rec = {"episode_id": lb["episode_id"], "mint": mint, "split": lb["split"],
                       "family": fam,
                       "messages": [{"role": "system", "content": sysm},
                                    {"role": "user", "content": usr},
                                    {"role": "assistant", "content": asst}],
                       "meta": {"action": lb["action"] if fam == "decision" else None,
                                "truth_type": lb["truth_type"], "label_status": lb["label_status"],
                                "builder": "serialize_families_v2.0"}}
                files[sp].write(json.dumps(rec, separators=(",", ":")) + "\n")
                counts[sp] += 1; famcount[f"{sp}:{fam}"] += 1
                if fam == "decision":
                    acount[f"{sp}:{lb['action']}"] += 1
    for f in files.values():
        f.close()
    if sum(counts.values()) == 0:
        raise SystemExit("FATAL: zero records")

    used = sum(per_mint)
    json.dump({"schema": "north_star_serialize_v2", "states_read": n_read,
               "records": dict(counts), "families": dict(famcount),
               "actions": dict(acount), "mints": len(by_mint),
               "episodes_per_mint_mean": round(used / max(len(per_mint), 1), 2),
               "max_per_mint": a.max_per_mint},
              open("reports/SERIALIZE_SUMMARY.json", "w"), indent=1)
    print(f"[FAMILIES] states={n_read:,} records={dict(counts)}")
    print(f"[FAMILIES] families={dict(famcount)}")
    print(f"[FAMILIES] mean episodes/mint={used/max(len(per_mint),1):.1f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
