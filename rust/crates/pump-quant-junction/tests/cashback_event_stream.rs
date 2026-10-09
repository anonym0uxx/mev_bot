//! Cashback through the PRODUCER and the event stream, on REAL captured transactions
//! (`fixtures/pumpswap_rpc_txs.json`, 3 mainnet PumpSwap txs: 2 x buy layout 472, 1 x sell layout 409; and the two
//! `amm_buy_*_bound.ndjson` captures, layout 457). Each AmmSwap the decoder emits carries the event's own
//! cashback pair with layout provenance, and survives writer -> file -> checked reader unchanged.
use pump_quant_app::event::AppEvent;
use pump_quant_junction::event_stream::{read_event_stream_checked, EventStreamWriter};
use pump_quant_junction::laserstream::{
    classify_pump_instructions, decode_amm_swaps, instructions_to_events_with_meta,
    parse_ndjson_line, LaserStreamTx, LaserStreamUpdate, PumpInstruction, PUMP_FUN_PROGRAM,
    PUMP_SWAP_PROGRAM,
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

const EVENT_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

fn event_ix_indices(tx: &LaserStreamTx) -> Vec<usize> {
    tx.instructions
        .iter()
        .enumerate()
        .filter(|(_, ix)| {
            ix.program_id == PUMP_SWAP_PROGRAM
                && ix.data.get(0..8) == Some(&EVENT_TAG[..])
                && (swap_event_payload(&ix.data, true).is_some()
                    || swap_event_payload(&ix.data, false).is_some())
        })
        .map(|(i, _)| i)
        .collect()
}

/// EMITTING-PROGRAM binding: the SAME real event bytes (same discriminator, same known layout) emitted by any
/// program other than PumpSwap yield NO AmmSwap and no cashback pair: neither from `decode_amm_swaps` nor from
/// the classifier. Fails if the producer accepts the payload on discriminator + length alone.
#[test]
fn identical_cashback_event_bytes_from_a_foreign_program_are_not_accepted() {
    let mut n = 0;
    for tx in txs() {
        let evs = event_ix_indices(&tx);
        assert!(!evs.is_empty());
        let (base, _) = decode_amm_swaps(&tx);
        assert_eq!(base.len(), evs.len(), "control: every pAMM event is accepted");
        for foreign in [PUMP_FUN_PROGRAM, [0x42; 32]] {
            let mut t = tx.clone();
            for &i in &evs {
                t.instructions[i].program_id = foreign;
            }
            let (facts, _) = decode_amm_swaps(&t);
            assert!(
                facts.is_empty(),
                "foreign-emitted event accepted: {:?}",
                facts.iter().map(|f| f.cashback).collect::<Vec<_>>()
            );
            assert!(
                !classify_pump_instructions(&t)
                    .iter()
                    .any(|c| matches!(c, PumpInstruction::AmmSwap(_))),
                "classifier emitted an AmmSwap from a foreign program"
            );
            n += 1;
        }
    }
    assert!(n >= 10, "{n}");
}

/// INVOCATION binding: with invocation positions, a PumpSwap-emitted event is accepted only when its CPI parent
/// is the PumpSwap swap instruction of the event's pool. Same tx, same bytes: parent = swap ix -> Known;
/// parent = another program's instruction -> excluded (counted), no cashback.
#[test]
fn cashback_event_is_bound_to_its_emitting_swap_instruction() {
    let tx = txs().into_iter().next().unwrap();
    assert_eq!(event_ix_indices(&tx).len(), 1);
    let swap = tx
        .instructions
        .iter()
        .position(|ix| ix.program_id == PUMP_SWAP_PROGRAM && ix.data.get(0..8) != Some(&EVENT_TAG[..]))
        .unwrap();
    let other = tx
        .instructions
        .iter()
        .position(|ix| ix.program_id != PUMP_SWAP_PROGRAM)
        .unwrap();
    let with_parent = |parent: usize| {
        let mut t = tx.clone();
        for (i, ix) in t.instructions.iter_mut().enumerate() {
            ix.outer = Some(0);
            ix.depth = Some(if i == parent { 1 } else { 2 });
        }
        t
    };
    let (ok, ex_ok) = decode_amm_swaps(&with_parent(swap));
    assert_eq!(ex_ok, 0);
    assert_eq!(ok.len(), 1);
    assert!(matches!(ok[0].cashback, CashbackField::Known { .. }));
    assert_eq!(ok[0].swap_ix, Some(swap));
    let (bad, ex_bad) = decode_amm_swaps(&with_parent(other));
    assert!(bad.is_empty(), "event attached under a non-PumpSwap parent");
    assert!(ex_bad >= 1, "the refusal is counted");
}
