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
//! - the expectations are the prompt LINES the corpus stored, byte for byte,
//! - and, per case, the corpus's own robust-band median (see below), computed from the SAME tape.
//!
//! ## What this test grades, and the one thing it cannot
//!
//! TWO readings of every case, from one ingest path:
//!
//! 1. **CAUSAL** — every tape trade at or before `t_dec`, exactly what the live ledger sees.
//! 2. **CORPUS-SIDE** — the same trades, minus those the corpus's own filter would have dropped:
//!    `build_states_v2` lines 159-165 delete every trade whose price is outside
//!    `[median/10, median*10]`, where `median` is the mint's median price over its WHOLE run —
//!    including trades AFTER `t_dec`. That is lookahead: a live reader cannot know it, and at the
//!    mint's earliest clocks there is no reference at all, so the corpus's state block for those
//!    clocks is UNDERSERVABLE by construction.
//!
//! The corpus-side reading must reproduce every STATE line BYTE FOR BYTE, and every causal STATE
//! divergence must be one the band itself produces (causal line ≠ corpus-side line for that key).
//! That pair of assertions is the whole claim: the derivation is identical given the same trade set,
//! and the residual is a corpus-side lookahead defect, not a live-path bug. Independent
//! confirmation (`/tmp/verify_band.py`): the corpus's OWN `_episode`, run over its own banded tape,
//! reproduces all six STATE lines of all 12 cases; the banded trade set is what the band median in
//! the fixture reconstructs.
//!
//! THE PROMPT JOINS TWO PRODUCERS OVER TWO TRADE SETS, and the test grades each against its own:
//! the STATE block comes from `build_states_v2` (bands), the ENRICHED block from
//! `build_c9_enrichment_full` (does not — it reads the tape prefix directly). So the ENRICHED line is
//! graded against the CAUSAL reading, and it matches: the live path reproduces that block exactly
//! today. Only the STATE block carries the corpus's band.
//!
//! ## Findings this harness forced, all CLOSED (2026-09-20)
//!
//! 1. **PRICE UNITS — was REAL, in the assembler.** The corpus stores
//!    `price_lamports_per_raw_token=0.9562185430525267` for a row whose tape ratio is the same
//!    number: the ledger's field (per the corpus's own misnomer, `price_sol_per_raw`) already holds
//!    lamports per raw token. `bundle_assemble` multiplied by 1e9, putting every price nine orders
//!    out. Fixed by identity, and the ledger field is now named `price_lamports_per_raw_token`
//!    (`state_ledger::StateTrade::from_market_trade` also converted the wire value with the wrong
//!    scale — `/1e18` where the corpus's field is `/1e9`).
//! 2. **`age_s` / counts / volumes / shares — the BAND**, per the two-reading proof above.
//! 3. **`curve_present` was REAL, in the assembler.** The corpus's `curve_venue_present` is defined
//!    as `venue != "unknown"` — an OBSERVATION. The assembler read it off the curve ANNOTATION, so a
//!    bundle over a live venue rendered `curve_present=False`. Now sourced from the served state.
//! 4. **TAPE STATUS FILTER** — the corpus's tapes carry `status`/`resolution`; this fixture's tape is
//!    the renormalized one, where the value-leg floors (`|sol| >= 1e5 AND |tok| >= 1e6`) do the work.
//!    The ledger applies both floors; the residual count differences are the band's.
//! 5. **FLOAT FORMATTING — was REAL, and the subtlest of the six.** The corpus renders the STATE
//!    line's return / volatility / share / ratio fields through `serialize_families.py`'s `fnum`
//!    ladder (`%.2fe9` / `%.2fe6` above 1e9 / 1e6, `%.0f` above 1e3, else `%.6g`), NOT
//!    `str(float)`. A live value carries full precision, so `ret_5s_bp=304.32986` rendered as
//!    `304.3299` where the corpus wrote `304.33`, and `vol_30s_bp=2329.60057` as `2329.60057` where
//!    the corpus wrote `2330` — the same numbers as different token sequences. Ported to
//!    `fmt::py_fnum`; nine divergences closed by it. **The P1 fixtures could not have caught this**:
//!    they round-trip the corpus's already-shortened text, and `str(float)` of a value the corpus had
//!    already put through `%g` reproduces it byte for byte.
//! 6. **CONCENTRATION WINDOW** — correct as built (`build_states_v2`: the last 2000 trades); the
//!    apparent mismatch was 2's band changing which trades populate it.
//! 7. **`round(x, nd)` — REAL, and the one that survived every earlier test.** The ledger's
//!    ties-to-even rounding was `(x * 10^nd).round_ties_even() / 10^nd`, which rounds an
//!    ALREADY-rounded product: `3.865 * 100` is exactly `386.5`, so it read as a tie and went to
//!    even — `3.86` — while CPython's `round(3.865, 2)` is `3.87`, because the exact value of that
//!    double is above the tie. One real prompt line (`last_trade_age_s`), fixed in
//!    `fmt::round_half_even_nd` by carrying the multiplication's exact residual (`mul_add` + a
//!    two-sum comparison); the duplicated implementation in `bundle_assemble::py_round` now
//!    delegates to it.
//!
//! Test-side placeholders (NOT defects, and excluded from the verdict): the fixture supplies
//! `CurveState::Absent` / `AmmState::Absent` and a zeroed `FlowState`, so the CURVE STATE / AMM
//! POOL STATE / PRICE UNITS / LIVE FLOW STATE lines are not compared — the curve/AMM annotation has
//!    no live producer yet (C3), and although `FlowReducer` serves the flow features, nothing has
//!    wired it into the assembler. `curve_present` used to be collateral of the curve placeholder;
//!    since finding 3 it is not.
//!
//! THE DEV HISTORY LINE **IS** GRADED, from the C4 producer (`creator_history::CreatorHistory`),
//! whose launch table the fixture carries: 12/12 rows, including one creator with 8 launches whose
//! prior-launch count is 6. Two dialects of that line exist in the corpus and only one belongs to
//! this family — `creator_known=1` (decision rows, `build_c9.py`) vs `creator_known=True …
//! bundle_wallets=… wash_ratio=…` (utility/management rows, `inject_enrichment.py`). The fixture
//! takes the DECISION family's row for each (mint, t_dec), because that is the family the live
//! entry path renders; do not "unify" the renderer on the other dialect without checking which
//! family is being served.
//!
//! MCAP IS AN INPUT, and that is stated rather than hidden: trades cannot see supply, so
//! `mcap_sol_at_t`/`mcap_source` are taken from the row's own rendered line and fed in. Every other
//! field on the ENRICHED line is DERIVED here from the tape.

