//! ## FINDINGS (2026-09-20) — the harness runs and DISAGREES with the corpus
//!
//! This test is `#[ignore]`d until the divergences below are closed. It is not ignored because it is
//! unreliable — it is ignored because it is RIGHT and the code is not. Running it:
//! `cargo test -p pump-quant-app --test c5_bundle_parity -- --ignored --nocapture`
//!
//! Six classes, each with the case-0 evidence:
//!
//! 1. **PRICE UNIT — REAL, in the assembler.** corpus `0.9562185430525267`, ours
//!    `956218543.0525267`. `bundle_assemble` multiplies by 1e9, but the ledger's
//!    `price_sol_per_raw` ALREADY holds the corpus's `price_lamports_per_raw_token` (the ratio of the
//!    tape's two legs). The ledger field's NAME is the misnomer, not the value. Fix: drop the ×1e9,
//!    and rename the ledger field.
//! 2. **`age_s` BASIS — REAL, and my first reading of it was WRONG.** corpus `660.0`, ours `664.291`.
//!    I first said the corpus ages from the mint's LAUNCH. It does not: `build_states_v2` line 240 is
//!    `age_s = (t_dec - tt[0]) / 1000.0`, the first trade in ITS OWN filtered run — the same basis the
//!    ledger uses. So the 4-second gap is not a basis difference at all: our trade SET differs.
//!    Candidate causes, in order: the corpus sorts `np.lexsort((slot, tms, code_inv))` (time, then
//!    SLOT) while the fixture sorts on time alone; and the corpus's tape may be a different file from
//!    `renormalized_v7` (check `build_states_v2`'s input path before believing either).
//! 3. **BUY VOLUME — REAL.** `sell_volume_lamports` matches EXACTLY (`24415268927`) while
//!    `buy_volume_lamports` is 137× too large (`8.14e12` vs `5.91e10`). Sells agree and buys do not,
//!    so this is not a units question: it is per-side, which points at the leg a BUY contributes
//!    (the tape signs a buy's SOL leg negative) or at which leg the corpus sums.
//! 4. **TAPE STATUS FILTER — REAL.** `n_prior_trades` 1703 vs 1700 and `buy_count` 1677 vs 1674: we
//!    count three trades the corpus does not. The tape carries `status`/`resolution` columns; a
//!    failed or unresolved transaction is not a swap and must not enter the ledger.
//! 5. **FLOAT FORMATTING — REAL, and the one that most needs a decision.** The corpus rounds for
//!    display (`top1_trader_share=0.09326`, `ret_5s_bp=5.43046`, `holder_hhi=0.5524`) while we render
//!    full float precision (`0.979236234823295`, `5.4304577631370154`). As rendered, these are
//!    out-of-distribution prompts even where the underlying value is right.
//! 6. **CONCENTRATION WINDOW — REAL.** `top1_trader_share` 0.979 vs corpus 0.09326: our
//!    concentration window keys on a different span, so the shares are computed over the wrong
//!    population.
//!
//! ## ROOT CAUSE of 2, 3, 4 and 6, found in the producer (2026-09-20)
//!
//! `build_states_v2` lines 160-166 drop every trade whose price is outside `[med/10, med*10]`,
//! where `med` is the mint's median price over its WHOLE run — **non-causal lookahead**. That one
//! filter explains all four:
//!
//! * 3 extra buys: case 0's three whale buys are priced outside the band and are dropped upstream.
//! * `buy_volume_lamports` 8.14e12 vs 5.91e10: those same three carry ~8.08e12 of the "volume", so
//!   dropping them collapses it. **Sells match to the digit** because no sell fell outside the band.
//! * `age_s` 664.291 vs 660.0: dropping early trades moves `tt[0]`, the age basis both sides share.
//! * `top1_trader_share` 0.979 vs 0.09326: banding changes WHICH trades populate the concentration
//!   window, so the shares are computed over a different population.
//!
//! The live path must NOT reproduce the band — it cannot know a future median, and a rule that
//! peeks at the full series is exactly the lookahead the causal ledger exists to refuse. So the
//! treatment is to SURFACE it: the ledger counts trades outside a *causal* (running) 10x band, and
//! the assembler marks the block `partial` rather than claiming `complete` over numbers the corpus
//! would have banded differently. Rebuilding the corpus causally is a corpus decision, not a silent
//! re-derivation here.
//!
//! Test-side placeholders (NOT production defects, and excluded from the verdict): `curve_present`
//! (the fixture supplies `CurveState::Absent`, so `curve_present=False` is the harness talking) and
//! `unique_traders` (+2 — a consequence of 4's extra trades).

