#!/usr/bin/env python
"""Stage 5 — serializer + candidate build.

Joins states.jsonl + labels.jsonl by episode_id and emits decision-first SFT
records. The model input is built ONLY from the allowlist in
contracts/input.schema.json; the completion cites causal facts only (never the
realized forward return).

Caps (SAMPLING_CONTRACT): max MAX_PER_MINT episodes per mint, selected
deterministically by sha256(episode_id) rank (NOT by outcome).

Outputs:
  candidate/train.jsonl, candidate/validation.jsonl, candidate/test.jsonl
  reports/TOKEN_CENSUS.json
"""
import argparse, json, hashlib, sys, os, collections

MAX_PER_MINT = 3
INPUT_KEYS = ["mint_tag", "t_dec_ms", "interval_s", "n_prior_trades", "price_sol_per_raw",
              "age_s", "last_trade_age_s", "buy_count", "sell_count", "unique_traders",
              "sol_volume_lamports", "buy_volume_lamports", "sell_volume_lamports",
              "net_flow_lamports", "ret_5s_bp", "ret_30s_bp", "price_volatility_30s_bp",
              "top1_trader_share", "top5_trader_share", "buyer_seller_ratio", "venue",
              "curve_venue_present", "evidence_status"]

SYSTEM = ("You are an on-chain opportunity assessor for pump.fun / pumpswap memecoins. "
          "You receive a strictly causal state snapshot measured at a decision time and "
          "must choose exactly one action: BUY, WATCH or SKIP. "
          "Round-trip cost (fees + slippage + priority + failure charge) is 210 bp. "
          "A BUY is only correct if the expected move can clear that cost. "
          "The highest past return is NOT the best trade. "
          "Answer in the fixed format: DECISION, then SIZE/PRICE LIMIT (BUY only), "
          "INVALIDATION, EVIDENCE (2-4 facts citing the supplied fields), "
          "COUNTEREVIDENCE, EVIDENCE_STATUS.")


def build_input(st, mint_tag):
    d = {"mint_tag": mint_tag, "t_dec_ms": st["t_dec_ms"], "interval_s": st["interval_s"],
         "n_prior_trades": st["n_prior_trades"], "price_sol_per_raw": st["price"],
         "age_s": round(st["age_s"], 1), "last_trade_age_s": round(st["last_age"], 2),
         "buy_count": st["nbuy"], "sell_count": st["nsell"], "unique_traders": st["uniq"],
         "sol_volume_lamports": st["solvol"], "buy_volume_lamports": st["buyvol"],
         "sell_volume_lamports": st["sellvol"], "net_flow_lamports": st["netflow"],
         "ret_5s_bp": rd(st["r5"]), "ret_30s_bp": rd(st["r30"]),
         "price_volatility_30s_bp": rd(st["vol"]),
         "top1_trader_share": rd(st["top1"]), "top5_trader_share": rd(st["top5"]),
         "buyer_seller_ratio": rd(st["bs"]), "venue": st["venue"],
         "curve_venue_present": st["cutv"], "evidence_status": st["estat"]}
    assert set(d.keys()) == set(INPUT_KEYS), set(d) ^ set(INPUT_KEYS)
    return d


def rd(x, n=5):
    return None if x is None else round(float(x), n)


def render_state(state):
    def f(v, s=""):
        return "n/a" if v is None else (f"{v}{s}")
    L = [
        f"t_dec_ms={state['t_dec_ms']}  age_s={state['age_s']}  last_trade_age_s={state['last_trade_age_s']}",
        f"venue={state['venue']}  curve_present={state['curve_venue_present']}  evidence_status={state['evidence_status']}",
        f"n_prior_trades={state['n_prior_trades']}  buy_count={state['buy_count']}  sell_count={state['sell_count']}  unique_traders={state['unique_traders']}",
        f"price_sol_per_raw={state['price_sol_per_raw']}  ret_5s_bp={f(state['ret_5s_bp'])}  ret_30s_bp={f(state['ret_30s_bp'])}  vol_30s_bp={f(state['price_volatility_30s_bp'])}",
        f"buy_volume_lamports={state['buy_volume_lamports']}  sell_volume_lamports={state['sell_volume_lamports']}  net_flow_lamports={state['net_flow_lamports']}",
        f"top1_trader_share={f(state['top1_trader_share'])}  top5_trader_share={f(state['top5_trader_share'])}  buyer_seller_ratio={f(state['buyer_seller_ratio'])}",
    ]
    return "\n".join(L)


