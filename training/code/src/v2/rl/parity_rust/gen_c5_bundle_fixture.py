#!/usr/bin/env python3
"""Emit the C5 parity fixture: REAL c12 rows + the REAL tape they were derived from.

Answers the question an assembler must answer before it can be trusted: does the Rust derivation,
run over the same tape the corpus used, render the same lines the corpus stored?

Sources (both real, neither synthesised):
  rows : /training/v2/candidate_sft_c12_entry/train.jsonl   (the corpus's own rendering)
  tape : /training/v2/canonical/renormalized_v7/trades.jsonl (the tape build_c9_enrichment_full.py read)

For each selected row we record the mint, the decision clock parsed from the row's own prompt, the
tape prefix strictly at-or-before that clock (the corpus's cutoff is `<= t_dec`), and the exact
prompt LINES the Rust side must reproduce. Every block with a live producer is recorded as an
expectation: the state line(s), the ENRICHED line, the DEV HISTORY line, and — for the
reserve/flow PLANE lines (CURVE STATE, AMM POOL STATE, PRICE UNITS, LIVE FLOW STATE) — the
AUTHORITY'S OWN INPUTS plus its rendering of them, so the Rust producer under test can be driven
and compared rather than the corpus's text copied.

Usage: gen_c5_bundle_fixture.py <out.json> [n_rows] [tape_path] [rows_path]
"""
import collections
import json
import os
import sys

OUT = sys.argv[1] if len(sys.argv) > 1 else "/home/alon/build/mev_bot_gh/rust/crates/pump-quant-app/tests/fixtures/c5_bundle_parity.json"
N_ROWS = int(sys.argv[2]) if len(sys.argv) > 2 else 20
TAPE = sys.argv[3] if len(sys.argv) > 3 else "/training/v2/canonical/renormalized_v7/trades.jsonl"
ROWS = sys.argv[4] if len(sys.argv) > 4 else "/training/v2/candidate_sft_c12_entry/train.jsonl"
LAUNCHES = "/training/v2/canonical/renormalized_v7/launches.jsonl"
FLOW_ENRICH = "/training/v2/reports/C11_FLOW_ENRICHMENT.jsonl"

# The reserve/flow PLANE lines this harness now grades end-to-end. Their expected side is the
# corpus AUTHORITY's own renderer applied to the same inputs the Rust producer is given:
# `build_sft_c6.curve_state/amm_state/price_units_line` and `inject_c11_flow.render`. The
# generator records those inputs (the reserve observation, the pool attribution, the flow
# aggregates, the price) so the Rust test can drive the producer under test and render it —
# never a copy of the corpus's own text.
PLANE_PREFIXES = ("CURVE STATE", "AMM POOL STATE", "PRICE UNITS", "LIVE FLOW STATE")

# `inject_c11_flow` parses argv at IMPORT time; neutralise it before importing the authority.
sys.argv = [sys.argv[0]]
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import re  # noqa: E402
import build_sft_c6 as C  # noqa: E402  (curve_state / amm_state / *_line — the authority)
import inject_c11_flow as C11  # noqa: E402  (FIELDS + render — the LIVE FLOW STATE authority)

# The reducer's integer carries are rebuilt from these corpus `round(x, 6)` doubles, exactly as
# `gen_flow_state_fixture.py` does.
FLOW_INT_FIELDS = ("entrants_60s", "entrants_300s", "smart_entrants_300s", "coentry_wallets_300s")
FLOW_FLOAT_FIELDS = ("net_flow_sol_300s", "fresh_wallet_share_300s", "flow_lookback_d",
                     "sniper_share_300s", "bot_uniform_share_300s", "smart_net_flow_sol_300s")

# The prompt lines whose producers exist today: the as-of-t state block and the enrichment block.
STATE_PREFIXES = (
    "t_dec_ms=",
    "venue=",
    "n_prior_trades=",
    "price_lamports_per_raw_token=",
    "buy_volume_lamports=",
    "top1_trader_share=",
)
ENRICHED_PREFIX = "ENRICHED CANDIDATE STATE"