use pump_quant_app::bundle_assemble::{assemble, BundleInputs};
use pump_quant_app::creator_history::CreatorHistory;
use pump_quant_app::curve_annotation::{
    AmmAttribution, AmmObservation, AnnotationState, CurveObservation,
};
use pump_quant_app::enrichment::{enrich, EnrichmentTrade};
use pump_quant_app::flow_feed::{flow_state_from_aggregates, zero_flow_state};
use pump_quant_app::state_ledger::{StateLedger, StateTrade, VenueLabel};
use pump_quant_market_state::flow_reducer::FlowAggregates;
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

/// Why a reading produced no comparison.
#[derive(Debug)]
enum Unserved {
    Refused(String),
    Ineligible(String),
}

/// The `LIVE FLOW STATE` block driven through its producer: the reducer's integer carries are
/// rebuilt from the fixture's corpus doubles, `flow_state_from_aggregates` maps them, and the
/// bundle renders the result — the same round trip `flow_state_parity` grades on its own.
fn flow_for_case(case: &Value) -> (FlowState, bool) {
    let block = &case["flow_state"];
    let Some(inputs) = block["inputs"].as_object() else {
        return (zero_flow_state(), false);
    };
    if inputs.get("no_prior_flow").and_then(Value::as_bool) == Some(true) {
        return (zero_flow_state(), true);
    }
    let ints = &inputs["ints"];
    let floats = &inputs["floats"];
    let micro = |k: &str| -> Option<u32> { floats[k].as_f64().map(|v| (v * 1e6).round() as u32) };
    let agg = FlowAggregates {
        entrants_60s: ints["entrants_60s"].as_u64().expect("entrants_60s") as u32,
        entrants_300s: ints["entrants_300s"].as_u64().expect("entrants_300s") as u32,
        net_flow_sol_300s_micro: (floats["net_flow_sol_300s"].as_f64().expect("net_flow") * 1e6)
            .round() as i64,
        fresh_wallet_share_300s_micro: micro("fresh_wallet_share_300s"),
        flow_lookback_d_tenths: (floats["flow_lookback_d"].as_f64().expect("lookback") * 10.0)
            .round() as u32,
        sniper_share_300s_micro: micro("sniper_share_300s"),
        bot_uniform_share_300s_micro: micro("bot_uniform_share_300s"),
        smart_entrants_300s: ints["smart_entrants_300s"]
            .as_u64()
            .expect("smart_entrants") as u32,
        smart_net_flow_sol_300s_micro: (floats["smart_net_flow_sol_300s"]
            .as_f64()
            .expect("smart_net_flow")
            * 1e6)
            .round() as i64,
        coentry_wallets_300s: ints["coentry_wallets_300s"].as_u64().expect("coentry") as u32,
        creator_trading_own_mint: inputs["creator_trading_own_mint"]
            .as_bool()
            .expect("creator_trading_own_mint"),
        entrant_fee_p90_lamports: inputs["entrant_fee_p90_lamports"].as_u64(),
        entrant_cu_p50: inputs["entrant_cu_p50"].as_u64(),
    };
    (flow_state_from_aggregates(&agg), false)
}

