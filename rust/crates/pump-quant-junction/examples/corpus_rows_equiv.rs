//! Equivalence harness: for every successful pump.fun transaction on stdin (wire lines carrying balances), emit the
//! Rust `corpus_rows::resolve_row` result for each corpus-known buy/sell instruction as TSV:
//!   sig \t ix_ordinal \t side \t mint_b58 \t trader_b58 \t sol \t tokens \t via_net
//! The corpus tape (renormalize_raw.py) is compared against this by `compare_corpus_rows.py`.
use std::io::{BufRead, Write};

use pump_quant_junction::corpus_rows::{corpus_side, not_a_launch_set, resolve_row};
use pump_quant_junction::laserstream::{parse_ndjson_line, LaserStreamUpdate, PUMP_FUN_PROGRAM};

fn b58(b: &[u8]) -> String {
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut digits: Vec<u8> = Vec::new();
    for &byte in b {
        let mut carry = u32::from(byte);
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
    let mut s = String::new();
    for &byte in b {
        if byte == 0 {
            s.push('1');
        } else {
            break;
        }
    }
    for d in digits.iter().rev() {
        s.push(A[*d as usize] as char);
    }
    s
}

fn main() {
    let not_launch = not_a_launch_set();
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { continue };
        let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(&line) else {
            continue;
        };
        if tx.tx_ok != Some(true) {
            continue;
        }
        let Some(bal) = tx.balances.as_ref() else {
            let _ = writeln!(out, "NOBAL\t{}", b58(&tx.signature));
            continue;
        };
        let sig = b58(&tx.signature);
        for (i, ix) in tx.instructions.iter().enumerate() {
            if ix.program_id != PUMP_FUN_PROGRAM {
                continue;
            }
            let Some(is_buy) = corpus_side(&ix.data) else {
                continue;
            };
            match resolve_row(is_buy, &ix.accounts, &tx.account_keys, &tx.invalid_key_idx, bal, &not_launch) {
                Some(r) => {
                    let _ = writeln!(
                        out,
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        sig,
                        i,
                        if r.is_buy { "buy" } else { "sell" },
                        b58(&r.mint),
                        b58(&r.trader),
                        r.sol_lamports,
                        r.tokens_raw,
                        u8::from(r.via_net_position)
                    );
                }
                None => {
                    let _ = writeln!(out, "REJ\t{}\t{}", sig, i);
                }
            }
        }
    }
}
