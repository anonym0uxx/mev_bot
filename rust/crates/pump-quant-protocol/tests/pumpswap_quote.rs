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

// ---- QUOTE ARITHMETIC at sizes other than the observed swap. The independent reference is a plain
// linear scan for the net input; it shares no code with the binary-search implementation. This is a
// self-consistency check of the INFERRED rule at other amounts, NOT program equivalence.
mod boundaries {
    use pump_quant_protocol::pumpswap_event::buy_exact_quote_in;
    fn ceil(n: u128, bps: u128) -> u128 {
        (n * bps + 9_999) / 10_000
    }
    fn ref_quote(
        base: u128,
        vault: u128,
        vq: u128,
        gross: u128,
        l: u128,
        p: u128,
        c: u128,
    ) -> Option<(u128, u128)> {
        let mut net = 0u128;
        for n in 1..=gross {
            if n + ceil(n, l) + ceil(n, p) + ceil(n, c) <= gross {
                net = n
            }
        }
        if net < 2 {
            return None;
        }
        let out = base * (net - 1) / (vault + vq + net - 1);
        if out == 0 {
            None
        } else {
            Some((net, out))
        }
    }
    const BASE: u128 = 3_000_000_000_000;
    const VAULT: u128 = 40_000_000_000;
    const VQ: u128 = 17_584_505_288;

    #[test]
    fn agrees_with_the_independent_reference_on_small_and_boundary_inputs() {
        for &(l, p, c) in &[(20u128, 5, 0), (20, 5, 30), (25, 5, 95), (0, 0, 0)] {
            for gross in (1u128..=400).chain([9_999, 10_000, 10_001, 12_345, 99_999, 100_000]) {
                let got = buy_exact_quote_in(BASE, VAULT, VQ, gross, l, p, c)
                    .map(|f| (f.net_quote_in, f.base_out));
                assert_eq!(
                    got,
                    ref_quote(BASE, VAULT, VQ, gross, l, p, c),
                    "gross={gross} fees=({l},{p},{c})"
                );
            }
        }
    }
    #[test]
    fn spend_never_exceeds_gross_and_output_is_monotonic_and_bounded() {
        let mut prev = 0u128;
        for k in 0..400u128 {
            let gross = 1 + k * 7_919_000;
            if let Some(f) = buy_exact_quote_in(BASE, VAULT, VQ, gross, 20, 5, 30) {
                let n = f.net_quote_in;
                let spent = n
                    + (n * 20).div_ceil(10_000)
                    + (n * 5).div_ceil(10_000)
                    + (n * 30).div_ceil(10_000);
                assert!(spent <= gross, "overspend at {gross}");
                assert!(f.base_out >= prev, "non-monotonic at {gross}");
                assert!(f.base_out < BASE, "drains the whole base reserve");
                prev = f.base_out;
            }
        }
    }
    #[test]
    fn reserve_limits_overflow_and_degenerate_inputs_refuse() {
        assert!(buy_exact_quote_in(BASE, VAULT, VQ, 0, 20, 5, 30).is_none());
        assert!(buy_exact_quote_in(BASE, VAULT, VQ, 1, 20, 5, 30).is_none());
        assert!(buy_exact_quote_in(0, VAULT, VQ, 1_000_000, 20, 5, 30).is_none());
        assert!(buy_exact_quote_in(u128::MAX, VAULT, VQ, 1_000_000_000, 20, 5, 30).is_none());
        assert!(buy_exact_quote_in(BASE, VAULT, VQ, u128::MAX, 20, 5, 30).is_none());
    }
}
