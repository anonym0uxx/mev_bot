//! DAEMON-BOUNDARY test: a daemon-facing NDJSON line -> `parse_ndjson_line` -> `classify_pump_instructions`
//! -> `instructions_to_events_with_meta` (the exact calls `pq_daemon` makes per transaction) ->
//! `Engine::tick`. The three lines are REAL on-chain transactions (public RPC, laid out in the line
//! contract), so this proves source decoding + the parser/engine boundary. It does NOT prove the
//! deployed sidecar emits this line: that is a separate Windows capture (see the handoff).
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::model_authority::ModelSource;
use pump_quant_inference::InferenceError;
use pump_quant_junction::laserstream::{
    classify_pump_instructions, instructions_to_events_with_meta, parse_ndjson_line,
    LaserStreamUpdate,
};

struct Skip;
impl ModelSource for Skip {
    fn complete(&self, _: &str, _: &str) -> Result<String, InferenceError> {
        Ok("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: x".into())
    }
}

#[test]
fn real_pumpswap_lines_become_amm_swap_events_and_reach_the_engine() {
    let raw = include_str!("fixtures/pumpswap_rpc_txs.json");
    let v = pq_stream_capture::json::parse(raw).unwrap();
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    e.enable_paper_model(Skip);
    let (mut amm_events, mut canonical, mut priced_halves) = (0, 0, 0);
    for item in v.as_array().unwrap() {
        let line = pq_stream_capture::json::serialize(item.get("line").unwrap());
        let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(&line) else {
            panic!("line must parse");
        };
        // The boundary facts the decoder needs must have SURVIVED parsing.
        assert!(
            tx.fee_lamports.is_some() && tx.cu_consumed.is_some(),
            "meta fee/CU survive"
        );
        assert!(tx.recv_unix_ms.is_some(), "wire clock survives");
        let classified = classify_pump_instructions(&tx);
        let evs = instructions_to_events_with_meta(
            &classified,
            tx.slot,
            tx.is_live,
            tx.recv_unix_ms,
            tx.fee_lamports,
            tx.cu_consumed,
        );
        for pe in &evs {
            if let AppEvent::AmmSwap {
                pool_is_canonical,
                quote_is_wsol,
                fee_lamports,
                cu_consumed,
                recv_unix_ms,
                ..
            } = &pe.event
            {
                amm_events += 1;
                assert!(*quote_is_wsol || !*pool_is_canonical);
                assert_eq!(*fee_lamports, tx.fee_lamports);
                assert_eq!(*cu_consumed, tx.cu_consumed);
                assert_eq!(*recv_unix_ms, tx.recv_unix_ms);
                if *pool_is_canonical {
                    canonical += 1;
                }
            }
            if let AppEvent::MarketTrade { price_fp: 0, .. } = &pe.event {
                priced_halves += 1;
            }
            e.tick(pe.event.clone());
        }
    }
    e.tick(AppEvent::Tick);
    assert_eq!(amm_events, 3, "one AmmSwap per real swap transaction");
    assert_eq!(
        canonical, 1,
        "exactly one of the three is the canonical pool; the others are excluded by name"
    );
    assert!(
        priced_halves >= 1,
        "the legacy instruction-half print is still emitted (and refused as NoPrice downstream)"
    );
    let r = e.model_lane_report();
    assert_eq!(
        r.get("amm_excluded:pool_not_canonical")
            .copied()
            .unwrap_or(0)
            + r.get("amm_excluded:quote_not_wsol").copied().unwrap_or(0),
        2,
        "{r:?}"
    );
}

#[test]
fn a_line_without_inner_instructions_yields_no_amm_swap_and_is_counted_not_guessed() {
    // The OLD emitter's shape: outer instructions only, no meta. The decoder must produce nothing.
    let raw = include_str!("fixtures/pumpswap_rpc_txs.json");
    let v = pq_stream_capture::json::parse(raw).unwrap();
    let item = &v.as_array().unwrap()[0];
    let line = pq_stream_capture::json::serialize(item.get("line").unwrap());
    let Some(LaserStreamUpdate::Transaction(mut tx)) = parse_ndjson_line(&line) else {
        panic!()
    };
    // Strip every instruction that is an Anchor event CPI (the inner self-call).
    tx.instructions.retain(|ix| {
        ix.data.get(0..8) != Some(&[0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d][..])
    });
    let evs = instructions_to_events_with_meta(
        &classify_pump_instructions(&tx),
        tx.slot,
        tx.is_live,
        tx.recv_unix_ms,
        None,
        None,
    );
    assert!(!evs
        .iter()
        .any(|p| matches!(p.event, AppEvent::AmmSwap { .. })));
}
