//! Event-stream codec v2: every production variant round-trips writer -> file -> checked reader with
//! ALL fields intact; v1 legacy lines and malformed critical lines produce NAMED incompleteness.
use pump_quant_app::event::{AppEvent, CreatorActionKind, FeatureBasis, TradeVenue};
use pump_quant_domain::ids::Mint;
use pump_quant_junction::event_codec::{decode, encode, is_critical, KINDS};
use pump_quant_junction::event_stream::{
    read_event_stream, read_event_stream_checked, EventStreamWriter,
};
use pump_quant_protocol::pumpswap_event::CashbackField;

const M: Mint = Mint([7; 32]);
const K: [u8; 32] = [9; 32];
const SIG: [u8; 64] = [0xAB; 64];

fn feat() -> FeatureBasis {
    FeatureBasis {
        sol_lamports: -1_644_733,
        tokens_raw: 4_324_453,
        trader: [3; 32],
    }
}

/// One fully-populated instance of EVERY variant (Some for every Option), plus None-variants.
fn all_events() -> Vec<AppEvent> {
    vec![
        AppEvent::MarketTrade {
            mint: M,
            price_fp: -170_141_183_460_469_231_731_687_303_715_884_105_727,
            quote_lamports: u64::MAX,
            liquidity_lamports: 5,
            signed_base: i64::MIN,
            buyer_entity: 42,
            age_slots: u32::MAX,
            trader_pubkey: Some(K),
            recv_unix_ms: Some(1_788_965_347_168),
            slot: Some(445_637_627),
            fee_lamports: Some(5_000),
            cu_consumed: Some(80_000),
            venue: Some(TradeVenue::PumpSwap),
            event_id: Some(u128::MAX - 1),
            feature: Some(feat()),
        },
        AppEvent::MarketTrade {
            mint: M,
            price_fp: 0,
            quote_lamports: 0,
            liquidity_lamports: 0,
            signed_base: 0,
            buyer_entity: 0,
            age_slots: 0,
            trader_pubkey: None,
            recv_unix_ms: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        },
        AppEvent::CorpusFlowRow {
            mint: M,
            venue: TradeVenue::PumpFun,
            feature: feat(),
            recv_unix_ms: Some(1),
            slot: Some(2),
            fee_lamports: Some(3),
            cu_consumed: Some(4),
            event_id: 0xcbdb_e071,
        },
        AppEvent::NarrativeSample {
            mint: M,
            prior_active: 1,
            new_mentions: 2,
        },
        AppEvent::SocialCall {
            mint: M,
            source_quality_bp: 7_500,
        },
        AppEvent::WalletAction {
            mint: M,
            followable: true,
            size_lamports: 9,
        },
        AppEvent::OnchainConfirm {
            mint: M,
            virtual_sol_lamports: 30,
            real_sol_lamports: 1,
        },
        AppEvent::CurveObserved {
            mint: M,
            v_sol_lamports: 30_000_000_000,
            v_tokens: 1_073_000_000_000_000,
            real_sol_lamports: 0,
            real_tokens: 793_100_000_000_000,
            recv_unix_ms: Some(-5),
            slot: 9,
        },
        AppEvent::CurveModeObserved {
            mint: M,
            mayhem: true,
            slot: 10,
        },
        AppEvent::AmmSwap {
            mint: M,
            pool: [4; 32],
            pool_is_canonical: true,
            quote_is_wsol: false,
            token_reserve_pre: 1,
            quote_reserve_pre: 2,
            fee_bps: Some(125),
            fee_parts: Some((2, 93, 30)),
            virtual_quote: Some(17_584_505_661),
            cashback: CashbackField::Known {
                bps: 30,
                lamports: 2_831,
                layout_len: 433,
            },
            is_buy: true,
            token_amount: 3,
            quote_lamports: 4,
            trader: K,
            fee_lamports: Some(5),
            cu_consumed: Some(6),
            recv_unix_ms: Some(7),
            slot: 8,
        },
        AppEvent::AmmSwap {
            mint: M,
            pool: [4; 32],
            pool_is_canonical: false,
            quote_is_wsol: true,
            token_reserve_pre: 1,
            quote_reserve_pre: 2,
            fee_bps: None,
            fee_parts: None,
            virtual_quote: None,
            cashback: CashbackField::Missing { layout_len: 352 },
            is_buy: false,
            token_amount: 3,
            quote_lamports: 4,
            trader: K,
            fee_lamports: None,
            cu_consumed: None,
            recv_unix_ms: None,
            slot: 8,
        },
        AppEvent::LaunchObserved {
            mint: M,
            creator: K,
            launch_unix_ms: 11,
        },
        AppEvent::LaunchFromChain {
            mint: M,
            creator: K,
            slot: 12,
            block_time_s: Some(13),
            retrieved_unix_ms: 14,
        },
        AppEvent::LaunchFromChain {
            mint: M,
            creator: K,
            slot: 12,
            block_time_s: None,
            retrieved_unix_ms: 14,
        },
        AppEvent::TokenMetadata {
            mint: M,
            category_id: 3,
            taxonomy_version: 2,
            creator: 99,
            slot: 15,
        },
    ]
}

