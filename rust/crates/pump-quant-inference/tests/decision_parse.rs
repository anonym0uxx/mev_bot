//! Corpus-parity harness for the decision parser.
//!
//! Each fixture is one **real** c11 completion (assistant message) plus the action and the
//! size/price the corpus itself carries. The parser must reproduce the corpus's own reading
//! of its own text — a parser that is stricter than the corpus refuses valid BUYs, and one
//! that is looser invents sizes the model never chose. Both are failures here.
//!
//! Regenerate with `gen_decision_fixtures.py` (see `tests/fixtures/README.md`).

use pump_quant_inference::{parse_decision_payload, Action, SizeTier};
use serde_json::Value;

fn tier_of(v: &Value) -> Option<SizeTier> {
    match v.as_str() {
        None => None,
        Some("SMALL") => Some(SizeTier::Small),
        Some("MID") => Some(SizeTier::Mid),
        Some("FULL") => Some(SizeTier::Full),
        Some(other) => panic!("fixture carries an unknown tier {other}"),
    }
}

#[test]
fn every_corpus_completion_parses_to_what_the_corpus_says_it_is() {
    let raw = include_str!("fixtures/decisions.jsonl");
    let mut n = 0usize;
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(line).expect("fixture json");
        let completion = row["completion"].as_str().expect("completion");
        let want_action = Action::parse(row["action"].as_str().expect("action")).expect("action");
        let want_size = tier_of(&row["size"]);
        let want_price = row["price_limit"].as_f64();

        let got = parse_decision_payload(completion)
            .unwrap_or_else(|e| panic!("fixtures/decisions.jsonl:{}: {e}", i + 1));
        assert_eq!(got.action, want_action, "action drifted at line {}", i + 1);
        assert_eq!(got.size, want_size, "size drifted at line {}", i + 1);
        match (got.price_limit, want_price) {
            (None, None) => {}
            (Some(a), Some(b)) => assert!(
                (a - b).abs() <= b.abs() * 1e-12,
                "price limit drifted at line {}: {a} vs {b}",
                i + 1
            ),
            (a, b) => panic!("price-limit presence drifted at line {}: {a:?} vs {b:?}", i + 1),
        }
        n += 1;
    }
    assert!(n >= 100, "expected a real fixture set, saw {n}");
}
