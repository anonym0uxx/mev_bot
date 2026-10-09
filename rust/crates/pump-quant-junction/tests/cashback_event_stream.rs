//! Cashback through the PRODUCER and the event stream, on REAL captured transactions
//! (`fixtures/pumpswap_rpc_txs.json`, 3 mainnet PumpSwap txs: 2 x buy layout 472, 1 x sell layout 409; and the two
//! `amm_buy_*_bound.ndjson` captures, layout 457). Each AmmSwap the decoder emits carries the event's own
//! cashback pair with layout provenance, and survives writer -> file -> checked reader unchanged.
use pump_quant_app::event::AppEvent;
use pump_quant_junction::event_stream::{read_event_stream_checked, EventStreamWriter};
use pump_quant_junction::laserstream::{
    classify_pump_instructions, decode_amm_swaps, instructions_to_events_with_meta,
    parse_ndjson_line, LaserStreamTx, LaserStreamUpdate,
};
use pump_quant_protocol::pumpswap_event::{cashback_fields, swap_event_payload, CashbackField};

fn txs() -> Vec<LaserStreamTx> {
    let mut out = Vec::new();
    let raw = include_str!("fixtures/pumpswap_rpc_txs.json");
    let v: pq_stream_capture::json::Value = pq_stream_capture::json::parse(raw).unwrap();
    for item in v.as_array().unwrap() {
        let line = pq_stream_capture::json::serialize(item.get("line").unwrap());
        let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(&line) else {
            panic!("fixture line must parse");
        };
        out.push(tx);
    }
    for nd in [
        include_str!("fixtures/amm_buy_finite_bound.ndjson"),
        include_str!("fixtures/amm_buy_unlimited_bound.ndjson"),
    ] {
        for line in nd.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(line) {
                out.push(tx);
            }
        }
    }
    out
}

/// The cashback pair read independently from the raw event bytes (by the protocol layout table).
fn raw_pairs(tx: &LaserStreamTx) -> Vec<(u64, u64, u16)> {
    let mut v = Vec::new();
    for ix in &tx.instructions {
        for buy in [true, false] {
            if let Some(p) = swap_event_payload(&ix.data, buy) {
                let (b, l) = cashback_fields(buy, p).expect("known layout");
                v.push((b, l, p.len() as u16));
            }
        }
    }
    v
}

#[test]
fn real_captured_swaps_carry_known_cashback_with_layout_provenance_through_the_stream() {
    let mut evs = Vec::new();
    let mut layouts = std::collections::BTreeSet::new();
    for tx in txs() {
        let (facts, _) = decode_amm_swaps(&tx);
        let mut raw = raw_pairs(&tx);
        let mut got: Vec<(u64, u64, u16)> = facts
            .iter()
            .map(|f| match f.cashback {
                CashbackField::Known {
                    bps,
                    lamports,
                    layout_len,
                } => (bps, lamports, layout_len),
                o => panic!("captured current layout must be known: {o:?}"),
            })
            .collect();
        raw.sort_unstable();
        got.sort_unstable();
        assert_eq!(
            got, raw,
            "decoder pair == raw bytes, layout = payload length"
        );
        layouts.extend(got.iter().map(|x| x.2));
        let classified = classify_pump_instructions(&tx);
        for pe in instructions_to_events_with_meta(
            &classified,
            tx.slot,
            tx.is_live,
            tx.recv_unix_ms,
            tx.fee_lamports,
            tx.cu_consumed,
        ) {
            if let AppEvent::AmmSwap { cashback, .. } = pe.event {
                assert!(matches!(cashback, CashbackField::Known { .. }));
                evs.push(pe.event);
            }
        }
    }
    assert!(evs.len() >= 5, "{}", evs.len());
    assert_eq!(
        layouts.into_iter().collect::<Vec<_>>(),
        vec![409, 457, 472],
        "pre-cashback-coin current layouts; all carry the pair (known zero here)"
    );
    // writer -> file -> checked reader: identical, complete, no schema gap.
    let p = std::env::temp_dir().join(format!("cb_stream_{}.jsonl", std::process::id()));
    let mut w = EventStreamWriter::open(&p).unwrap();
    for (i, e) in evs.iter().enumerate() {
        w.write_event(e, i as u64).unwrap();
    }
    w.flush().unwrap();
    drop(w);
    let c = read_event_stream_checked(&p).unwrap();
    assert_eq!(c.events, evs);
    assert!(c.is_complete(), "{:?}", c.incomplete());
    assert!(c.schema_gaps().is_empty());
    let _ = std::fs::remove_file(&p);
}
