//! Population census through the PRODUCTION path (parse -> decode -> quote identity -> corpus basis) on wire lines.
//! Prints one TSV line per successful curve TradeEvent: sig, ordinal, quote class, basis result, side, sol_amount,
//! basis sol_lamports (or -), and a summary on stderr. Read-only.
use std::io::{BufRead, Write};

use pump_quant_junction::corpus_rows::not_a_launch_set;
use pump_quant_junction::curve_trade_events::{
    corpus_basis_for, decode_curve_trade_events, QuoteIdentity, TxDecode,
};
use pump_quant_junction::laserstream::{parse_ndjson_line, LaserStreamUpdate};

fn b58(b: &[u8]) -> String {
    bs58_like(b)
}
fn bs58_like(b: &[u8]) -> String {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut digits: Vec<u8> = Vec::new();
    for &x in b {
        let mut carry = u32::from(x);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = b.iter().take_while(|&&x| x == 0).count();
    let mut s = "1".repeat(zeros);
    s.extend(digits.iter().rev().map(|&d| A[d as usize] as char));
    s
}

fn main() {
    let not_launch = not_a_launch_set();
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout());
    let mut tally: std::collections::BTreeMap<String, u64> = Default::default();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { continue };
        let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(&line) else {
            continue;
        };
        let TxDecode::Events(evs) = decode_curve_trade_events(&tx) else {
            continue;
        };
        for e in evs {
            let q = match e.quote {
                QuoteIdentity::Sol => "SOL".to_string(),
                QuoteIdentity::Other(m) => format!("OTHER:{}", b58(&m)),
                QuoteIdentity::Unknown => "UNKNOWN".to_string(),
            };
            let (basis, sl) = match corpus_basis_for(&tx, &e, &not_launch) {
                Ok(f) => ("ok".to_string(), f.sol_lamports.to_string()),
                Err(w) => (w.to_string(), "-".to_string()),
            };
            *tally.entry(format!("{q}\t{basis}")).or_insert(0) += 1;
            let _ = writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                b58(&tx.signature),
                e.ix_ordinal,
                q,
                basis,
                if e.is_buy { "buy" } else { "sell" },
                e.sol_amount,
                sl
            );
        }
    }
    for (k, v) in tally {
        eprintln!("{v}\t{k}");
    }
}
