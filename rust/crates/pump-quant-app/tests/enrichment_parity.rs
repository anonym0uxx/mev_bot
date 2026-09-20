//! Byte-parity check for the enriched candidate state against the corpus producer's own
//! expressions.
//!
//! The fixture (`tests/fixtures/enrichment_parity.json`) is emitted by
//! `/training/v2/code/src/v2/rl/parity_rust/gen_enrichment_fixture.py`, which transcribes the
//! snapshot loop of `build_c9_enrichment_full.py` (the script that wrote the `enriched` field of
//! every c-series row) and computes the expected record with those expressions.
//!
//! Integers are pinned exactly, because an off-by-one in `holders_at_t` or `bundle_wallets` is a
//! different prompt. Floats are pinned to 1e-9: `holder_hhi` is a float sum, Python sums it in
//! dict-insertion order and the Rust port sums in address order, and float addition is not
//! associative. Byte-identical rendering additionally needs the corpus's `round(x, 6)`
//! ties-to-even, which is an owed decision tracked outside this test.

use std::collections::BTreeMap;

use pump_quant_app::enrichment::{enrich, EnrichmentGap, EnrichmentTrade};
use serde_json::Value;

fn fixture() -> Value {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/enrichment_parity.json"
    ))
    .expect("fixture present");
    serde_json::from_str(&raw).expect("fixture parses")
}

fn decode_wallet(hex: &str) -> [u8; 32] {
    assert_eq!(hex.len(), 64, "wallet is 32 bytes of hex");
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

fn num(v: &Value, key: &str) -> f64 {
    v.get(key)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("missing {key}"))
}

#[test]
fn enrichment_matches_the_corpus_producer() {
    let f = fixture();
    let cases = f["cases"].as_array().expect("cases");
    assert!(
        cases.len() >= 100,
        "fixture should be broad: {}",
        cases.len()
    );

    let mut snapshots = 0usize;
    let mut refused = 0usize;
    let mut gaps_seen: BTreeMap<String, usize> = BTreeMap::new();

    for (i, case) in cases.iter().enumerate() {
        let trades: Vec<EnrichmentTrade> = case["trades"]
            .as_array()
            .expect("trades")
            .iter()
            .map(|t| EnrichmentTrade {
                recv_unix_ms: t["recv_unix_ms"].as_i64().expect("clock"),
                trader: decode_wallet(t["trader"].as_str().expect("trader")),
                tokens_raw: t["tokens_raw"].as_i64().expect("tokens") as i128,
                sol_lamports: t["sol_lamports"].as_u64().expect("sol"),
                slot: t["slot"].as_u64(),
            })
            .collect();
        let t_dec = case["t_dec_ms"].as_i64().expect("t_dec");

        match (case["expected"].as_object(), case["gap"].as_str()) {
            (Some(expected), _) => {
                let got = enrich(&trades, t_dec).unwrap_or_else(|e| {
                    panic!("case {i}: corpus produced a snapshot, we gave {e:?}")
                });
                for (key, want) in [
                    (
                        "holders_at_t",
                        num(&Value::Object(expected.clone()), "holders_at_t"),
                    ),
                    (
                        "bundle_slots",
                        num(&Value::Object(expected.clone()), "bundle_slots"),
                    ),
                    (
                        "bundle_wallets",
                        num(&Value::Object(expected.clone()), "bundle_wallets"),
                    ),
                    (
                        "n_buys_at_t",
                        num(&Value::Object(expected.clone()), "n_buys_at_t"),
                    ),
                    (
                        "n_sells_at_t",
                        num(&Value::Object(expected.clone()), "n_sells_at_t"),
                    ),
                    (
                        "round_trip_wallets",
                        num(&Value::Object(expected.clone()), "round_trip_wallets"),
                    ),
                ] {
                    let have = match key {
                        "holders_at_t" => got.holders_at_t,
                        "bundle_slots" => got.bundle_slots,
                        "bundle_wallets" => got.bundle_wallets,
                        "n_buys_at_t" => got.n_buys_at_t,
                        "n_sells_at_t" => got.n_sells_at_t,
                        _ => got.round_trip_wallets,
                    };
                    assert_eq!(have, want as u64, "case {i}: {key}");
                }
                for (key, have) in [
                    ("top1_float_share", got.top1_float_share),
                    ("top5_float_share", got.top5_float_share),
                    ("holder_hhi", got.holder_hhi),
                    ("volume_sol_at_t", got.volume_sol_at_t),
                    ("wash_ratio", got.wash_ratio),
                ] {
                    let want = num(&Value::Object(expected.clone()), key);
                    // The corpus RECORD is rounded to six decimals (build_c9_enrichment_full.py
                    // stores `round(x, 6)`), so the comparison is against that precision. How the
                    // live renderer formats the same number for the prompt is the separately
                    // owed byte-parity decision (Python's ties-to-even vs Rust's).
                    let rounded = (have * 1e6).round() / 1e6;
                    assert!(
                        (rounded - want).abs() < 1e-9,
                        "case {i}: {key} corpus={want} rust={have} (rounded {rounded})"
                    );
                }
                snapshots += 1;
            }
            (None, Some(gap)) => {
                let expected = match gap {
                    "insufficient_history" => EnrichmentGap::InsufficientHistory,
                    "inconsistent_shares" => EnrichmentGap::InconsistentShares,
                    other => panic!("unknown gap {other}"),
                };
                assert_eq!(
                    enrich(&trades, t_dec),
                    Err(expected),
                    "case {i}: the corpus refused this row, and so must we"
                );
                *gaps_seen.entry(gap.to_string()).or_insert(0) += 1;
                refused += 1;
            }
            (None, None) => panic!("case {i} has neither snapshot nor gap"),
        }
    }

    // A fixture that only ever exercised one branch would prove much less than it appears to.
    assert!(snapshots >= 50, "snapshots: {snapshots}");
    assert!(refused >= 10, "refusals: {refused}");
    assert!(
        gaps_seen.contains_key("insufficient_history"),
        "gaps seen: {gaps_seen:?}"
    );
}
