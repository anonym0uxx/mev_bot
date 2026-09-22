#!/usr/bin/env python
"""LIVE FLOW STATE parity fixture - real C11 enrichment entries vs the Rust flow block.

WHAT IS BEING GRADED. `flow_feed::flow_state_from_aggregates` maps the reducer's integer carries
onto the thirteen corpus fields, and `render_live_flow_state` prints them. Both are graded here
against the authority's OWN renderer (`inject_c11_flow.render`) over real enrichment entries, so
a wrong field order, a wrong unit conversion, or a `None` that renders as `0` all fail.

The fixture carries the enrichment's printed values, which are the corpus's `round(x, 6)` doubles;
the Rust side rebuilds the reducer's integer carries from them (millionths, tenths) and renders.
That is exactly the round-trip the live path performs in the other direction.

Output: parity_rust/flow_state_parity.json
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.argv = [sys.argv[0]]  # inject_c11_flow parses argv at import time
sys.path.insert(0, os.path.dirname(HERE))
import inject_c11_flow as C11  # noqa: E402  (the authority's FIELDS + render)

ENRICH = "/training/v2/reports/C11_FLOW_ENRICHMENT.jsonl"
INT_FIELDS = ("entrants_60s", "entrants_300s", "smart_entrants_300s", "coentry_wallets_300s")
FLOAT_FIELDS = ("net_flow_sol_300s", "fresh_wallet_share_300s", "flow_lookback_d",
                "sniper_share_300s", "bot_uniform_share_300s", "smart_net_flow_sol_300s")


def main():
    cases = []
    n = 0
    with open(ENRICH, encoding="utf-8") as fh:
        for line in fh:
            n += 1
            e = json.loads(line)
            if e.get("no_prior_flow"):
                cases.append({"no_prior_flow": True, "expected_line": C11.render(e)})
                continue
            case = {
                "no_prior_flow": False,
                "ints": {k: int(e[k]) for k in INT_FIELDS},
                "floats": {k: (None if e.get(k) is None else float(e[k])) for k in FLOAT_FIELDS},
                "creator_trading_own_mint": bool(e.get("creator_trading_own_mint")),
                "entrant_fee_p90_lamports": e.get("entrant_fee_p90_lamports"),
                "entrant_cu_p50": e.get("entrant_cu_p50"),
                "expected_line": C11.render(e),
            }
            cases.append(case)
            if len(cases) >= 60:
                break

    # A couple of deliberate edges the enrichment file may not contain, rendered by the authority.
    # Flat, in the enrichment's own shape - `render` reads flat keys, so handing it a nested
    # case structure would render thirteen `None`s (which is exactly what a first version did).
    edge_flat = {"entrants_60s": 0, "entrants_300s": 0, "net_flow_sol_300s": 0.0,
                 "fresh_wallet_share_300s": None, "flow_lookback_d": 7.0,
                 "sniper_share_300s": None, "bot_uniform_share_300s": None,
                 "smart_entrants_300s": 0, "smart_net_flow_sol_300s": 0.0,
                 "coentry_wallets_300s": 0, "creator_trading_own_mint": False,
                 "entrant_fee_p90_lamports": None, "entrant_cu_p50": None}
    cases.append({"no_prior_flow": False,
                  "ints": {k: edge_flat[k] for k in INT_FIELDS},
                  "floats": {k: edge_flat[k] for k in FLOAT_FIELDS},
                  "creator_trading_own_mint": edge_flat["creator_trading_own_mint"],
                  "entrant_fee_p90_lamports": edge_flat["entrant_fee_p90_lamports"],
                  "entrant_cu_p50": edge_flat["entrant_cu_p50"],
                  "expected_line": C11.render(edge_flat)})
    cases.append({"no_prior_flow": True, "expected_line": C11.render({"no_prior_flow": True})})

    out = {"schema": "flow-state-parity/1",
           "producer": "C11_FLOW_ENRICHMENT.jsonl via inject_c11_flow.render",
           "fields": list(C11.FIELDS),
           "n_entries_seen": n,
           "n_cases": len(cases),
           "cases": cases}
    path = os.path.join(HERE, "flow_state_parity.json")
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(out, fh, indent=1, sort_keys=True)
    print("wrote %s: %d cases from %d enrichment entries" % (path, len(cases), n))
    for c in cases[:3]:
        print("  " + c["expected_line"][:110])


if __name__ == "__main__":
    main()
