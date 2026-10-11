//! Throughput probe: time parse_ndjson_line and classify+translate on captured wire lines, offline. No daemon, no I/O besides reading.
use pump_quant_junction::laserstream::{
    classify_pump_instructions, instructions_to_events_with_meta, parse_ndjson_line,
    LaserStreamUpdate,
};
use std::io::BufRead;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let f = std::fs::File::open(&a[1]).unwrap();
    let (s, e): (usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let lines: Vec<String> = std::io::BufReader::new(f)
        .lines()
        .skip(s - 1)
        .take(e - s + 1)
        .map(|l| l.unwrap())
        .collect();
    let bytes: usize = lines.iter().map(|l| l.len()).sum();
    let t = Instant::now();
    let mut parsed = Vec::new();
    for l in &lines {
        if let Some(u) = parse_ndjson_line(l) {
            parsed.push(u);
        }
    }
    let tp = t.elapsed();
    let t = Instant::now();
    let (mut tx_n, mut ev_n) = (0usize, 0usize);
    for u in &parsed {
        if let LaserStreamUpdate::Transaction(tx) = u {
            tx_n += 1;
            let c = classify_pump_instructions(tx);
            let ev = instructions_to_events_with_meta(
                &c,
                tx.slot,
                tx.is_live,
                tx.recv_unix_ms,
                tx.fee_lamports,
                tx.cu_consumed,
            );
            ev_n += ev.len();
        }
    }
    let tc = t.elapsed();
    println!("lines={} bytes={} parsed={} tx={} events={} parse_ms={:.1} classify_ms={:.1} parse_lines_per_s={:.0}",
        lines.len(), bytes, parsed.len(), tx_n, ev_n, tp.as_secs_f64() * 1e3, tc.as_secs_f64() * 1e3,
        lines.len() as f64 / tp.as_secs_f64());
}