def build_completion(action, state, operands):
    net = state["net_flow_lamports"]
    flow_sign = "inflow" if (net or 0) > 0 else "outflow"
    conc = state["top1_trader_share"]
    age = state["last_trade_age_s"]
    if action == "BUY":
        out = [
            "DECISION: BUY",
            "SIZE: 0.5 SOL",
            f"PRICE LIMIT: {state['price_sol_per_raw']}",
            f"INVALIDATION: exit if net_flow_lamports turns negative or last_trade_age_s > {max(3.0, (age or 0) * 3):.1f}",
            f"EVIDENCE: cost floor is 210 bp so the position needs >210 bp to break even; "
            f"sustained {flow_sign} of {net} lamports with {state['buy_count']} buys vs {state['sell_count']} sells; "
            f"top1 trader share {conc} means flow is {'concentrated (fragile)' if (conc or 0) > 0.5 else 'distributed'}",
            "COUNTEREVIDENCE: memecoin reverses are common; a single large seller can exhaust the book",
            "EVIDENCE_STATUS: " + state["evidence_status"],
        ]
    elif action == "WATCH":
        out = [
            "DECISION: WATCH",
            f"INVALIDATION: re-evaluate when net_flow_lamports exceeds 0 and buy_count exceeds sell_count, or if last_trade_age_s exceeds 5 s",
            f"EVIDENCE: net_flow_lamports={net} is not decisively positive against the 210 bp cost floor; "
            f"{state['buy_count']} buys vs {state['sell_count']} sells; last trade {age}s ago",
            "COUNTEREVIDENCE: quiet books can move fast; waiting risks missing the move",
            "EVIDENCE_STATUS: " + state["evidence_status"],
        ]
    else:  # SKIP
        out = [
            "DECISION: SKIP",
            f"INVALIDATION: none for this snapshot; only a new snapshot with positive net flow would change it",
            f"EVIDENCE: net_flow_lamports={net} with {state['sell_count']} sells vs {state['buy_count']} buys; "
            f"top1 trader share {conc} signals {'concentration risk' if (conc or 0) > 0.5 else 'thin participation'}; "
            f"cost floor 210 bp is not reachable from this state",
            "COUNTEREVIDENCE: none material — the state is not attractive",
            "EVIDENCE_STATUS: " + state["evidence_status"],
        ]
    return "\n".join(out)


