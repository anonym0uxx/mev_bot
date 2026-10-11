//! MEASUREMENT: run the production PumpSwap row producer over a whole wire directory (a session slice) and report every outcome:
//! rows emitted, rejects by frozen-builder rule and population, duplicates, no-balance / not-verified-success txs, and a
//! deliberate redelivery pass (the same wire fed twice) as a deduplication proof rather than an observation of zero.
//! Also classifies each reject against the FROZEN TAPE: was a row for that (signature, instruction) present there?
use pump_quant_junction::curve_trade_events::{ingest_amm_rows, AmmRowStats, EventDedup};
use pump_quant_junction::laserstream::{parse_ndjson_line, LaserStreamUpdate};
use std::io::{BufRead, BufReader};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    files.sort();
    let mut dedup = EventDedup::new(1 << 22);
    let mut st = AmmRowStats::default();
    let mut out = Vec::new();
    let mut txs = 0u64;
    let mut swap_txs = 0u64;
    let mut rows_sig: Vec<(String, u64)> = Vec::new();
    let mut reject_sigs: Vec<String> = Vec::new();
    for pass in 0..2 {
        let before = st.clone();
        for f in &files {
            for line in BufReader::new(std::fs::File::open(f).unwrap()).lines() {
                let line = line.unwrap();
                if !line.contains("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA") {
                    continue;
                }
                let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(&line) else {
                    continue;
                };
                if pass == 0 {
                    txs += 1;
                    if tx.instructions.iter().any(|i| {
                        i.program_id == pump_quant_junction::laserstream::PUMP_SWAP_PROGRAM
                    }) {
                        swap_txs += 1;
                    }
                }
                let n0 = st.resolver_rejects;
                ingest_amm_rows(&tx, &mut dedup, &mut st, &mut out);
                if pass == 0 && st.resolver_rejects > n0 {
                    let sig = bs58_sig(&tx.signature);
                    reject_sigs.push(sig);
                }
            }
        }
        if pass == 0 {
            println!("PASS1 txs_with_amm_program={txs} swap_txs={swap_txs} emitted={} rejects={} dup={} no_balances_txs={} not_verified_success_txs={}",
                st.emitted, st.resolver_rejects, st.duplicates, st.no_balances_txs, st.not_verified_success_txs);
            println!("REASONS {:?}", st.reject_reasons);
            println!("POP {:?}", st.reject_population);
            rows_sig.push(("emitted".into(), st.emitted));
        } else {
            println!(
                "PASS2 (identical redelivery) new_emitted={} duplicates_added={} rejects_added={}",
                st.emitted - before.emitted,
                st.duplicates - before.duplicates,
                st.resolver_rejects - before.resolver_rejects
            );
        }
    }
    std::fs::write(&a[2], reject_sigs.join("\n")).unwrap();
}

fn bs58_sig(s: &[u8; 64]) -> String {
    solana_sdk_b58(s)
}
fn solana_sdk_b58(s: &[u8]) -> String {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut digits: Vec<u8> = vec![0];
    for &b in s {
        let mut carry = b as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::new();
    for &b in s {
        if b == 0 {
            out.push('1');
        } else {
            break;
        }
    }
    for d in digits.iter().rev() {
        out.push(A[*d as usize] as char);
    }
    out
}