/// The reserve plane driven through its producer: one observation per plane, through
/// `AnnotationState::{observe_curve, set_attribution, observe_amm}` and then
/// `{curve_state, amm_state}` — never a `CurveState`/`AmmState` lifted from the corpus's text.
///
/// The fixture carries the ATTRIBUTION facts for the absent-branch rows (`never_graduated`), so the
/// refusal is produced by the same rule the corpus applied rather than hard-coded here.
fn curve_and_amm_for_case(case: &Value, mint: &[u8; 32], t_dec: i64) -> (CurveState, AmmState) {
    let mut st = AnnotationState::new();
    if let Some(obs) = case["curve_line"]["input"].as_object() {
        st.observe_curve(
            *mint,
            CurveObservation {
                v_sol_lamports: obs["v_sol_lamports"].as_u64().expect("v_sol_lamports"),
                v_tokens: obs["v_tokens"].as_u64().expect("v_tokens"),
                real_sol_lamports: obs["real_sol_lamports"]
                    .as_u64()
                    .expect("real_sol_lamports"),
                real_tokens: obs["real_tokens"].as_u64().expect("real_tokens"),
                ts_ms: obs["ts_ms"].as_i64().expect("ts_ms"),
                slot: 0,
            },
        );
    }
    let attr = &case["amm_line"]["input"]["attribution"];
    st.set_attribution(
        *mint,
        AmmAttribution {
            wsol_pools: attr["wsol_pools"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .map(|p| p.as_str().expect("pool").to_string())
                        .collect()
                })
                .unwrap_or_default(),
            pools_total: attr["pools_total"].as_u64().unwrap_or(0) as usize,
            graduated: attr["graduated"].as_bool().unwrap_or(false),
        },
    );
    if let Some(obs) = case["amm_line"]["input"]["obs"].as_object() {
        st.observe_amm(
            *mint,
            AmmObservation {
                pool: obs["pool"].as_str().expect("pool").to_string(),
                base_reserves_raw: obs["base_reserves_raw"].as_u64().expect("base"),
                quote_reserves_lamports: obs["quote_reserves_lamports"].as_u64().expect("quote"),
                quote_is_wsol: obs["quote_is_wsol"].as_bool().unwrap_or(true),
                ts_ms: obs["ts_ms"].as_i64().expect("ts_ms"),
                slot: obs["slot"].as_u64().expect("slot"),
            },
        );
    }
    (st.curve_state(mint, t_dec), st.amm_state(mint, t_dec))
}