def sha(s):
    return hashlib.sha256(s.encode()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--states", required=True)
    ap.add_argument("--labels", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-per-mint", type=int, default=MAX_PER_MINT)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    os.makedirs("reports", exist_ok=True)

    labels = {}
    with open(a.labels) as f:
        for line in f:
            if line.strip():
                r = json.loads(line)
                labels[r["episode_id"]] = r

    # gather episodes, group by mint, cap deterministically
    by_mint = collections.defaultdict(list)
    n_read = 0
    with open(a.states) as f:
        for line in f:
            if not line.strip():
                continue
            r = json.loads(line)
            n_read += 1
            lb = labels.get(r["identity"]["episode_id"])
            if not lb or lb["action"] is None:
                continue          # censored / unlabeled
            by_mint[r["identity"]["mint"]].append((r, lb))
    if n_read == 0:
        raise SystemExit("FATAL: zero states read")
    if not by_mint:
        raise SystemExit("FATAL: zero labeled episodes after join")

    files = {"train": open(f"{a.out}/train.jsonl", "w", buffering=1 << 20),
             "validation": open(f"{a.out}/validation.jsonl", "w", buffering=1 << 20),
             "test": open(f"{a.out}/test.jsonl", "w", buffering=1 << 20)}
    counts = {"train": 0, "validation": 0, "test": 0}
    action_counts = {"train": collections.Counter(), "validation": collections.Counter(),
                     "test": collections.Counter()}
    chars = collections.Counter()
    mint_rec_counts = []

    for mint, eps in by_mint.items():
        eps.sort(key=lambda x: sha(x[0]["identity"]["episode_id"]))
        chosen = eps[:a.max_per_mint]
        mint_rec_counts.append(len(chosen))
        for (r, lb) in chosen:
            st = {
                "t_dec_ms": r["decision_clock"]["t_dec_ms"],
                "interval_s": r["decision_clock"]["interval_s"],
                "n_prior_trades": r["decision_clock"]["n_prior_trades"],
                "price": r["state"]["price_sol_per_raw"],
                "age_s": r["state"]["age_s"], "last_age": r["state"]["last_trade_age_s"],
                "nbuy": r["state"]["buy_count"], "nsell": r["state"]["sell_count"],
                "uniq": r["state"]["unique_traders"], "solvol": r["state"]["sol_volume_lamports"],
                "buyvol": r["state"]["buy_volume_lamports"], "sellvol": r["state"]["sell_volume_lamports"],
                "netflow": r["state"]["net_flow_lamports"],
                "r5": r["state"]["ret_5s_bp"], "r30": r["state"]["ret_30s_bp"],
                "vol": r["state"]["price_volatility_30s_bp"],
                "top1": r["state"]["top1_trader_share"], "top5": r["state"]["top5_trader_share"],
                "bs": r["state"]["buyer_seller_ratio"], "venue": r["state"]["venue"],
                "cutv": r["state"]["curve_venue_present"], "estat": r["state"]["evidence_status"],
            }
            tag = "mint_" + sha(mint)[:10]
            inp = build_input(st, tag)
            user = ("DECISION CLOCK — assess this opportunity.\n" + render_state(inp) +
                    "\n\nChoose exactly one action: BUY, WATCH, SKIP. "
                    "Respect the 210 bp round-trip cost. Answer in the fixed format.")
            compl = build_completion(lb["action"], inp, lb["rationale_evidence"]["operands"])
            rec = {
                "episode_id": lb["episode_id"], "mint": mint, "split": lb["split"],
                "messages": [{"role": "system", "content": SYSTEM},
                             {"role": "user", "content": user},
                             {"role": "assistant", "content": compl}],
                "meta": {"action": lb["action"], "truth_type": lb["truth_type"],
                         "label_status": lb["label_status"], "builder": "serialize_v2.0"},
            }
            sp = "validation" if lb["split"] == "val" else ("test" if lb["split"] == "test" else "train")
            files[sp].write(json.dumps(rec, separators=(",", ":")) + "\n")
            counts[sp] += 1
            action_counts[sp][lb["action"]] += 1
            chars[sp] += len(user) + len(compl) + len(SYSTEM)

    for f in files.values():
        f.close()
    if sum(counts.values()) == 0:
        raise SystemExit("FATAL: zero records written")

    census = {
        "schema": "north_star_token_census_v2",
        "states_read": n_read,
        "records_written": counts,
        "mints_used": len(by_mint),
        "max_per_mint": a.max_per_mint,
        "episodes_per_mint_mean": round(sum(mint_rec_counts) / max(len(mint_rec_counts), 1), 2),
        "action_counts": {k: dict(v) for k, v in action_counts.items()},
        "approx_chars": dict(chars),
        "approx_tokens_4char": {k: round(v / 4.0) for k, v in chars.items()},
        "note": "approx_tokens_4char is a rough proxy; real tokenizer census is run by loss_budget.py",
    }
    json.dump(census, open("reports/TOKEN_CENSUS.json", "w"), indent=1)
    print(f"[SERIALIZE] states={n_read:,} mints={len(by_mint):,} records={counts}")
    print(f"[SERIALIZE] actions train={dict(action_counts['train'])}")
    print(f"[SERIALIZE] actions val={dict(action_counts['validation'])}")
    print(f"[SERIALIZE] approx tokens (chars/4) {census['approx_tokens_4char']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
