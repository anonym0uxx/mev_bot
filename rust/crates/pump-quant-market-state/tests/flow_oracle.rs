//! ORACLE TEST: the live c11 flow reducer against the real c11 builder.
//!
//! `tests/fixtures/flow_oracle/` holds an 18-event micro-tape, five decision clocks and the
//! launch table. `expected.jsonl` in that directory is NOT hand-written: it is the output of
//! the real `/training/v2/code/src/v2/rl/build_c11_flow_enrichment.py` run over exactly these
//! inputs (only its two input paths were swapped; the algorithm is untouched). Regenerate with:
//!
//! ```text
//! sed -e "s#^TAPE = .*#TAPE = 'tests/fixtures/flow_oracle/tape.jsonl'#" \
//!     -e "s#^LAUNCHES = .*#LAUNCHES = 'tests/fixtures/flow_oracle/launches.jsonl'#" \
//!     /training/v2/code/src/v2/rl/build_c11_flow_enrichment.py > /tmp/oracle_build.py
//! python3 /tmp/oracle_build.py --clocks tests/fixtures/flow_oracle/clocks.jsonl \
//!     --out tests/fixtures/flow_oracle/expected.jsonl
//! ```
//!
//! If this test ever fails, the live reducer has drifted from the corpus the model was
//! trained on — that is a release blocker, not a test to update. The fixture deliberately
//! exercises every one of the thirteen fields and both `no_prior_flow` branches: a never-seen
//! mint, a seen mint with an emptied window, the 60 s/300 s entrant split, the sign of net
//! flow, fresh-share, the 7 d lookback cap, the 2-slot sniper rule, the 3-repeat amount rule,
//! the smart-wallet bar (reached only via net-sold SOL across five mints), 2-shared-co-entry
//! linking, creator self-trading, and both percentiles at their tie points.

use pump_quant_market_state::flow_reducer::{FlowEvent, FlowOutcome, FlowReducer, Side};
use std::collections::BTreeSet;

fn tok<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let i = line.find(&pat)? + pat.len();
    let rest = line[i..].trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        let j = stripped.find('"')?;
        Some(&stripped[..j])
    } else {
        let j = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..j].trim_end())
    }
}