/// Assemble and render one reading of a case.
///
/// `band` is the corpus's whole-run band median when the reading is the corpus-side one: a trade
/// whose price falls outside `[median/10, median*10]` is dropped BEFORE ingest, reproducing the
/// corpus's own trade set. `None` is the causal reading — every trade the tape carries.
///
/// `dev` is the C4 producer's verdict for this mint ([`CreatorHistory::dev_history`]) — the
/// `DEV HISTORY:` inputs, which come from the launch table rather than from trades, so they are the
/// same in both readings.
fn render_reading(
    case: &Value,
    band: Option<f64>,
    dev: DevHistoryDecision,
) -> Result<String, Unserved> {
    let mint = mint_key(case["mint"].as_str().expect("mint"));
    let t_dec = case["t_dec_ms"].as_i64().expect("t_dec_ms");
    let trades = case["trades"].as_array().expect("trades");

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

        // THE CORPUS'S BAND, applied only when this reading is the corpus-side one. It is a filter
        // on the INPUT, because that is exactly what it was upstream of the corpus's builder. A
        // trade with no price cannot be judged by a price band; the ledger refuses it anyway.
        if let (Some(med), Some(p)) = (band, price) {
            if p < med / 10.0 || p > med * 10.0 {
                continue;
            }
        }

        ledger.on_trade(
            &mint,
            StateTrade {
                recv_unix_ms: recv,
                price_lamports_per_raw_token: price,
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
    // ledger refuses is not a parity failure — it is the ledger correctly declining.
    if let Err(refusal) = ledger.eligibility(&mint, t_dec) {
        return Err(Unserved::Ineligible(format!("ledger/{refusal:?}")));
    }
    let Some(snapshot) = ledger.serve(&mint, t_dec) else {
        return Err(Unserved::Ineligible("ledger/serve_none".to_string()));
    };
    let snapped =
        enrich(&enr, t_dec).map_err(|e| Unserved::Ineligible(format!("enrichment/{e:?}")))?;

    let expected_enriched = case["expected_enriched_line"]
        .as_str()
        .expect("enriched line");
    let (mcap, mcap_source) = mcap_from_line(expected_enriched);

    // THE THREE PLANE PRODUCERS, driven from the fixture's own INPUTS. The placeholder
    // `CurveState::Absent` / zeroed `FlowState` that used to stand here masked exactly the lines
    // this harness now grades; a case whose inputs are not recoverable carries `null` inputs and
    // the corresponding line is never compared (it is counted instead).
    let (flow, flow_no_prior) = flow_for_case(case);
    let (curve, amm) = curve_and_amm_for_case(case, &mint, t_dec);
    let bundle = assemble(&BundleInputs {
        snapshot: &snapshot,
        enriched: &snapped,
        t_dec_ms: t_dec,
        venue: &snapshot.venue,
        mcap_sol_at_t: mcap,
        mcap_source: &mcap_source,
        flow: &flow,
        flow_no_prior,
        curve,
        amm,
        dev,
        size_depth_sol: None,
        size_amm: false,
    })
    .map_err(|e| Unserved::Refused(e.as_str().to_string()))?;

    Ok(render_decision(&bundle))
}

/// The four reserve/flow PLANE lines the harness grades end-to-end (fixture block, line header).
const PLANE_LINES: [(&str, &str); 4] = [
    ("curve_line", "CURVE STATE"),
    ("amm_line", "AMM POOL STATE"),
    ("price_units", "PRICE UNITS"),
    ("flow_state", "LIVE FLOW STATE"),
];

/// The key that identifies a line's slot in the rendering: state lines are keyed by their first
/// field name, the block headers by their own text.
fn line_key(want: &str) -> &str {
    for header in [
        "ENRICHED CANDIDATE STATE",
        "CURVE STATE",
        "AMM POOL STATE",
        "PRICE UNITS",
        "LIVE FLOW STATE",
    ] {
        if want.starts_with(header) {
            return header;
        }
    }
    want.split('=').next().unwrap_or(want)
}

/// The reserve/flow PLANE lines: built from observations and aggregates, never from the trade
/// ledger, so the corpus's lookahead band cannot move them and the two readings must agree.
fn is_plane_line(want: &str) -> bool {
    want.starts_with("CURVE STATE")
        || want.starts_with("AMM POOL STATE")
        || want.starts_with("LIVE FLOW STATE")
}

/// `PRICE UNITS` is derived from the ledger's own price (it is the same measurement the state line
/// carries), so it belongs to the banded/ledger side, not to the plane side.
fn is_price_line(want: &str) -> bool {
    want.starts_with("PRICE UNITS")
}

/// The six STATE lines of the decision prompt — the block whose producer (`build_states_v2`) bands
/// its tape. Every other graded line (ENRICHED, DEV HISTORY) comes from a producer that does not.
fn is_state_line(want: &str) -> bool {
    [
        "t_dec_ms=",
        "venue=",
        "n_prior_trades=",
        "price_lamports_per_raw_token=",
        "buy_volume_lamports=",
        "top1_trader_share=",
    ]
    .iter()
    .any(|p| want.starts_with(p))
}

fn find_key<'a>(rendered: &'a str, key: &str) -> Option<&'a str> {
    rendered.lines().find(|l| l.starts_with(key))
}