fn all_events_2() -> Vec<AppEvent> {
    vec![
        AppEvent::CreatorAction {
            mint: M,
            kind: CreatorActionKind::Init {
                initial_tokens: 1,
                total_supply: 2,
            },
            slot: 1,
        },
        AppEvent::CreatorAction {
            mint: M,
            kind: CreatorActionKind::Buy {
                tokens: 1,
                quote_lamports: 2,
            },
            slot: 1,
        },
        AppEvent::CreatorAction {
            mint: M,
            kind: CreatorActionKind::Sell {
                tokens: 1,
                quote_lamports: 2,
            },
            slot: 1,
        },
        AppEvent::CreatorAction {
            mint: M,
            kind: CreatorActionKind::LinkedBuy {
                cluster: 5,
                tokens: 2,
            },
            slot: 1,
        },
        AppEvent::Migration { mint: M, slot: 16 },
        AppEvent::MarketAuxiliary {
            mint: M,
            token_standard: 1,
            symbol_len: 4,
        },
        AppEvent::NarrativeResolved {
            mint: M,
            verdict: 1,
            stage: 2,
            family: 3,
            lexicon_version: 4,
        },
        AppEvent::TimeSignal {
            dow: 3,
            hour_utc: 23,
        },
        AppEvent::ModelOrderEvidence {
            mint: M,
            order_id: 1,
            attempt: 2,
            clip_lamports: 3,
            filled: Some((4, 5)),
        },
        AppEvent::ModelOrderEvidence {
            mint: M,
            order_id: 1,
            attempt: 2,
            clip_lamports: 3,
            filled: None,
        },
        AppEvent::ModelMgmtReport {
            mint: M,
            order_id: 1,
            action: 2,
            intended: 3,
            cumulative_tokens: 4,
            cumulative_gross: 5,
            cumulative_fees: 6,
            terminal: true,
        },
        AppEvent::OurBuyConfirmed {
            mint: M,
            signature: SIG,
            slot: 1,
        },
        AppEvent::OurBuyFailed {
            mint: M,
            signature: SIG,
            err_code: 7,
            slot: 1,
        },
        AppEvent::OurSellConfirmed {
            mint: M,
            signature: SIG,
            slot: 1,
        },
        AppEvent::OurSellFailed {
            mint: M,
            signature: SIG,
            err_code: 7,
            slot: 1,
        },
        AppEvent::Tick,
    ]
}

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("codec_{}_{name}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn every_variant_round_trips_with_every_field_through_file_and_checked_reader() {
    let evs: Vec<AppEvent> = all_events().into_iter().chain(all_events_2()).collect();
    let kinds: std::collections::BTreeSet<&str> = evs
        .iter()
        .map(|e| {
            let l = encode(e, 0);
            let (k, _) = decode(&l).unwrap();
            KINDS.iter().find(|x| **x == k).copied().unwrap()
        })
        .collect();
    assert_eq!(kinds.len(), KINDS.len(), "every kind exercised");
    let p = tmp("all");
    let mut w = EventStreamWriter::open(&p).unwrap();
    for (i, e) in evs.iter().enumerate() {
        w.write_event(e, i as u64).unwrap();
    }
    w.flush().unwrap();
    drop(w);
    let c = read_event_stream_checked(&p).unwrap();
    assert_eq!(c.events, evs, "byte-for-byte field equality, in order");
    assert!(c.is_complete(), "{:?}", c.incomplete());
    let total: u64 = c.by_kind.values().map(|k| k.written).sum();
    assert_eq!(total as usize, evs.len());
    assert!(c
        .by_kind
        .values()
        .all(|k| k.written == k.parsed && k.rejected == 0 && k.lossy_v1 == 0));
    let (back, skipped) = read_event_stream(&p).unwrap();
    assert_eq!((back.len(), skipped), (evs.len(), 0));
}

