//! Diagnostic: print what the production decoder says for a fixture, next to the instruction list.
use pump_quant_junction::laserstream::{decode_amm_swaps, parse_ndjson_line, LaserStreamUpdate};
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let line = std::fs::read_to_string(p).unwrap();
    let LaserStreamUpdate::Transaction(tx) = parse_ndjson_line(line.trim()).unwrap() else {
        panic!()
    };
    println!(
        "n_instructions={} tx_ok={:?}",
        tx.instructions.len(),
        tx.tx_ok
    );
    for (i, ix) in tx.instructions.iter().enumerate() {
        println!(
            "  ix{i} prog={} disc={:02x?} len={} nacct={}",
            hex8(&ix.program_id),
            &ix.data[..ix.data.len().min(8)],
            ix.data.len(),
            ix.accounts.len()
        );
    }
    let (f, ex) = decode_amm_swaps(&tx);
    println!("swaps={} excluded={ex}", f.len());
    for s in f {
        println!(
            "  buy={} token_amount={} quote_lamports={} canonical={} swap_ix={:?} trader={}",
            s.is_buy,
            s.token_amount,
            s.quote_lamports,
            s.canonical,
            s.swap_ix,
            hex8(&s.trader)
        );
    }
}
fn hex8(b: &[u8; 32]) -> String {
    b[..4].iter().map(|x| format!("{x:02x}")).collect()
}