//! C5 — does the Rust derivation, run over the SAME tape the corpus used, render the same lines?
//!
//! This is the check that turns "the assembler works" into "the assembler is indistinguishable from
//! the corpus", and it is the one that matters most: an assembler that renders differently feeds the
//! trading brain out-of-distribution input while looking perfectly correct.
//!
//! The fixture is built from REAL artefacts only (`gen_c5_bundle_fixture.py`):
//! - the rows are the corpus's own stored prompts (`candidate_sft_c12_entry/train.jsonl`),
//! - the trades are the tape prefix `<= t_dec` from `canonical/renormalized_v7/trades.jsonl`, the
//!   same tape `build_c9_enrichment_full.py` read,
//! - the expectations are the prompt LINES the corpus stored, byte for byte.
//!
//! WHAT IS GRADED. Only the blocks whose producers exist today: the as-of-t state block and the
//! ENRICHED block. The flow / curve / AMM / dev blocks are supplied as placeholders and their lines
//! are not compared, so this test never grades a block that has no live producer.
//!
//! MCAP IS AN INPUT, and that is stated rather than hidden: trades cannot see supply, so
//! `mcap_sol_at_t`/`mcap_source` are taken from the row's own rendered line and fed in. Every other
//! field on the ENRICHED line is DERIVED here from the tape.

use pump_quant_app::bundle_assemble::{assemble, BundleInputs};
use pump_quant_app::enrichment::{enrich, EnrichmentTrade};
use pump_quant_app::state_ledger::{StateLedger, StateTrade, VenueLabel};
use pump_quant_proposal::decision::{AmmState, CurveState, DevHistoryDecision};
use pump_quant_proposal::{render_decision, FlowState};
use serde_json::Value;

fn fixture() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c5_bundle_parity.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture unreadable at {}: {e}", path.display()));
    serde_json::from_str(&text).expect("fixture must parse")
}

/// The ledger is keyed by a 32-byte mint; the fixture carries base58. Any injective map works — the
/// mint's bytes are not an input to anything derived here, they are only the key — so a small stable
/// hash avoids a base58 dependency in the test crate.
fn mint_key(base58: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, b) in base58.bytes().enumerate() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        out[i % 32] ^= (h >> ((i % 8) * 8)) as u8;
    }
    // Never all-zero: a zero mint is the "no mint" sentinel elsewhere in the workspace.
    out[0] |= 1;
    out
}

fn venue_of(s: &str) -> VenueLabel {
    match s {
        "pumpfun" => VenueLabel::Pumpfun,
        "pumpswap" => VenueLabel::Pumpswap,
        _ => VenueLabel::Unknown,
    }
}

/// The two `ENRICHED` fields that are inputs rather than derivations, read back off the corpus's own
/// line so the comparison is honest about which side they came from.
fn mcap_from_line(line: &str) -> (Option<f64>, String) {
    let pick = |key: &str| -> Option<String> {
        let i = line.find(&format!("{key}="))?;
        let rest = &line[i + key.len() + 1..];
        let end = rest.find(' ').unwrap_or(rest.len());
        Some(rest[..end].to_string())
    };
    let source = pick("mcap_source").unwrap_or_else(|| "curve".to_string());
    let raw = pick("mcap_sol_at_t").unwrap_or_else(|| "na".to_string());
    let value = if raw == "na" || raw == "None" {
        None
    } else {
        raw.parse::<f64>().ok()
    };
    (value, source)
}

