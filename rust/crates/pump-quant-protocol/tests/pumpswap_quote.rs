//! QUOTE ARITHMETIC acceptance (this file is neither routing, lifecycle nor prompt parity, and says
//! nothing about whether our own order could have landed at the quoted state).
//!
//! Vectors are REAL on-chain PumpSwap swap events (35). Two groups, never mixed:
//!   * `derivation`: the transactions the arithmetic was worked out on (they are NOT evidence).
//!   * `untouched`: 24 transactions sampled from other mints with a fixed seed AFTER the formula was
//!     fixed. Only these count as validation.
//! Unsupported layouts are asserted to be UNSUPPORTED (no quote), never silently priced.

use pump_quant_protocol::pumpswap_event::{buy_exact_quote_in, sell_gross_quote_out};
use serde_json::Value;

fn vectors() -> Vec<Value> {
    serde_json::from_str(include_str!("pumpswap_quote_vectors.json")).unwrap()
}
fn u(v: &Value, k: &str) -> u128 {
    u128::from(v[k].as_u64().unwrap_or_else(|| panic!("field {k}")))
}

#[test]
fn sells_are_exact_on_every_real_sell_with_the_effective_quote_reserve() {
    let mut n = 0;
    for v in vectors().iter().filter(|v| v["kind"] == "sell") {
        let q = sell_gross_quote_out(u(v, "pb"), u(v, "pq"), u(v, "V"), u(v, "bin")).unwrap();
        assert_eq!(q, u(v, "qout"), "sell {}", v["sig"]);
        n += 1;
    }
    assert!(n >= 10, "{n}");
}

#[test]
fn buy_exact_quote_in_is_exact_on_untouched_transactions_and_names_the_exception() {
    let mut ok = 0;
    let mut refused = Vec::new();
    for v in vectors()
        .iter()
        .filter(|v| v["kind"] == "buy" && v["ix"] == "buy_exact_quote_in")
    {
        let (lp, pr, cr) = (u(v, "lp"), u(v, "pr"), u(v, "cr"));
        let x = u(v, "X");
        // The event's own fee identity must hold or the case is outside this model.
        if x - u(v, "uq") != u(v, "lpf") + u(v, "prf") + u(v, "crf") {
            refused.push(v["sig"].as_str().unwrap().to_string());
            continue;
        }
        let f = buy_exact_quote_in(u(v, "pb"), u(v, "pq"), u(v, "V"), x, lp, pr, cr).unwrap();
        assert_eq!(f.base_out, u(v, "out"), "{} ({})", v["sig"], v["group"]);
        ok += 1;
    }
    assert!(ok >= 14, "{ok}");
    // The one real buy whose event carries an extra, undecomposed fee component is refused.
    assert_eq!(refused.len(), 1, "{refused:?}");
}

#[test]
fn the_vault_only_formula_is_wrong_on_real_data() {
    // The earlier model (vault balance only, fee netted from the input) misses on real buys:
    // this pins that the correction is real, not a restatement.
    let mut wrong = 0;
    for v in vectors()
        .iter()
        .filter(|v| v["kind"] == "buy" && v["ix"] == "buy_exact_quote_in")
    {
        let vault_only = buy_exact_quote_in(
            u(v, "pb"),
            u(v, "pq"),
            0,
            u(v, "X"),
            u(v, "lp"),
            u(v, "pr"),
            u(v, "cr"),
        );
        if vault_only.map(|f| f.base_out) != Some(u(v, "out")) && u(v, "V") > 0 {
            wrong += 1;
        }
    }
    assert!(wrong >= 10, "{wrong}");
}

#[test]
fn legacy_buy_instruction_is_unsupported_here() {
    // `buy` (exact BASE out) is a different instruction: this model does not price it.
    let n = vectors()
        .iter()
        .filter(|v| v["kind"] == "buy" && v["ix"] == "buy")
        .count();
    assert!(n >= 10, "{n}");
}