def prompt_of(row):
    msgs = row.get("messages") or []
    for m in msgs:
        if m.get("role") == "user":
            return str(m.get("content") or "")
    return ""


def lines_of(rows_text):
    return [ln for ln in rows_text.split("\n") if ln.strip()]


def field_of(line, key):
    """The `key=value` token of a rendered line, or None."""
    m = re.search(re.escape(key) + r"=(\S+)", line)
    return m.group(1) if m else None


def _curve_block(case):
    """CURVE STATE: inputs = the reserve observation the row's own line was rendered from."""
    line = case["_plane_lines"].get("CURVE STATE")
    block = {"producer": "build_sft_c6.curve_state + curve_line",
             "graded": False, "skip_reason": None, "input": None,
             "expected_line": None, "corpus_line": line}
    if not line:
        block["skip_reason"] = "the_row_carries_no_CURVE_STATE_line"
        return block
    if "=absent" in line:
        # The row's absent line carries only the reason; the reserve snapshot (and therefore the
        # staleness the authority's refusal is computed from) is NOT recoverable from it, so it is
        # not graded rather than reconstructed from nothing.
        block["skip_reason"] = "corpus_line_is_absent_%s_and_the_snapshot_is_not_carried" % (
            field_of(line, "reason") or "unknown")
        return block
    ts = case["t_dec_ms"] - int(field_of(line, "staleness_ms"))
    obs = {
        "ts_ms": ts,
        "v_sol_lamports": int(round(float(field_of(line, "v_sol_reserves_sol")) * 1e9)),
        "v_tokens": int(field_of(line, "v_tokens_reserves")),
        "real_sol_lamports": int(round(float(field_of(line, "real_sol_reserves_sol")) * 1e9)),
        "real_tokens": int(field_of(line, "real_tokens_reserves")),
        "slot": 0,  # only used to reject out-of-order replays; a single observation has none
    }
    tape = {case["mint"]: ([ts], [(ts, {
        "virtual_sol_reserves_lamports": obs["v_sol_lamports"],
        "virtual_token_reserves_raw": obs["v_tokens"],
        "real_sol_reserves_lamports": obs["real_sol_lamports"],
        "real_token_reserves_raw": obs["real_tokens"]})])}
    cb, why = C.curve_state(case["mint"], case["t_dec_ms"], tape)
    expected = C.curve_line(cb, why)
    block["input"] = obs
    if expected != line:
        block["skip_reason"] = "the_authority_over_these_inputs_does_not_reproduce_the_row"
        return block
    block.update({"graded": True, "expected_line": expected})
    return block


