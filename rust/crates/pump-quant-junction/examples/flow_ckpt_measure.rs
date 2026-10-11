//! MEASUREMENT: build the flow history from the frozen tape prefix (recv < SEED_BEFORE_MS), persist a checkpoint,
//! reload it, and report size / write time / restore time / equality of served values. Diagnostic only.
use pump_quant_app::flow_checkpoint::{load, FlowHistory, IngestStats, Load, Provenance};
use pump_quant_market_state::flow_reducer::{FlowEvent, FlowParams, Side};
use std::io::{BufRead, BufReader};
use std::time::Instant;

fn b58(s: &str) -> [u8; 32] {
    use std::str::FromStr;
    solana_program::pubkey::Pubkey::from_str(s)
        .map(|p| p.to_bytes())
        .unwrap_or([0; 32])
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (tape, before, out) = (&a[1], a[2].parse::<i64>().unwrap(), &a[3]);
    let mut h = FlowHistory::new(
        FlowParams::default(),
        Provenance {
            seed_source: "frozen_tape:renormalized_v7".into(),
            seed_sha256: "see milestone6".into(),
            seed_before_ms: before,
            producer: "flow_ckpt_measure".into(),
        },
    );
    let mut st = IngestStats::default();
    let t0 = Instant::now();
    let mut n = 0u64;
    for line in BufReader::new(std::fs::File::open(tape).unwrap()).lines() {
        let line = line.unwrap();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(t) = v["recv_unix_ms"].as_i64() else {
            continue;
        };
        if t >= before || v["status"] != "success" {
            continue;
        }
        let side = if v["side"] == "buy" {
            Side::Buy
        } else {
            Side::Sell
        };
        let e = FlowEvent {
            mint: b58(v["mint"].as_str().unwrap_or("")),
            trader: b58(v["trader"].as_str().unwrap_or("")),
            side,
            slot: v["slot"].as_u64().unwrap_or(0),
            recv_unix_ms: t,
            sol_lamports: v["sol_lamports"].as_i64().unwrap_or(0),
            fee_lamports: v["fee_lamports"].as_u64().unwrap_or(0),
            cu_consumed: v["cu_consumed"].as_u64(),
        };
        n += 1;
        h.ingest("seed:tape", n as u128, &e, &mut st);
    }
    eprintln!(
        "built from {} events in {:.1}s; applied={} dup={} late={}",
        n,
        t0.elapsed().as_secs_f64(),
        st.applied,
        st.duplicate,
        st.late_unseen
    );
    let (w, l) = h.resources();
    // ENGINE-THREAD COST of a checkpoint: the consistent clone (reducer + meta) handed to the writer. Encoding and disk
    // IO happen on the writer thread; only this clone runs on the feed/engine thread. Measured 5x.
    let mut clone_us = Vec::new();
    for _ in 0..5 {
        let c0 = Instant::now();
        let snap = (h.reducer.clone(), h.meta.clone());
        clone_us.push(c0.elapsed().as_micros() as u64);
        drop(snap);
    }
    let t1 = Instant::now();
    let bytes = h.persist(std::path::Path::new(out)).unwrap();
    let wt = t1.elapsed().as_secs_f64();
    let t2 = Instant::now();
    let Load::Loaded(r) = load(FlowParams::default(), std::path::Path::new(out)) else {
        panic!("did not load")
    };
    let rt = t2.elapsed().as_secs_f64();
    let same = r.encode() == h.encode();
    println!("{{\"clone_us\":{clone_us:?},\"events\":{n},\"wallets\":{w},\"coentry_links\":{l},\"bytes\":{bytes},\"persist_s\":{wt:.2},\"restore_s\":{rt:.2},\"reencode_identical\":{same}}}");
}
