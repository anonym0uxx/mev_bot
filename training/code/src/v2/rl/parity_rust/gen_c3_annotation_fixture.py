#!/usr/bin/env python
"""C3 annotation parity fixture - the CORPUS's own curve/AMM rules vs the Rust annotator.

WHAT IS BEING GRADED. `curve_annotation.rs` reimplements `build_sft_c6.curve_state` /
`amm_state`. A reimplementation that only agrees with itself is worthless, so this fixture
runs the AUTHORITATIVE producer - imported here, not copied - over a case set, and records
both the INPUTS (raw reserves, timestamps, attribution facts) and the corpus's own rendered
line. The Rust test rebuilds the line from the inputs and compares text.

Because the inputs carry raw numbers and the expectation carries the corpus's text, a wrong
Rust renderer (wrong decimals, wrong units, wrong reason string) fails. A fixture built by
re-emitting the authority's text could not do that.

CASES. Every branch of both rule sets, plus the ones that bit before: the multi-pool mint
whose last row is not an authoritative WSOL pool (the 1000x USDC unit bug), the sole-pool
mint where mint-only attribution is safe, staleness past the pricing budget but inside the
annotation cap, past the cap, a future-only observation, and nonpositive reserves.

Output: parity_rust/c3_annotation_parity.json
"""
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
C6_DIR = os.path.abspath(os.path.join(HERE, ".."))
sys.path.insert(0, C6_DIR)

import build_sft_c6 as C  # noqa: E402  the authoritative producer

MINT_A = "A" * 44
MINT_B = "B" * 44
MINT_C = "C" * 44


def curve_case(mint, t_dec, samples):
    """curve: {mint: (ts[], [(ts, {reserve keys})])} - the corpus's own structure.

    An EMPTY sample list means the mint is absent from the tape altogether, which the
    corpus expresses as a missing key, not as a present-but-empty entry.
    """
    ts = [s["ts"] for s in samples]
    lst = [(s["ts"], {
        "virtual_sol_reserves_lamports": s["vsol"],
        "virtual_token_reserves_raw": s["vtok"],
        "real_sol_reserves_lamports": s["rsol"],
        "real_token_reserves_raw": s["rtok"],
    }) for s in samples]
    tape = {mint: (ts, lst)} if samples else {}
    cb, why = C.curve_state(mint, t_dec, tape)
    # The tape-as-live-could-have-seen-it: the corpus reads a historical tape and can answer
    # `no_event_before_t_dec` for a mint whose only rows are in the future. A live engine
    # cannot hold a future observation, so its parity target is the authority's own output on
    # this FILTERED input set - the same producer, the inputs a live engine could have had.
    live = [s for s in samples if s["ts"] <= t_dec]
    lts = [s["ts"] for s in live]
    llst = [(s["ts"], {
        "virtual_sol_reserves_lamports": s["vsol"],
        "virtual_token_reserves_raw": s["vtok"],
        "real_sol_reserves_lamports": s["rsol"],
        "real_token_reserves_raw": s["rtok"],
    }) for s in live]
    lcb, lwhy = C.curve_state(mint, t_dec, {mint: (lts, llst)} if live else {})
    return {
        "kind": "curve",
        "mint": mint,
        "t_dec_ms": t_dec,
        "samples": samples,
        "expected_line": C.curve_line(cb, why),
        "expected_line_live": C.curve_line(lcb, lwhy),
        "live_differs": C.curve_line(cb, why) != C.curve_line(lcb, lwhy),
        "live_reachable": any(s["ts"] <= t_dec for s in samples),
        "expected_why": why,
        "expected_status": "present" if cb is not None else "absent",
        "expected_status_live": "present" if lcb is not None else "absent",
    }