/// The creator launch registry, built from the fixture's own launch table (the corpus's
/// `launches.jsonl` rows for the creators of these mints). `by_creator` is not shipped: the
/// registry derives each creator's sorted launch times from the launches themselves, exactly as
/// the corpus's `prior_launches` bisect does.
fn creator_history(f: &Value) -> CreatorHistory {
    let mut history = CreatorHistory::new();
    for row in f["launches"].as_array().expect("launches") {
        let mint = row[0].as_str().expect("mint");
        let creator = row[1].as_str().expect("creator");
        let launch_ms = row[2].as_i64().expect("launch_unix_ms");
        assert!(
            history.observe(mint_key(mint), creator_id(creator), launch_ms),
            "the fixture's launch table is bounded by MAX_TRACKED_LAUNCHES"
        );
    }
    history
}

/// A stable per-creator id: any injective map works, the creator address is only a key.
fn creator_id(base58: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in base58.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// THE HARD RULE, asserted rather than assumed: a graded plane line must carry the INPUTS the
/// producer under test is driven from and the AUTHORITY's rendering of those inputs. A fixture
/// that merely copied the corpus's text into `expected` would fail this test — which is the whole
/// reason `gen_c5_bundle_fixture.py` reconstructs each plane's inputs and re-renders it.
#[test]
fn every_graded_plane_line_carries_its_inputs_and_the_authoritys_rendering() {
    let f = fixture();
    assert_eq!(
        f["schema"].as_str(),
        Some("c5-bundle-parity/3"),
        "the plane lines arrived with schema /3"
    );
    let mut graded = 0usize;
    for case in f["cases"].as_array().expect("cases") {
        for (key, name) in PLANE_LINES {
            let b = &case[key];
            if b["graded"].as_bool() != Some(true) {
                continue;
            }
            graded += 1;
            assert!(
                b["producer"].as_str().is_some_and(|p| !p.is_empty()),
                "{name}: a graded line names no producer"
            );
            let expected = b["expected_line"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: a graded line carries no expected_line"));
            // The authority's rendering over the recorded inputs must BE the row's own line —
            // otherwise the inputs we recovered are not the ones the corpus used.
            assert_eq!(
                Some(expected),
                b["corpus_line"].as_str(),
                "{name}: the authority's rendering of the recorded inputs is not the row's line"
            );
            let has_input = match key {
                "curve_line" => b["input"].is_object(),
                "amm_line" => b["input"]["attribution"].is_object(),
                "price_units" => b["input_price_lamports_per_raw_token"].is_number(),
                "flow_state" => b["inputs"].is_object(),
                _ => false,
            };
            assert!(
                has_input,
                "{name}: a graded line carries no INPUTS — a fixture without inputs cannot catch a \
                 wrong renderer"
            );
        }
    }
    assert!(
        graded >= 46,
        "the four plane lines must be graded on the real rows (got {graded} graded line-instances)"
    );
}

#[test]
fn derived_blocks_render_identically_to_the_corpus_over_the_corpus_tape() {
    let f = fixture();
    let cases = f["cases"].as_array().expect("cases");
    assert!(
        cases.len() >= 8,
        "the fixture must be broad enough to mean something: {}",
        cases.len()
    );

    let mut graded = 0usize;
    let mut skipped = 0usize;
    let mut band_cases = 0usize;
    let mut band_explained_lines = 0usize;
    // WHY a case was skipped, as a histogram: a fixture where every row is refused is not a passing
    // test, it is a mis-wired ingest, and the cause is what says which.
    let mut skip_causes: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut corpus_failures: Vec<String> = Vec::new();
    let mut causal_failures: Vec<String> = Vec::new();
    let mut plane_failures: Vec<String> = Vec::new();
    // Per-line graded/skipped counts for the four plane lines, so a silent skip cannot hide.
    let mut plane_counts: std::collections::BTreeMap<&str, (usize, usize)> = PLANE_LINES
        .iter()
        .map(|(_, n)| (*n, (0usize, 0usize)))
        .collect();
    let mut plane_skip_causes: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    let history = creator_history(&f);
    for (i, case) in cases.iter().enumerate() {
        let mint = mint_key(case["mint"].as_str().expect("mint"));
        // The C4 producer's verdict for this mint, from the launch table (not from trades).
        let dev = history.dev_history(&mint);
        let band = case["band"]["median_lamports_per_raw_token"].as_f64();
        let band_applies = case["band"]["applies"].as_bool().unwrap_or(false);
        let banded_out = case["band"]["prefix_trades_banded_out"]
            .as_u64()
            .unwrap_or(0);

        let causal = match render_reading(case, None, dev) {
            Ok(r) => r,
            Err(u) => {
                skipped += 1;
                let why = match u {
                    Unserved::Refused(r) => format!("assembler/{r}"),
                    Unserved::Ineligible(r) => r,
                };
                *skip_causes.entry(why.clone()).or_default() += 1;
                // The plane lines are not graded for a case that never assembled: count them so
                // the per-line tally stays complete.
                for (_, name) in PLANE_LINES {
                    plane_counts.get_mut(name).expect("known plane line").1 += 1;
                    *plane_skip_causes
                        .entry(format!("{name}: case_not_assembled/{why}"))
                        .or_default() += 1;
                }
                continue;
            }
        };
        // The corpus-side reading uses the band only where the corpus applied it (it needs >= 5
        // finite prices); below that the corpus's trade set IS the causal one.
        let corpus_side = if band_applies {
            band_cases += 1;
            match render_reading(case, band, dev) {
                Ok(r) => r,
                Err(u) => {
                    corpus_failures.push(format!(
                        "case {i}: the corpus-side reading could not be assembled: {u:?} \
                         (banded out {banded_out} prefix trades)"
                    ));
                    continue;
                }
            }
        } else {
            causal.clone()
        };

        let mut expected: Vec<&str> = case["expected_state_lines"]
            .as_array()
            .expect("expected_state_lines")
            .iter()
            .map(|l| l.as_str().expect("line"))
            .collect();
        expected.push(
            case["expected_enriched_line"]
                .as_str()
                .expect("enriched line"),
        );
        expected.push(
            case["expected_dev_line"]
                .as_str()
                .expect("expected_dev_line — the C4 producer's line"),
        );
        // THE FOUR RESERVE/FLOW PLANE LINES. Each is appended only when the fixture carries the
        // AUTHORITY's own rendering of the SAME inputs the producers above were driven from — a
        // line the row does not carry (or whose inputs are not recoverable) is counted, never
        // graded from a copy of the corpus's own text.
        for (key, name) in PLANE_LINES {
            let block = &case[key];
            if block["graded"].as_bool() == Some(true) {
                plane_counts.get_mut(name).expect("known plane line").0 += 1;
                expected.push(
                    block["expected_line"]
                        .as_str()
                        .expect("a graded plane line carries its expected_line"),
                );
            } else {
                let why = block["skip_reason"].as_str().unwrap_or("unspecified");
                plane_counts.get_mut(name).expect("known plane line").1 += 1;
                *plane_skip_causes
                    .entry(format!("{name}: {why}"))
                    .or_default() += 1;
            }
        }

        let mut case_failures = 0usize;
        for want in expected {
            let key = line_key(want);
            // TWO PRODUCERS, TWO TRADE SETS. The corpus's STATE block comes from
            // `build_states_v2` (which bands) while the ENRICHED and DEV HISTORY lines come from
            // `build_c9_enrichment_full` (which does NOT — it reads the tape prefix directly, and the
            // dev line reads the launch table). The prompt is the join, so each block is graded
            // against the reading that reproduces its own producer. The reserve/flow plane lines
            // are ledger-independent (the band cannot move them) but are graded on the same side as
            // the STATE block for one uniform rule; `PRICE UNITS` is ledger-derived, so it must be.
            let causal_line = find_key(&causal, key);
            let corpus_line = find_key(&corpus_side, key);
            let graded_line = if is_state_line(want) || is_price_line(want) || is_plane_line(want) {
                corpus_line
            } else {
                causal_line
            };
            if graded_line != Some(want) {
                case_failures += 1;
                corpus_failures.push(format!(
                    "case {i}: line not reproduced by its own producer's trade set\n   \
                     corpus: {want}\n   ours  : {}",
                    graded_line.unwrap_or("<no line with that key>")
                ));
                continue;
            }
            // A plane line is built from observations and aggregates, not from the ledger: if the
            // band moved it, something downstream is reading trades it should not.
            if is_plane_line(want) && causal_line != corpus_line {
                case_failures += 1;
                plane_failures.push(format!(
                    "case {i}: a reserve/flow plane line moved with the trade band — the band must \
                     not touch CURVE / AMM / LIVE FLOW\n   corpus-side: {want}\n   causal     : {}",
                    causal_line.unwrap_or("<no line with that key>")
                ));
                continue;
            }
            // The causal reading may differ on a STATE line — but ONLY where the band itself moves
            // that line. If the banded and causal readings agree and the corpus does not, the
            // divergence has another cause and this test must fail on it.
            if is_state_line(want) && causal_line != Some(want) {
                if causal_line == corpus_line {
                    case_failures += 1;
                    causal_failures.push(format!(
                        "case {i}: the causal reading diverges and the band does NOT explain it\n   \
                         corpus: {want}\n   causal: {}",
                        causal_line.unwrap_or("<no line with that key>")
                    ));
                } else {
                    band_explained_lines += 1;
                }
            }
        }
        if case_failures == 0 {
            graded += 1;
        }
    }

    assert!(
        corpus_failures.is_empty(),
        "{} line(s) diverged from the corpus over the corpus's OWN trade set:\n\n{}",
        corpus_failures.len(),
        corpus_failures.join("\n\n")
    );
    assert!(
        causal_failures.is_empty(),
        "{} causal divergence(s) NOT explained by the corpus's lookahead band:\n\n{}",
        causal_failures.len(),
        causal_failures.join("\n\n")
    );
    assert!(plane_failures.is_empty(), "{plane_failures:?}",);
    assert!(
        graded >= 8,
        "only {graded} case(s) reached a graded comparison (skipped {skipped}: {skip_causes:?}); \
         the fixture must exercise the derivation, not the refusals"
    );
    assert!(
        band_cases > 0 && band_explained_lines > 0,
        "the fixture must exercise the band ({band_cases} banded case(s), \
         {band_explained_lines} band-explained line(s))"
    );
    // THE PLANE LINES ARE THE POINT OF THIS TEST: each must actually be graded on the real rows.
    // A floor per line (not an equality) keeps the harness honest without pinning the fixture count.
    let plane_floor = graded.saturating_sub(1).max(1); // one row may be an unrecoverable line
    for (name, (n_graded, n_skipped)) in &plane_counts {
        println!(
            "C5 plane {name}: graded {n_graded}/{} (skipped {n_skipped})",
            cases.len()
        );
        assert!(
            *n_graded >= plane_floor,
            "{name}: only {n_graded} of {} real rows graded — the harness must exercise this line \
             (skips: {plane_skip_causes:?})",
            cases.len()
        );
    }
    assert!(
        !plane_skip_causes.is_empty(),
        "the fixture must exercise at least one out-of-scope line so the skip path is proven"
    );
    println!(
        "C5: {graded}/{} case(s) byte-identical given the corpus's trade set; \
         {band_cases} carry the band; {band_explained_lines} causal line(s) attributed to it; \
         plane skips: {plane_skip_causes:?}",
        cases.len()
    );
}