def _amm_block(case):
    """AMM POOL STATE: inputs = the pool observation + the mint's attribution facts."""
    line = case["_plane_lines"].get("AMM POOL STATE")
    block = {"producer": "build_sft_c6.amm_state + amm_line",
             "graded": False, "skip_reason": None, "input": None,
             "expected_line": None, "corpus_line": line}
    if not line:
        block["skip_reason"] = "the_row_carries_no_AMM_POOL_STATE_line"
        return block
    if "=absent" in line:
        reason = field_of(line, "reason") or "unknown"
        if reason != "never_graduated":
            block["skip_reason"] = "corpus_line_is_absent_%s_and_its_inputs_are_not_carried" % reason
            return block
        # `never_graduated` IS reconstructible: the input is the attribution fact that no pool
        # exists, and the authority (like the Rust producer) must render the refusal from it.
        attribution = {"wsol_pools": [], "pools_total": 0, "graduated": False}
        cb, why = C.amm_state(case["mint"], case["t_dec_ms"], {}, {}, {case["mint"]}, {})
        expected = C.amm_line(cb, why)
        block["input"] = {"attribution": attribution, "obs": None}
        if expected != line:
            block["skip_reason"] = "the_authority_over_these_inputs_does_not_reproduce_the_row"
            return block
        block.update({"graded": True, "expected_line": expected})
        return block
    pool = field_of(line, "pool")
    ts = case["t_dec_ms"] - int(field_of(line, "staleness_ms"))
    base = int(field_of(line, "base_reserves_raw"))
    quote = int(field_of(line, "quote_reserves_lamports"))
    slot = int(field_of(line, "reserve_slot"))
    obs = {"pool": pool, "base_reserves_raw": base, "quote_reserves_lamports": quote,
           "quote_is_wsol": True, "ts_ms": ts, "slot": slot}
    # The row's own line prints one authoritative WSOL pool and no other, so the corpus's own
    # attribution inputs are: exactly one pool in total, that pool authoritative WSOL.
    attribution = {"wsol_pools": [pool], "pools_total": 1, "graduated": True}
    tape = {case["mint"]: ([ts], [(ts, pool, base, quote, slot, "trade")])}
    cb, why = C.amm_state(case["mint"], case["t_dec_ms"], tape,
                          {case["mint"]: {pool}}, set(), {case["mint"]: 1})
    expected = C.amm_line(cb, why)
    block["input"] = {"attribution": attribution, "obs": obs}
    if expected != line:
        block["skip_reason"] = "the_authority_over_these_inputs_does_not_reproduce_the_row"
        return block
    block.update({"graded": True, "expected_line": expected})
    return block


def _price_block(case):
    """PRICE UNITS: input = the one measurement the line names, in lamports per raw token."""
    line = case["_plane_lines"].get("PRICE UNITS")
    state = next((l for l in case["expected_state_lines"]
                  if l.startswith("price_lamports_per_raw_token=")), None)
    block = {"producer": "build_sft_c6.price_units_line",
             "graded": False, "skip_reason": None,
             "input_price_lamports_per_raw_token": None,
             "expected_line": None, "corpus_line": line}
    if not line or state is None:
        block["skip_reason"] = "the_row_carries_no_PRICE_UNITS_line_or_no_state_price"
        return block
    px = float(field_of(state, "price_lamports_per_raw_token"))
    expected = C.price_units_line(px)
    block["input_price_lamports_per_raw_token"] = px
    if expected != line:
        block["skip_reason"] = "the_authority_over_these_inputs_does_not_reproduce_the_row"
        return block
    block.update({"graded": True, "expected_line": expected})
    return block


def _flow_block(case, flow_idx):
    """LIVE FLOW STATE: inputs = the C11 enrichment entry's own values, keyed by clock."""
    line = case["_plane_lines"].get("LIVE FLOW STATE")
    block = {"producer": "inject_c11_flow.render",
             "graded": False, "skip_reason": None, "corpus_line": line,
             "expected_line": None, "inputs": None}
    e = flow_idx.get((case["mint"], case["t_dec_ms"]))
    if e is None:
        block["skip_reason"] = "no_C11_FLOW_ENRICHMENT_entry_for_this_mint_and_clock"
        return block
    if not line:
        block["skip_reason"] = "the_row_carries_no_LIVE_FLOW_STATE_line"
        return block
    if e.get("no_prior_flow"):
        inputs = {"no_prior_flow": True}
    else:
        inputs = {
            "no_prior_flow": False,
            "ints": {k: int(e[k]) for k in FLOW_INT_FIELDS},
            "floats": {k: (None if e.get(k) is None else float(e[k])) for k in FLOW_FLOAT_FIELDS},
            "creator_trading_own_mint": bool(e.get("creator_trading_own_mint")),
            "entrant_fee_p90_lamports": e.get("entrant_fee_p90_lamports"),
            "entrant_cu_p50": e.get("entrant_cu_p50"),
        }
    expected = C11.render(e)
    block["inputs"] = inputs
    if expected != line:
        block["skip_reason"] = "the_authority_over_these_inputs_does_not_reproduce_the_row"
        return block
    block.update({"graded": True, "expected_line": expected})
    return block


