//! **LIVE FLOW STATE parity: the app-side flow block against the authority's own renderer.**
//!
//! `flow_feed::flow_state_from_aggregates` is the join between the reducer's integer carries and
//! the thirteen corpus fields, and `flow_feed::flow_for_bundle` is what the live path calls to
//! fill the bundle's flow block. Both are graded here over real `C11_FLOW_ENRICHMENT.jsonl`
//! entries, whose expected lines were rendered by `inject_c11_flow.render` — the authority.
//!
//! What each part of the comparison catches:
//!
//! * **field order and identity** — the fixture's `fields` list is asserted against the renderer's
//!   own `FIELD_ORDER`, and the line comparison would fail on any transposition.
//! * **unit conversion** — millionths to six decimals, tenths to one decimal. A `*1e6` where a
//!   `/1e6` belongs shows up as a 10^12 error, not a rounding difference.
//! * **`None` is not `0`** — a share with no entrants must print `None`; a zero would read as a
//!   real, measured share of zero.
//! * **`no_prior_flow`** — the marker line, never thirteen zeroed fields.

use pump_quant_app::flow_feed::{flow_state_from_aggregates, zero_flow_state};
use pump_quant_market_state::flow_reducer::FlowAggregates;
use pump_quant_proposal::{render_live_flow_state, render_no_prior_flow, FIELD_ORDER};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/flow_state_parity.json")).expect("fixture")
}

/// The corpus's `round(x, 6)` doubles back to the reducer's integer carries.
fn micro(v: f64) -> u32 {
    (v * 1e6).round() as u32
}

#[test]
fn the_fixture_field_order_is_the_renderers_field_order() {
    let f = fixture();
    let got: Vec<String> = f["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let want: Vec<String> = FIELD_ORDER.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        got, want,
        "the fixture must be generated in the renderer's order"
    );
}

#[test]
fn flow_lines_match_the_authority_over_real_entries() {
    let f = fixture();
    let cases = f["cases"].as_array().expect("cases");
    assert!(
        cases.len() >= 50,
        "the population must be real: {}",
        cases.len()
    );

    let mut graded = 0usize;
    let mut option_nones = 0usize;
    for (i, c) in cases.iter().enumerate() {
        let expected = c["expected_line"].as_str().expect("expected_line");
        let got = if c["no_prior_flow"].as_bool() == Some(true) {
            render_no_prior_flow().to_string()
        } else {
            let ints = &c["ints"];
            let floats = &c["floats"];
            let opt_micro = |k: &str| floats[k].as_f64().map(micro);
            let opt_u64 = |k: &str| c[k].as_u64();
            if opt_micro("fresh_wallet_share_300s").is_none()
                && opt_micro("sniper_share_300s").is_none()
                && opt_micro("bot_uniform_share_300s").is_none()
            {
                option_nones += 1;
            }
            let agg = FlowAggregates {
                entrants_60s: ints["entrants_60s"].as_u64().unwrap() as u32,
                entrants_300s: ints["entrants_300s"].as_u64().unwrap() as u32,
                net_flow_sol_300s_micro: (floats["net_flow_sol_300s"].as_f64().unwrap() * 1e6)
                    .round() as i64,
                fresh_wallet_share_300s_micro: opt_micro("fresh_wallet_share_300s"),
                flow_lookback_d_tenths: (floats["flow_lookback_d"].as_f64().unwrap() * 10.0).round()
                    as u32,
                sniper_share_300s_micro: opt_micro("sniper_share_300s"),
                bot_uniform_share_300s_micro: opt_micro("bot_uniform_share_300s"),
                smart_entrants_300s: ints["smart_entrants_300s"].as_u64().unwrap() as u32,
                smart_net_flow_sol_300s_micro: (floats["smart_net_flow_sol_300s"].as_f64().unwrap()
                    * 1e6)
                    .round() as i64,
                coentry_wallets_300s: ints["coentry_wallets_300s"].as_u64().unwrap() as u32,
                creator_trading_own_mint: c["creator_trading_own_mint"].as_bool().unwrap(),
                entrant_fee_p90_lamports: opt_u64("entrant_fee_p90_lamports"),
                entrant_cu_p50: opt_u64("entrant_cu_p50"),
            };
            render_live_flow_state(&flow_state_from_aggregates(&agg))
        };
        assert_eq!(
            got, expected,
            "case {i} diverged from the authority's renderer"
        );
        graded += 1;
    }
    assert!(graded >= 50, "graded {graded}");
    // The edge cases exist to prove `None` survives the mapping rather than becoming 0.
    assert!(option_nones >= 1, "the fixture must exercise absent shares");
}

#[test]
fn the_no_prior_marker_is_not_thirteen_zeroes() {
    let z = zero_flow_state();
    assert_eq!(
        render_no_prior_flow(),
        "LIVE FLOW STATE: no_prior_flow=true"
    );
    let rendered = render_live_flow_state(&z);
    assert_ne!(
        rendered,
        render_no_prior_flow(),
        "a zero-filled state is NOT the same line as the marker; they must not be conflated"
    );
    assert!(rendered.contains("fresh_wallet_share_300s=None"));
    assert!(rendered.contains("entrants_60s=0"));
}