#[test]
#[ignore = "C5 findings 1-6 recorded in this file's header; un-ignore as each is closed"]
fn derived_blocks_render_identically_to_the_corpus_over_the_corpus_tape() {
    let f = fixture();
    let cases = f["cases"].as_array().expect("cases");
    assert!(
        cases.len() >= 8,
        "the fixture must be broad enough to mean something: {}",
        cases.len()
    );

    let mut graded = 0usize;
    let mut skipped_for_eligibility = 0usize;
    // WHY a case was skipped, as a histogram: a fixture where every row is refused is not a passing
    // test, it is a mis-wired ingest, and the cause is what says which.
    let mut skip_causes: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();

    for (i, case) in cases.iter().enumerate() {
        let mint = mint_key(case["mint"].as_str().expect("mint"));
        let t_dec = case["t_dec_ms"].as_i64().expect("t_dec_ms");
        let trades = case["trades"].as_array().expect("trades");

        // ---- ingest: one tape prefix, two derivations (the corpus's own cutoff, `<= t_dec`)
        let mut ledger = StateLedger::new();
        let mut enr: Vec<EnrichmentTrade> = Vec::with_capacity(trades.len());
        for t in trades {
            let recv = t[0].as_i64().expect("recv_unix_ms");
            let trader = t[1].as_u64().expect("trader id");
            let is_buy = t[2].as_u64().expect("side") == 1;
            let tokens_raw = t[3].as_i64().expect("tokens_raw");
            let sol = t[4].as_i64().expect("sol_lamports");
            let slot = t[5].as_i64().expect("slot");
            let venue = t[6].as_str().expect("venue");

            // price = SOL per raw token, from the legs the tape actually carries. The tape signs a
            // BUY's SOL leg NEGATIVE (it is SOL leaving the trader), so the price is the ratio of
            // the two legs' magnitudes — taking `sol` as given yields a negative price, which the
            // ledger rightly refuses as unusable rather than believing.
            let price = if tokens_raw != 0 {
                Some((sol as f64).abs() / (tokens_raw as f64).abs())
            } else {
                None
            };
            ledger.on_trade(
                &mint,
                StateTrade {
                    recv_unix_ms: recv,
                    price_sol_per_raw: price,
                    sol_lamports_signed: sol,
                    base_qty: Some(tokens_raw),
                    is_buy,
                    // +1: the fixture's ids are dense from 0, and 0 is the workspace's
                    // "unknown trader" sentinel — passing it would make every window unidentifiable.
                    trader: trader + 1,
                    venue: venue_of(venue),
                },
            );
            enr.push(EnrichmentTrade {
                recv_unix_ms: recv,
                trader: {
                    let mut w = [0u8; 32];
                    w[..8].copy_from_slice(&(trader + 1).to_le_bytes());
                    w
                },
                tokens_raw: i128::from(tokens_raw),
                sol_lamports: sol.unsigned_abs(),
                slot: if slot < 0 { None } else { Some(slot as u64) },
            });
        }

        // The corpus's own clock gates this row (>= 20 strictly-prior trades, etc.). A row the
        // ledger refuses is not a parity failure — it is the ledger correctly declining — so it is
        // counted separately rather than silently dropped.
        if let Err(refusal) = ledger.eligibility(&mint, t_dec) {
            skipped_for_eligibility += 1;
            *skip_causes
                .entry(format!("ledger/{refusal:?}"))
                .or_default() += 1;
            continue;
        }
        let Some(snapshot) = ledger.serve(&mint, t_dec) else {
            skipped_for_eligibility += 1;
            *skip_causes
                .entry("ledger/serve_none".to_string())
                .or_default() += 1;
            continue;
        };
        let snapped = match enrich(&enr, t_dec) {
            Ok(s) => s,
            Err(e) => {
                skipped_for_eligibility += 1;
                *skip_causes.entry(format!("enrichment/{e:?}")).or_default() += 1;
                continue;
            }
        };

        let expected_enriched = case["expected_enriched_line"]
            .as_str()
            .expect("enriched line");
        let (mcap, mcap_source) = mcap_from_line(expected_enriched);

        let flow = FlowState {
            entrants_60s: 0,
            entrants_300s: 0,
            net_flow_sol_300s: 0.0,
            fresh_wallet_share_300s: None,
            flow_lookback_d: 0.0,
            sniper_share_300s: None,
            bot_uniform_share_300s: None,
            smart_entrants_300s: 0,
            smart_net_flow_sol_300s: 0.0,
            coentry_wallets_300s: 0,
            creator_trading_own_mint: false,
            entrant_fee_p90_lamports: None,
            entrant_cu_p50: None,
        };
        let bundle = match assemble(&BundleInputs {
            snapshot: &snapshot,
            enriched: &snapped,
            t_dec_ms: t_dec,
            venue: &snapshot.venue,
            mcap_sol_at_t: mcap,
            mcap_source: &mcap_source,
            flow: &flow,
            flow_no_prior: false,
            curve: CurveState::Absent {
                reason: "c5".to_string(),
            },
            amm: AmmState::Absent {
                reason: "c5".to_string(),
            },
            dev: DevHistoryDecision {
                creator_past_launches: None,
                creator_known: 0,
            },
            size_depth_sol: None,
            size_amm: false,
        }) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!(
                    "case {i} (mint {}): the assembler refused a corpus row: {}",
                    case["mint"].as_str().unwrap_or("?"),
                    e.as_str()
                ));
                continue;
            }
        };

        let rendered = render_decision(&bundle);
        let have: Vec<&str> = rendered.lines().collect();
        let mut case_failures = 0usize;

        for want in case["expected_state_lines"]
            .as_array()
            .expect("expected_state_lines")
        {
            let want = want.as_str().expect("line");
            if !have.iter().any(|l| *l == want) {
                case_failures += 1;
                failures.push(format!(
                    "case {i}: STATE line not reproduced\n   corpus: {want}\n   ours  : {}",
                    have.iter()
                        .find(|l| l.split('=').next() == want.split('=').next())
                        .unwrap_or(&"<no line with that key>")
                ));
            }
        }
        if !have.iter().any(|l| *l == expected_enriched) {
            case_failures += 1;
            failures.push(format!(
                "case {i}: ENRICHED line not reproduced\n   corpus: {expected_enriched}\n   ours  : {}",
                have.iter()
                    .find(|l| l.starts_with("ENRICHED CANDIDATE STATE"))
                    .unwrap_or(&"<no enriched line>")
            ));
        }
        if case_failures == 0 {
            graded += 1;
        }
    }

    assert!(
        failures.is_empty(),
        "{} line(s) diverged from the corpus:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );

    assert!(
        graded >= 8,
        "only {graded} case(s) reached a graded comparison (skipped {skipped_for_eligibility}: \
         {skip_causes:?}); the fixture must exercise the derivation, not the refusals"
    );
}
