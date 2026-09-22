#!/usr/bin/env python3
"""Emit the enrichment parity fixture: random tapes, expected values computed with the
corpus producer's OWN expressions.

Source: /training/v2/code/src/v2/rl/build_c9_enrichment_full.py lines 130-166 (the snapshot
loop that wrote every c-series row's `enriched` field). The expressions are transcribed here
rather than imported because they are inline in that script, not a function — which is the
same reason gen_state_ledger_fixture.py transcribes `_ret_bp`.

Deterministic: seeded RNG, no clock, no environment.

CAVEAT the fixture exists to make checkable: `holder_hhi` is a float sum, and Python sums it in
dict-insertion order while the Rust port sums in address order. Float addition is not
associative, so the fixture pins integers EXACTLY and floats to 1e-9. Byte-identical PROMPT
rendering additionally needs the corpus's `round(x, 6)` (ties-to-even) — that remains an owed
decision, tracked separately.
"""
import json
import random
import sys

LAMPORTS = 1e9
OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/enrichment_parity.json"


def wallet(rng):
    return bytes(rng.randrange(256) for _ in range(32)).hex()


def corpus_snapshot(trades, t_dec):
    """Transcribed from build_c9_enrichment_full.py:130-166. Returns (record, reject)."""
    cutoff = [e for e in trades if e["recv_unix_ms"] <= t_dec]
    if len(cutoff) < 2:
        return None, "insufficient_history"
    bal, slots, rt = {}, {}, {}
    buys = sells = vol = 0
    for e in cutoff:
        bal[e["trader"]] = bal.get(e["trader"], 0) + e["tokens_raw"]
        vol += abs(e["sol_lamports"])
        if e["tokens_raw"] > 0:
            buys += 1
            rt[e["trader"]] = rt.get(e["trader"], 0) + 1
        else:
            sells += 1
            rt[e["trader"]] = rt.get(e["trader"], 0) - 1
        if e["slot"] is not None:
            slots.setdefault(e["slot"], set()).add(e["trader"])
    hs = [v for v in bal.values() if v > 0]
    tot = sum(hs) or 1
    desc = sorted(hs, reverse=True)
    top1 = desc[0] / tot if desc else 0.0
    top5 = sum(desc[:5]) / tot if desc else 0.0
    hhi = sum((x / tot) ** 2 for x in hs)
    bundle_slots = sum(1 for s, trs in slots.items() if len(trs) >= 2)
    bundle_wallets = sum(len(trs) for trs in slots.values() if len(trs) >= 2)
    if not (0.0 <= top1 <= 1.0 and 0.0 <= top5 <= 1.0 and 0.0 <= hhi <= 1.0):
        return None, "inconsistent_shares"
    return {
        "holders_at_t": len(hs),
        "top1_float_share": round(top1, 6),
        "top5_float_share": round(top5, 6),
        "holder_hhi": round(hhi, 6),
        "bundle_slots": bundle_slots,
        "bundle_wallets": bundle_wallets,
        "n_buys_at_t": buys,
        "n_sells_at_t": sells,
        "volume_sol_at_t": round(vol / LAMPORTS, 6),
        "round_trip_wallets": sum(1 for n in rt.values() if n == 0),
        "wash_ratio": round(sum(1 for n in rt.values() if n == 0) / max(1, len(bal)), 6),
    }, None


def main():
    rng = random.Random(20260920)
    cases = []
    for i in range(120):
        n = rng.randrange(0, 14)
        wallets = [wallet(rng) for _ in range(rng.randrange(1, 6))]
        trades = []
        t = 1_700_000_000_000
        for _ in range(n):
            t += rng.randrange(0, 400)
            who = rng.choice(wallets)
            # Bias toward buys with a real float, plus deliberate round trips and
            # multi-wallet slots (the bundle case) rather than uniform noise.
            if rng.random() < 0.25:
                tokens = -rng.randrange(1, 500)
            else:
                tokens = rng.randrange(1, 500)
            slot = rng.choice([None, 10, 11, 12])
            trades.append({
                "recv_unix_ms": t,
                "trader": who,
                "tokens_raw": tokens,
                "sol_lamports": rng.randrange(1_000, 900_000_000),
                "slot": slot,
            })
        # Half the cases cut the window mid-tape (causality), half at the last print.
        t_dec = t if i % 2 else (trades[len(trades) // 2]["recv_unix_ms"] if trades else t)
        expected, gap = corpus_snapshot(trades, t_dec)
        cases.append({
            "trades": trades,
            "t_dec_ms": t_dec,
            "expected": expected,
            "gap": gap,
        })
    with open(OUT, "w", encoding="utf-8") as fh:
        json.dump({"cases": cases}, fh, indent=1, sort_keys=True)
    ok = sum(1 for c in cases if c["expected"])
    print(json.dumps({"out": OUT, "cases": len(cases), "with_snapshot": ok,
                      "refused": len(cases) - ok}, indent=1))


if __name__ == "__main__":
    main()