def amm_case(mint, t_dec, rows, wsol_pools, pools_total, graduated, has_tape=True):
    """rows: [(ts, pool, base, quote, slot, event)] - the corpus's own tuple shape.

    `has_tape=False` models a graduated mint with NO backfill rows at all (the corpus's
    `no_amm_event_in_backfill`), which is a missing key in its `amm` map.
    """
    ts = [r[0] for r in rows]
    ent = {mint: (ts, rows)} if has_tape else {}
    wsol_by_mint = {mint: set(wsol_pools)} if wsol_pools else {}
    never = set() if graduated else {mint}
    npools = {mint: pools_total}
    cb, why = C.amm_state(mint, t_dec, ent, wsol_by_mint, never, npools)
    live_rows = [r for r in rows if r[0] <= t_dec]
    lts = [r[0] for r in live_rows]
    lcb, lwhy = C.amm_state(mint, t_dec, {mint: (lts, live_rows)} if live_rows else {},
                            wsol_by_mint, never, npools)
    return {
        "kind": "amm",
        "mint": mint,
        "t_dec_ms": t_dec,
        "rows": rows,
        "wsol_pools": sorted(wsol_pools),
        "pools_total": pools_total,
        "graduated": graduated,
        "has_tape": has_tape,
        "expected_line": C.amm_line(cb, why),
        "expected_line_live": C.amm_line(lcb, lwhy),
        "live_differs": C.amm_line(cb, why) != C.amm_line(lcb, lwhy),
        "live_reachable": any(r[0] <= t_dec for r in rows),
        "expected_why": why,
        "expected_status": "present" if cb is not None else "absent",
        "expected_status_live": "present" if lcb is not None else "absent",
    }


