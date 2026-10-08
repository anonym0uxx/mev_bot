# Milestone 1: raw capture -> production decoder -> engine decision join/flow reducer -> features

Run (read-only on captures; writes only under /tmp):
    python3 to_wire.py SESSION /tmp/mh_recon2/m1/SESSION          # raw parts 0000-0019 -> daemon wire lines (+meta.tx_ok)
    cargo build -p pump-quant-junction --example curve_event_milestone
    target/debug/examples/curve_event_milestone /tmp/mh_recon2/m1/SESSION > SESSION.out
    python3 compare.py SESSION                                    # independent reference + before/after

`to_wire.py` is a harness SHIM (it reproduces tools/stream-capture-rs main.rs::daemon_tx_line from the raw
recorder records). Everything after it -- parse_ndjson_line, decode_curve_trade_events, dedup, DecisionCache,
FlowReducer, derive_market_trade_from_delta, TradeJoin -- is production code.

Scope: raw parts 0000-0019 of each session (not whole session). Grid clocks every 60 s per mint with >=5 events;
clock t is served BEFORE applying any line with recv >= t (causal by receipt).
Flagged window = >=1 snapshot-derived `possible_trade`/invalid drop for that mint in [t-300s, t). Unflagged = none.

Independent reference R1 = Python re-decode of TradeEvents from the same wire lines + c11 serve() semantics
(build_c11_flow_enrichment.serve). It SHARES the "TradeEvent is the trade" premise and the vs==0 refusal rule with the
Rust path -- so exactness vs R1 proves implementation correctness, NOT that TradeEvents are the corpus's definition.
Definition gap vs the corpus tape: see diag_s1.txt (quantities differ: TradeEvent sol_amount vs trader balance delta;
85 of 16,773 common rows resolve a different trader; 904 tape-only rows; 3,416 event-only rows (events include
routed/CPI trades the tape's resolver rejects, and the tape's slice boundaries)). Corpus-tape agreement on entrants is
therefore LOW (see compare.json `==corpus_tape`) and is NOT a pass.
HASHES.json pins reference/wire/output hashes.