def attach_plane_blocks(cases, flow_idx):
    """Record the four plane lines' INPUTS and the AUTHORITY's rendering of them, per case."""
    for c in cases:
        c["curve_line"] = _curve_block(c)
        c["amm_line"] = _amm_block(c)
        c["price_units"] = _price_block(c)
        c["flow_state"] = _flow_block(c, flow_idx)
        c.pop("_plane_lines", None)


def load_flow_index(cases):
    """Stream the C11 enrichment, keeping only the entries this fixture's clocks need."""
    keys = {(c["mint"], c["t_dec_ms"]) for c in cases}
    idx = {}
    with open(FLOW_ENRICH, encoding="utf-8") as fh:
        for line in fh:
            try:
                r = json.loads(line)
            except Exception:
                continue
            k = (r.get("mint"), int(r.get("t_dec_ms")))
            if k in keys:
                idx[k] = r
    return idx


def main():
    # ---- 1. select rows, and harvest the clock + the lines to reproduce
    cases, wanted = [], {}
    with open(ROWS, encoding="utf-8") as fh:
        for i, line in enumerate(fh):
            if len(cases) >= N_ROWS:
                break
            # Spread the sample rather than taking the first N: the corpus is ordered by build
            # stage, and a prefix would over-represent one stage's mints.
            if i % 137:
                continue
            row = json.loads(line)
            prompt = prompt_of(row)
            ls = lines_of(prompt)
            t_dec = None
            for ln in ls:
                if ln.startswith("t_dec_ms="):
                    t_dec = int(ln.split()[0].split("=")[1])
                    break
            if t_dec is None:
                continue
            state = [ln for ln in ls if ln.startswith(STATE_PREFIXES)]
            enriched = [ln for ln in ls if ln.startswith(ENRICHED_PREFIX)]
            if len(state) < len(STATE_PREFIXES) or not enriched:
                continue
            # every recorded line must be a block we can derive today
            mint = row["mint"]
            wanted[mint] = len(cases)
            # The four plane lines, as the selected row stores them. The decision-family pass below
            # may override these with the same clock's decision row (only the DEV line differs by
            # family, but taking the family the live path serves keeps every grading side honest).
            plane_lines = {p: next((x for x in ls if x.startswith(p)), None) for p in PLANE_PREFIXES}
            cases.append(
                {
                    "mint": mint,
                    "t_dec_ms": t_dec,
                    "trades": [],
                    "expected_state_lines": state,
                    "expected_enriched_line": enriched[0],
                    "_plane_lines": plane_lines,
                }
            )
    print(json.dumps({"rows": len(cases)}), flush=True)
    if not cases:
        raise SystemExit("no usable rows")

    # ---- 1b. the DEV HISTORY expectation, from the DECISION family's row for the same clock.
    #
    # The corpus renders this line in two dialects — `creator_known=1` (decision rows,
    # `build_c9.py`) and `creator_known=True … bundle_wallets=… (utility / management rows,
    # `inject_enrichment.py`). The live entry path serves the decision family, so the expectation is
    # taken from that family's row even when a sibling family shares the (mint, t_dec) clock.
    wanted_dev = {(c["mint"], c["t_dec_ms"]): i for i, c in enumerate(cases)}
    dev_lines: dict[int, str] = {}
    plane_by_idx: dict[int, dict] = {}
    with open(ROWS, encoding="utf-8") as fh:
        for line in fh:
            hit = next((m for m in wanted if m in line), None)
            if hit is None:
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            if (r.get("family") or "?") != "decision":
                continue
            prompt = prompt_of(r)
            ls = lines_of(prompt)
            t_dec = None
            for ln in ls:
                if ln.startswith("t_dec_ms="):
                    t_dec = int(ln.split()[0].split("=")[1])
                    break
            idx = wanted_dev.get((r.get("mint"), t_dec))
            if idx is None:
                continue
            if idx not in plane_by_idx:
                plane_by_idx[idx] = {p: next((x for x in ls if x.startswith(p)), None)
                                     for p in PLANE_PREFIXES}
            if idx in dev_lines:
                continue
            dev = [ln for ln in ls if ln.startswith("DEV HISTORY:")]
            if dev:
                dev_lines[idx] = dev[0]
    for i, c in enumerate(cases):
        c["expected_dev_line"] = dev_lines.get(i)
        # Prefer the decision row's plane lines; fall back to whatever the selected row carried.
        c["_plane_lines"] = plane_by_idx.get(i) or c.get("_plane_lines") or {}
    missing_dev = [c["mint"] for c in cases if not c["expected_dev_line"]]
    if missing_dev:
        print(json.dumps({"warning": "cases without a decision-family DEV line",
                          "mints": missing_dev[:5]}), flush=True)

    # ---- 1c. the launch table the dev line is counted from, restricted to these mints' creators.
    creator_of, launch_by_c = {}, {}
    with open(LAUNCHES, encoding="utf-8") as fh:
        for line in fh:
            try:
                r = json.loads(line)
            except Exception:
                continue
            c, mi, t = r.get("creator"), r.get("mint"), r.get("recv_unix_ms")
            if not (c and mi and t):
                continue
            launch_by_c.setdefault(c, []).append([mi, c, int(t)])
            if mi in wanted:
                creator_of[mi] = c
    creators = sorted({creator_of[m] for m in wanted if m in creator_of})
    launches = sorted((row for c in creators for row in launch_by_c.get(c, [])),
                      key=lambda x: (x[1], x[2]))

    hist: dict[int, list] = {}
    # ---- 2. one streaming pass over the tape, keeping only the selected mints
    kept = 0
    scanned = 0
    with open(TAPE, encoding="utf-8") as fh:
        for line in fh:
            scanned += 1
            if scanned % 2_000_000 == 0:
                print(json.dumps({"scanned": scanned, "kept": kept}), flush=True)
            hit = False
            for mint in wanted:
                if mint in line:
                    hit = True
                    break
            if not hit:
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            idx = wanted.get(r.get("mint"))
            if idx is None:
                continue
            t = r.get("recv_unix_ms")
            trader = r.get("trader")
            if t is None or not trader:
                continue
            t = int(t)
            # The corpus's band needs the mint's WHOLE run, not just the prefix, so the legs are
            # recorded for every row that clears the trader gate — the cutoff is applied after.
            hist.setdefault(idx, []).append((int(r.get("sol_lamports") or 0),
                                             int(r.get("tokens_raw") or 0)))
            if t > cases[idx]["t_dec_ms"]:
                continue  # strictly the corpus's cutoff: `<= t_dec`
            cases[idx]["trades"].append(
                {
                    "recv_unix_ms": t,
                    "trader": trader,
                    "side": str(r.get("side") or ""),
                    "tokens_raw": int(r.get("tokens_raw") or 0),
                    "sol_lamports": int(r.get("sol_lamports") or 0),
                    "slot": int(r["slot"]) if r.get("slot") is not None else None,
                    "venue": str(r.get("venue") or ""),
                }
            )
            kept += 1

    # ---- 3. a case with no tape is not a parity case; drop it rather than grade nothing.
    # Addresses become dense integers: the ledger keys traders by u64, an injective map preserves
    # every derived number exactly, and a 44-character address per trade is the entire size of the
    # fixture. Mints with more than MAX_TRADES are dropped rather than truncated — a truncated
    # prefix is not the corpus's derivation, so it could not be graded honestly.
    MAX_TRADES = 20_000
    usable = []
    for c in cases:
        c["trades"].sort(key=lambda x: x["recv_unix_ms"])
        if not c["trades"] or len(c["trades"]) > MAX_TRADES:
            continue
        ids, packed = {}, []
        for tr in c["trades"]:
            addr = tr["trader"]
            if addr not in ids:
                ids[addr] = len(ids)
            packed.append(
                [
                    tr["recv_unix_ms"],
                    ids[addr],
                    1 if tr["side"] == "buy" else 0,
                    tr["tokens_raw"],
                    tr["sol_lamports"],
                    -1 if tr["slot"] is None else tr["slot"],
                    tr["venue"],
                ]
            )
        c["trades"] = packed
        c["traders"] = len(ids)
        # THE CORPUS'S ROBUST BAND, recorded as an INPUT rather than hidden.
        #
        # `build_states_v2` lines 159-165 drop every trade outside 10x of the mint's median price over
        # its WHOLE run — including trades after `t_dec`. A live ledger cannot reproduce that (it is
        # lookahead), and the C5 test's job is to prove the DERIVATION matches given the same trade
        # set, while stating exactly what the live path cannot supply. So the median is computed here,
        # from the same tape, and shipped with the case: the Rust harness can then grade both readings
        # — the causal one it ships, and the corpus's own banded one.
        px = [
            abs(sol) / abs(tok)
            for (sol, tok) in hist.get(wanted[c["mint"]], [])
            if abs(tok) >= 1_000_000 and abs(sol) >= 100_000 and tok != 0
        ]
        px = [p for p in px if p == p and p not in (float("inf"), float("-inf"))]
        if len(px) < 5:
            # `build_states_v2` applies the band only when it has >= 5 finite prices; below that the
            # corpus's trade set IS the causal one, so there is nothing to exclude.
            c["band"] = {"applies": False, "median_lamports_per_raw_token": None,
                         "prefix_trades_banded_out": 0, "history_prices": len(px)}
        else:
            med_sorted = sorted(px)
            n = len(med_sorted)
            median = (med_sorted[n // 2] if n % 2
                      else (med_sorted[n // 2 - 1] + med_sorted[n // 2]) / 2.0)
            banded_out = sum(
                1
                for tr in packed
                if abs(int(tr[3])) >= 1_000_000
                and abs(int(tr[4])) >= 100_000
                and not (median / 10.0 <= abs(int(tr[4])) / abs(int(tr[3])) <= median * 10.0)
            )
            c["band"] = {"applies": True, "median_lamports_per_raw_token": median,
                         "prefix_trades_banded_out": banded_out, "history_prices": len(px)}
        usable.append(c)
    usable = usable[:12]

    # ---- 4. the reserve/flow PLANE lines, graded through their producers (see the helpers above).
    flow_idx = load_flow_index(usable)
    attach_plane_blocks(usable, flow_idx)
    plane_counts = {}
    for key, name in (("curve_line", "CURVE STATE"), ("amm_line", "AMM POOL STATE"),
                      ("price_units", "PRICE UNITS"), ("flow_state", "LIVE FLOW STATE")):
        graded = sum(1 for c in usable if c[key]["graded"])
        skipped = collections.Counter(
            c[key]["skip_reason"] for c in usable if not c[key]["graded"])
        plane_counts[name] = {"graded": graded, "skipped": dict(skipped)}

    blob = json.dumps({"schema": "c5-bundle-parity/3", "cases": usable, "launches": launches})
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as out:
        out.write(blob)
    # A fresh run must reproduce this file byte for byte: the blob is re-derived from disk and
    # compared to what this run serialised. (`cmp` against a second run is the cross-run proof.)
    with open(OUT, encoding="utf-8") as back:
        reread = back.read()
    assert reread == blob, "the fixture did not round-trip byte-identically through disk"
    print(
        json.dumps(
            {
                "out": OUT,
                "bytes": len(blob),
                "cases": len(usable),
                "dropped_no_tape": len(cases) - len(usable),
                "trades": kept,
                "tape_lines_scanned": scanned,
                "trades_min": min((len(c["trades"]) for c in usable), default=0),
                "trades_max": max((len(c["trades"]) for c in usable), default=0),
                "traders_total": sum(c["traders"] for c in usable),
                "plane_lines": plane_counts,
            },
            indent=1,
        )
    )


if __name__ == "__main__":
    main()