def main():
    rng = random.Random(7)
    cases = []

    # --- curve: the pump.fun initialisation, then a mid-curve state.
    init = {"ts": 1_000_000, "vsol": 30 * C.LAMPORTS_PER_SOL, "vtok": 1_073_000_000_000_000,
            "rsol": 0, "rtok": 793_100_000_000_000}
    mid = {"ts": 1_060_000, "vsol": 55 * C.LAMPORTS_PER_SOL, "vtok": 585_000_000_000_000,
           "rsol": 25 * C.LAMPORTS_PER_SOL, "rtok": 428_000_000_000_000}
    near = {"ts": 1_120_000, "vsol": 120 * C.LAMPORTS_PER_SOL, "vtok": 268_000_000_000_000,
            "rsol": 90 * C.LAMPORTS_PER_SOL, "rtok": 175_000_000_000_000}

    cases.append(curve_case(MINT_A, 1_000_500, [init]))
    cases.append(curve_case(MINT_A, 1_030_000, [init, mid]))          # 30 s stale, eligible
    cases.append(curve_case(MINT_A, 1_250_000, [init, mid, near]))     # 130 s stale, ineligible
    cases.append(curve_case(MINT_A, 1_300_000, [init, mid, near]))
    cases.append(curve_case(MINT_B, 999_000, [init]))                  # before any observation
    cases.append(curve_case(MINT_C, 1_000_500, []))                    # mint absent entirely
    cases.append(curve_case(MINT_A, 1_000_000 + C.ANNOTATION_CAP_MS + 1, [init]))  # past the cap
    cases.append(curve_case(MINT_B, 2_000_000, [{"ts": 3_000_000, "vsol": 40_000_000_000,
                                                 "vtok": 900_000_000_000_000, "rsol": 0,
                                                 "rtok": 700_000_000_000_000}]))     # future only
    cases.append(curve_case(MINT_B, 2_000_000, [{"ts": 1_000_000, "vsol": 0,
                                                 "vtok": 900_000_000_000_000, "rsol": 0,
                                                 "rtok": 700_000_000_000_000}]))     # nonpositive
    # graduated regime + a randomly-generated case so a formatting accident is not hidden
    grad = {"ts": 1_000_000, "vsol": 140 * C.LAMPORTS_PER_SOL, "vtok": 230_000_000_000_000,
            "rsol": 85 * C.LAMPORTS_PER_SOL, "rtok": 150_000_000_000_000}
    cases.append(curve_case(MINT_C, 1_000_100, [grad]))
    for i in range(3):
        s = {"ts": rng.randrange(1_000_000, 2_000_000),
             "vsol": rng.randrange(1_000, 200_000) * C.LAMPORTS_PER_SOL,
             "vtok": rng.randrange(10**12, 2 * 10**15),
             "rsol": rng.randrange(0, 90) * C.LAMPORTS_PER_SOL,
             "rtok": rng.randrange(10**12, 8 * 10**14)}
        cases.append(curve_case(MINT_A, s["ts"] + rng.randrange(1, 200_000), [init, s]))

    # --- amm: sole pool (mint-only attribution is safe) and multi-pool (needs the pool).
    cases.append(amm_case(MINT_A, 1_010_000, [(1_000_000, MINT_A, 1_000_000_000,
                                               2_500_000_000 * 10**3, 55, "tradе")],
                          [MINT_A], 1, True))
    cases.append(amm_case(MINT_B, 1_010_000, [(1_000_000, MINT_B, 500_000_000,
                                               1_200_000_000_000, 77, "trade")],
                          [MINT_B], 1, True))
    # the 1000x bug: one WSOL pool + a USDC pool, last row is the USDC pool
    cases.append(amm_case(MINT_C, 1_010_000, [(1_000_000, "USDC" * 11, 1_000_000,
                                               9_000_000, 88, "trade")],
                          ["WSOL" * 11], 2, True))
    cases.append(amm_case(MINT_C, 1_010_000, [(1_000_000, "WSOL" * 11, 1_000_000,
                                               9_000_000_000, 88, "trade")],
                          ["WSOL" * 11], 2, True))
    cases.append(amm_case(MINT_A, 1_010_000, [], [MINT_A], 1, True, has_tape=False))  # no rows at all
    cases.append(amm_case(MINT_B, 1_010_000, [(1_000_000, MINT_B, 1, 1, 1, "t")],
                          [MINT_B], 1, False))                               # never graduated
    cases.append(amm_case(MINT_A, 1_010_000, [(1_000_000, MINT_A, 0, 1_000_000, 1, "t")],
                          [MINT_A], 1, True))                                # nonpositive
    cases.append(amm_case(MINT_A, 900_000, [(1_000_000, MINT_A, 1_000_000, 1_000_000, 1, "t")],
                          [MINT_A], 1, True))                                # future only
    cases.append(amm_case(MINT_A, 1_000_000 + C.ANNOTATION_CAP_MS + 5,
                          [(1_000_000, MINT_A, 1_000_000, 1_000_000, 1, "t")],
                          [MINT_A], 1, True))                                # past the cap
    # stale but inside the cap -> reported, ineligible
    cases.append(amm_case(MINT_B, 1_300_000, [(1_000_000, MINT_B, 700_000_000,
                                               3_000_000_000_000, 91, "trade")],
                          [MINT_B], 1, True))

    out = {"schema": "c3-annotation-parity/1",
           "producer": "build_sft_c6.curve_state/amm_state/curve_line/amm_line",
           "constants": {"LAMPORTS_PER_SOL": C.LAMPORTS_PER_SOL, "TOKEN_SCALE": C.TOKEN_SCALE,
                         "V_GRAD_LAMPORTS": C.V_GRAD_LAMPORTS,
                         "ANNOTATION_CAP_MS": C.ANNOTATION_CAP_MS,
                         "PRICING_BUDGET_MS": C.PRICING_BUDGET_MS},
           "cases": cases}
    path = os.path.join(HERE, "c3_annotation_parity.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, sort_keys=True)
    abs_cases = sum(1 for c in cases if c["expected_status"] == "absent")
    print("wrote %s: %d cases (%d absent, %d present), %d bytes"
          % (path, len(cases), abs_cases, len(cases) - abs_cases, os.path.getsize(path)))
    for c in cases:
        print("  %-5s %-6s %s" % (c["kind"], c["expected_status"], c["expected_line"][:96]))


if __name__ == "__main__":
    main()