fn hex32(s: &str) -> [u8; 32] {
    assert_eq!(s.len(), 64, "expected a 32-byte hex key, got {s}");
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

fn i64t(line: &str, key: &str) -> i64 {
    tok(line, key).unwrap_or_else(|| panic!("missing {key} in {line}")).parse().expect("int")
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/flow_oracle/{name}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// The same double Python produced, compared bit-for-bit.
fn f64_eq(a: f64, b: f64, what: &str) {
    assert_eq!(a.to_bits(), b.to_bits(), "{what}: {a:?} vs {b:?} (bits)");
}

fn opt_f64(line: &str, key: &str) -> Option<f64> {
    match tok(line, key) {
        Some("null") | None => None,
        Some(v) => Some(v.parse().expect("float")),
    }
}

#[test]
fn live_flow_reducer_matches_the_c11_builder_on_the_micro_tape() {
    let tape = fixture("tape.jsonl");
    let clocks_raw = fixture("clocks.jsonl");
    let launches = fixture("launches.jsonl");
    let expected = fixture("expected.jsonl");

    let mut r = FlowReducer::new();
    for line in launches.lines().filter(|l| !l.trim().is_empty()) {
        let (Some(m), Some(c)) = (tok(line, "mint"), tok(line, "creator")) else {
            continue;
        };
        r.set_creator(hex32(m), hex32(c));
    }

    // Clocks are served in global time order, exactly as the builder sorts them.
    let mut clocks: Vec<(i64, String)> = clocks_raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| (i64t(l, "t_dec_ms"), tok(l, "mint").expect("mint").to_string()))
        .collect();
    clocks.sort();
    let clock_mints: BTreeSet<String> = clocks.iter().map(|(_, m)| m.clone()).collect();
    for m in &clock_mints {
        r.track_mint(hex32(m));
    }
    assert_eq!(clock_mints.len(), 4, "expected four distinct clock mints in the fixture");

    let events: Vec<FlowEvent> = tape
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| FlowEvent {
            mint: hex32(tok(l, "mint").expect("mint")),
            trader: hex32(tok(l, "trader").expect("trader")),
            side: if tok(l, "side") == Some("buy") { Side::Buy } else { Side::Sell },
            slot: i64t(l, "slot") as u64,
            recv_unix_ms: i64t(l, "recv_unix_ms"),
            sol_lamports: i64t(l, "sol_lamports"),
            fee_lamports: i64t(l, "fee_lamports") as u64,
            cu_consumed: tok(l, "cu_consumed").and_then(|v| v.parse().ok()),
        })
        .collect();

    let expected: Vec<&str> = expected.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(expected.len(), clocks.len(), "one expected row per clock");

    let mut applied = 0usize;
    for (i, (t, mint)) in clocks.iter().enumerate() {
        // Apply every event strictly before the clock; never the clock's own instant.
        while applied < events.len() && events[applied].recv_unix_ms < *t {
            r.on_event(&events[applied]);
            applied += 1;
        }
        let row = expected[i];
        assert_eq!(tok(row, "mint"), Some(mint.as_str()), "row {i} mint");
        assert_eq!(i64t(row, "t_dec_ms"), *t, "row {i} t_dec_ms");

        match r.serve(&hex32(mint), *t) {
            FlowOutcome::NoPriorFlow => {
                assert_eq!(tok(row, "no_prior_flow"), Some("true"), "row {i}: builder served flow, reducer said no_prior_flow");
            }
            FlowOutcome::Aggregates(a) => {
                assert!(tok(row, "no_prior_flow").is_none(), "row {i}: builder said no_prior_flow, reducer served flow");
                assert_eq!(a.entrants_60s, i64t(row, "entrants_60s") as u32, "row {i} entrants_60s");
                assert_eq!(a.entrants_300s, i64t(row, "entrants_300s") as u32, "row {i} entrants_300s");
                assert_eq!(a.smart_entrants_300s, i64t(row, "smart_entrants_300s") as u32, "row {i} smart_entrants");
                assert_eq!(a.coentry_wallets_300s, i64t(row, "coentry_wallets_300s") as u32, "row {i} coentry");
                assert_eq!(
                    a.creator_trading_own_mint,
                    tok(row, "creator_trading_own_mint") == Some("true"),
                    "row {i} creator_trading_own_mint"
                );
                assert_eq!(a.entrant_fee_p90_lamports, tok(row, "entrant_fee_p90_lamports").map(|v| v.parse().unwrap()), "row {i} fee p90");
                assert_eq!(a.entrant_cu_p50, tok(row, "entrant_cu_p50").map(|v| v.parse().unwrap()), "row {i} cu p50");
                let v = a.to_corpus_values();
                f64_eq(v.net_flow_sol_300s, opt_f64(row, "net_flow_sol_300s").expect("net_flow"), "net_flow_sol_300s");
                f64_eq(v.smart_net_flow_sol_300s, opt_f64(row, "smart_net_flow_sol_300s").expect("smart_flow"), "smart_net_flow_sol_300s");
                f64_eq(v.flow_lookback_d, opt_f64(row, "flow_lookback_d").expect("lookback"), "flow_lookback_d");
                let shares = [
                    ("fresh_wallet_share_300s", a.fresh_wallet_share_300s_micro),
                    ("sniper_share_300s", a.sniper_share_300s_micro),
                    ("bot_uniform_share_300s", a.bot_uniform_share_300s_micro),
                ];
                for (key, got) in shares {
                    match (got, opt_f64(row, key)) {
                        (Some(g), Some(want)) => f64_eq(f64::from(g) / 1e6, want, key),
                        (None, None) => {}
                        (g, w) => panic!("row {i} {key}: reducer {g:?} vs builder {w:?}"),
                    }
                }
            }
        }
    }
    assert_eq!(applied, events.len(), "every fixture event should be applied by the last clock");
}
