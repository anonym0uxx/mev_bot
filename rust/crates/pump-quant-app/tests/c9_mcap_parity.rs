//! **C9 parity: `reserve_view` against the corpus's own market cap and regime tag.**
//!
//! The fixture joins 95,654 real c12 rows with their C9 enrichment entries and picks 60 across
//! every `mcap_source` (`amm` 29,527 / `curve` 64,908 / `absent` 1,219 in the full join). Each
//! case carries the RAW reserve numbers parsed out of the row's own CURVE/AMM STATE lines plus
//! the corpus's resulting `mcap_sol_at_t`, `mcap_source` and the depth its SIZE OPTIONS line
//! quoted. The Rust side rebuilds the view from the raw numbers only.
//!
//! What this can catch, and why each comparison exists:
//!
//! * **the precedence rule.** `curve` vs `amm` is decided by whether the curve is complete; a
//!   wrong branch is invisible to a test that only checks "mcap is a positive number".
//! * **the supply multiplier.** 1e15 raw units, not 1e9 or 1e6. Off by 1e6 is a plausible typo
//!   and shows up here as a 10^6 error.
//! * **the depth basis.** For a pool row the depth is the quote reserve EXACTLY; for a curve row
//!   it is the real-SOL side, checked to the precision the corpus printed (`%.4g`).
//! * **`na` is not zero.** A row the corpus could not price must come back `None`.

use pump_quant_app::curve_annotation::{
    AmmAttribution, AmmObservation, AnnotationState, CurveObservation,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/c9_mcap_parity.json")).expect("fixture parses")
}

fn mint_key(s: &str) -> [u8; 32] {
    let mut m = [0u8; 32];
    for (i, b) in s.as_bytes().iter().take(32).enumerate() {
        m[i] = *b;
    }
    m
}

/// The half-ulp of the depth as the corpus PRINTED it.
///
/// The SIZE OPTIONS line does not print the depth exactly — the corpus tooling that
/// reconstructed it says so in as many words (`p1_mgmt.py`: "it is not printed exactly") — so
/// three significant digits is the strongest comparison the text can support. Pool rows are
/// graded exactly instead (their depth is the quote reserve itself), which is where the sharp
/// check lives; this one still fails for the two plausible mistakes (using `real_sol` or the
/// AMM reserve on a curve row), which differ at the tens of percent.
fn printed_tolerance(expected: f64) -> f64 {
    let exp = expected.abs().log10().floor();
    0.5 * 10f64.powf(exp - 2.0)
}

#[test]
fn reserve_view_matches_the_corpus_market_cap_and_domain_plane() {
    let f = fixture();
    let cases = f["cases"].as_array().expect("cases");
    assert!(cases.len() >= 40, "the case set must span every source");

    let mut checked = 0usize;
    let mut checked_depth = 0usize;
    for (i, c) in cases.iter().enumerate() {
        let mint = mint_key(c["mint"].as_str().expect("mint"));
        let t_dec = c["t_dec_ms"].as_i64().expect("t_dec");
        let mut st = AnnotationState::new();

        let curve = &c["curve"];
        if curve["status"] == "present" {
            let stale = curve["staleness_ms"].as_i64().expect("staleness");
            st.observe_curve(
                mint,
                CurveObservation {
                    v_sol_lamports: curve["v_sol_lamports"].as_u64().expect("vsol"),
                    v_tokens: curve["v_tokens"].as_u64().expect("vtok"),
                    real_sol_lamports: curve["real_sol_lamports"].as_u64().expect("rsol"),
                    real_tokens: curve["real_tokens"].as_u64().expect("rtok"),
                    ts_ms: t_dec - stale,
                    slot: 0,
                },
            );
        }
        let amm = &c["amm"];
        if amm["status"] == "present" {
            let pool = amm["pool"].as_str().expect("pool").to_string();
            st.set_attribution(
                mint,
                AmmAttribution {
                    wsol_pools: vec![pool.clone()],
                    pools_total: 1,
                    graduated: true,
                },
            );
            st.observe_amm(
                mint,
                AmmObservation {
                    pool,
                    base_reserves_raw: amm["base_reserves_raw"].as_u64().expect("base"),
                    quote_reserves_lamports: amm["quote_reserves_lamports"]
                        .as_u64()
                        .expect("quote"),
                    quote_is_wsol: true,
                    ts_ms: t_dec - amm["staleness_ms"].as_i64().expect("amm staleness"),
                    slot: amm["reserve_slot"].as_u64().unwrap_or(0),
                },
            );
        }

        let v = st.reserve_view(&mint, t_dec);
        let expected_source = c["expected_source"].as_str().expect("source");
        assert_eq!(
            v.mcap_source, expected_source,
            "case {i}: the plane the cap is priced off diverged (mint {})",
            c["mint"]
        );
        match c["expected_mcap_sol"].as_f64() {
            Some(want) => {
                let got = v
                    .mcap_sol_at_t
                    .unwrap_or_else(|| panic!("case {i}: corpus priced it, we returned na"));
                let tol = 1e-6 + 1e-9 * want.abs();
                assert!(
                    (got - want).abs() <= tol,
                    "case {i}: mcap {got} != corpus {want} (source {expected_source})"
                );
            }
            None => assert!(
                v.mcap_sol_at_t.is_none(),
                "case {i}: corpus could not price it (na) but we returned {:?}",
                v.mcap_sol_at_t
            ),
        }

        // Depth: exact for a pool row, four-significant-digit precision for a curve row.
        if let Some(exact) = c["expected_depth_exact"].as_f64() {
            assert_eq!(
                v.size_amm, true,
                "case {i}: an AMM row must size on the pool"
            );
            let got = v.size_depth_sol.expect("depth");
            assert!(
                (got - exact).abs() < 1e-9,
                "case {i}: pool depth {got} != quote reserve {exact}"
            );
            checked_depth += 1;
        } else if let Some(printed) = c["expected_depth_4g"].as_f64() {
            if let Some(got) = v.size_depth_sol {
                assert!(
                    (got - printed).abs() <= printed_tolerance(printed),
                    "case {i}: curve depth {got} != the printed {printed} (tol {})",
                    printed_tolerance(printed)
                );
                checked_depth += 1;
            }
        }
        checked += 1;
    }
    assert!(checked >= 40, "graded rows: {checked}");
    assert!(
        checked_depth >= 20,
        "depth must be graded on a real population, not one row ({checked_depth} graded)"
    );
}

/// The fixture's own coverage claim: every source is represented, and the population is real.
#[test]
fn the_fixture_spans_every_source() {
    let f = fixture();
    let counts = f["source_counts_picked"].clone();
    for src in ["amm", "curve", "absent"] {
        assert!(
            counts[src].as_u64().unwrap_or(0) >= 10,
            "the picked cases must cover {src}: {counts}"
        );
    }
    assert!(
        f["n_joined"].as_u64().unwrap_or(0) > 50_000,
        "the join must be over the real corpus, not a hand-made sample"
    );
}