#[test]
fn legacy_v1_and_malformed_critical_lines_are_named_incompleteness_not_success() {
    let p = tmp("legacy");
    // Real v1 lines from the bF1 capture shape: an AmmSwap (no v1 reader arm), a v1 MarketTrade that the
    // legacy reader accepts but whose encoding dropped slot/fee/CU/venue/feature (lossy), and a Tick.
    let v1_amm = r#"{"slot":0,"kind":"AmmSwap","mint":"FoaKMeybrT7UZs6jxfdo38ZwTzF1tpaB4pNv8QRpump","fields":{"token_reserve_pre":1,"quote_reserve_pre":2,"is_buy":true,"token_amount":3,"quote_lamports":4,"slot":5}}"#;
    let v1_trade = r#"{"slot":0,"kind":"MarketTrade","mint":"d4sZJCvJytmsS6AMScxfg9SShuPAvhXCkEo7gGDXoxQ","fields":{"price_fp":0,"quote_lamports":0,"liquidity_lamports":0,"signed_base":4324453,"buyer_entity":4621333875301830244,"age_slots":0,"recv_unix_ms":1788965347168}}"#;
    let v1_tick = r#"{"slot":0,"kind":"Tick"}"#;
    // A v2 line with a required field removed, and a v2 line of an unknown kind.
    let good = encode(&AppEvent::Migration { mint: M, slot: 1 }, 0);
    let broken = good.replace(r#""slot":1"#, r#""slotx":1"#);
    let unknown = good.replace("Migration", "FutureKind");
    std::fs::write(
        &p,
        [v1_amm, v1_trade, v1_tick, &broken, &unknown, &good].join("\n"),
    )
    .unwrap();
    let c = read_event_stream_checked(&p).unwrap();
    assert!(!c.is_complete());
    let inc = c.incomplete().join(" ");
    assert!(inc.contains("AmmSwap:rejected=1"), "{inc}");
    assert!(inc.contains("MarketTrade:rejected=0,lossy_v1=1"), "{inc}");
    assert!(inc.contains("Migration:rejected=1"), "{inc}");
    assert!(
        inc.contains("FutureKind:rejected=1"),
        "unknown kinds are critical by default: {inc}"
    );
    assert!(
        !inc.contains("Tick"),
        "a legacy Tick is not a critical loss"
    );
    assert_eq!(c.by_kind["Migration"].parsed, 1);
    assert!(
        c.reasons["Migration"]
            .keys()
            .any(|r| r.contains("field slot")),
        "{:?}",
        c.reasons
    );
    let (_, skipped) = read_event_stream(&p).unwrap();
    assert_eq!(skipped, 4, "rejected (3) + critical lossy v1 (1)");
    assert!(is_critical("AmmSwap") && is_critical("LaunchFromChain") && !is_critical("Tick"));
}

#[test]
fn wide_integers_are_lossless_and_bad_values_are_rejected_by_field() {
    let e = all_events()[0];
    let l = encode(&e, 0);
    assert!(
        l.contains(r#""event_id":"fffffffffffffffffffffffffffffffe""#),
        "{l}"
    );
    assert!(
        l.contains(r#""price_fp":"-170141183460469231731687303715884105727""#),
        "{l}"
    );
    assert_eq!(decode(&l).unwrap().1, e);
    let neg = l.replace(
        r#""quote_lamports":18446744073709551615"#,
        r#""quote_lamports":-1"#,
    );
    assert!(decode(&neg).unwrap_err().contains("quote_lamports"));
    let big = encode(
        &AppEvent::TimeSignal {
            dow: 1,
            hour_utc: 2,
        },
        0,
    )
    .replace(r#""dow":1"#, r#""dow":300"#);
    assert!(decode(&big).unwrap_err().contains("dow"));
    // Schema v3 is current (cashback); a FUTURE version is still rejected by name.
    let badver = l.replacen(r#""v":3"#, r#""v":4"#, 1);
    assert!(decode(&badver).unwrap_err().contains("version 4"));
}

// ---------------------------------------------------------------------------------------------------------
// Schema v3: AmmSwap carries the cashback pair with its layout provenance. Old (v2) data still reads, with the
// gap NAMED; every cashback state survives the round trip distinctly.
// ---------------------------------------------------------------------------------------------------------

fn amm(cashback: CashbackField) -> AppEvent {
    let mut e = all_events()
        .into_iter()
        .find(|e| matches!(e, AppEvent::AmmSwap { .. }))
        .unwrap();
    if let AppEvent::AmmSwap { cashback: c, .. } = &mut e {
        *c = cashback;
    }
    e
}

const STATES: [CashbackField; 5] = [
    CashbackField::Known {
        bps: 30,
        lamports: 2_831,
        layout_len: 433,
    },
    CashbackField::Known {
        bps: 0,
        lamports: 0,
        layout_len: 481,
    },
    CashbackField::Missing { layout_len: 352 },
    CashbackField::Unsupported { layout_len: 440 },
    CashbackField::NotRecorded,
];

#[test]
fn v3_round_trips_every_cashback_state_distinctly_with_provenance() {
    let mut lines = Vec::new();
    for c in STATES {
        let e = amm(c);
        let l = encode(&e, 0);
        assert!(l.contains(r#""v":3"#), "{l}");
        assert!(l.contains(&format!(r#""state":"{}""#, c.state())), "{l}");
        let back = decode(&l).unwrap().1;
        assert_eq!(back, e);
        lines.push(l);
    }
    assert!(lines[0]
        .contains(r#""cashback":{"bps":30,"lamports":2831,"layout_len":433,"state":"known"}"#));
    assert!(
        lines[1].contains(r#""cashback":{"bps":0,"lamports":0,"layout_len":481,"state":"known"}"#)
    );
    assert!(lines[2].contains(r#""cashback":{"layout_len":352,"state":"missing"}"#));
    let uniq: std::collections::BTreeSet<&String> = lines.iter().collect();
    assert_eq!(uniq.len(), STATES.len(), "five states, five encodings");
    let p = tmp("v3cb");
    std::fs::write(&p, lines.join("\n")).unwrap();
    let c = read_event_stream_checked(&p).unwrap();
    assert!(c.is_complete(), "{:?}", c.incomplete());
    assert!(c.schema_gaps().is_empty());
    assert_eq!(c.versions.get(&3), Some(&5));
}

#[test]
fn v3_amm_swap_without_the_cashback_object_is_rejected_by_name() {
    let l = encode(&amm(STATES[0]), 0);
    let v: serde_json::Value = serde_json::from_str(&l).unwrap();
    let mut v2 = v.clone();
    v2["fields"].as_object_mut().unwrap().remove("cashback");
    let err = decode(&v2.to_string()).unwrap_err();
    assert!(err.contains("field cashback"), "{err}");
    for (k, bad) in [
        ("state", serde_json::json!("guessed")),
        ("bps", serde_json::json!(-1)),
        ("layout_len", serde_json::json!(70_000)),
    ] {
        let mut b = v.clone();
        b["fields"]["cashback"][k] = bad;
        let err = decode(&b.to_string()).unwrap_err();
        assert!(err.contains("cashback"), "{k}: {err}");
    }
}

/// OLD DATA: a real v2 line shape (exactly what a566cb0d's writer emitted: no `cashback` key) still decodes with
/// every v2 field intact; its cashback is NOT_RECORDED (not zero, not missing-layout), and the checked reader
/// names the gap. The legacy `skipped` count does not move (the event is delivered).
#[test]
fn v2_amm_lines_still_read_and_name_the_cashback_gap() {
    let e3 = amm(STATES[0]);
    let mut v: serde_json::Value = serde_json::from_str(&encode(&e3, 0)).unwrap();
    v["v"] = serde_json::json!(2);
    v["fields"].as_object_mut().unwrap().remove("cashback");
    let v2_line = v.to_string();
    let back = decode(&v2_line).unwrap().1;
    assert_eq!(back, amm(CashbackField::NotRecorded));
    // A v2 line may not smuggle a v3 field.
    let mut smuggle = v.clone();
    smuggle["fields"]["cashback"] =
        serde_json::json!({"state": "known", "bps": 0, "lamports": 0, "layout_len": 433});
    assert!(decode(&smuggle.to_string())
        .unwrap_err()
        .contains("not part of schema v2"));
    // A v2 non-AMM line is unchanged by the bump.
    let mig2 =
        encode(&AppEvent::Migration { mint: M, slot: 1 }, 0).replacen(r#""v":3"#, r#""v":2"#, 1);
    assert_eq!(
        decode(&mig2).unwrap().1,
        AppEvent::Migration { mint: M, slot: 1 }
    );
    let p = tmp("v2cb");
    std::fs::write(&p, [v2_line.as_str(), &mig2, &encode(&e3, 0)].join("\n")).unwrap();
    let c = read_event_stream_checked(&p).unwrap();
    assert_eq!(c.events.len(), 3);
    assert_eq!(c.by_kind["AmmSwap"].schema_gap, 1);
    assert_eq!(c.by_kind["Migration"].schema_gap, 0);
    assert_eq!(
        c.schema_gaps(),
        vec!["AmmSwap:cashback_not_recorded(schema<3)=1".to_string()]
    );
    assert!(!c.is_complete());
    assert!(c
        .incomplete()
        .iter()
        .any(|s| s.starts_with("AmmSwap:cashback_not_recorded")));
    assert_eq!(
        (c.versions.get(&2), c.versions.get(&3)),
        (Some(&2), Some(&1))
    );
    let (evs, skipped) = read_event_stream(&p).unwrap();
    assert_eq!(
        (evs.len(), skipped),
        (3, 0),
        "legacy replay input unchanged"
    );
}
