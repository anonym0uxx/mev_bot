//! AMM QUOTE VALIDATION over every captured PumpSwap Buy/Sell event (both sessions), BUY and SELL separately.
//! Uses the repo's own decoder and `buy_exact_quote_in`; the SELL net rule is a CANDIDATE tested here (not assumed).
//! BUY : buy_exact_quote_in(pre reserves, virtual_quote, user_quote_amount_in, fee bps) -> base_out  vs  event base_amount_out
//! SELL: gross = sell_gross_quote_out(pre reserves, virtual_quote, base_in) vs event quote_amount_out (verified rule),
//!       then net = gross - ceil(gross*lp/1e4) - ceil(gross*prot/1e4) - ceil(gross*creator/1e4) vs event user_quote_amount_out.
//! Population keys: canonical WSOL pool (as the execution scope) vs everything else, counted apart. Misses are binned.
use pump_quant_junction::laserstream::{parse_ndjson_line, LaserStreamUpdate};
use pump_quant_protocol::pumpswap_event::{
    buy_exact_quote_in, decode_pumpswap_event, sell_gross_quote_out, PumpSwapEvent,
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};

#[derive(Default)]
struct Tally {
    n: u64,
    gross_or_out_match: u64,
    net_match: u64,
    no_vq: u64,
    no_creator_tail: u64,
    err_net: Vec<i128>,
}

/// Kind of the swap instruction naming `pool` in this transaction (first match).
fn ix_kind(tx: &pump_quant_junction::laserstream::LaserStreamTx, pool: &[u8; 32]) -> &'static str {
    for ix in &tx.instructions {
        if ix.program_id != pump_quant_junction::laserstream::PUMP_SWAP_PROGRAM
            || ix.data.get(0..8) == Some(&[0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d][..])
        {
            continue;
        }
        let Some(&ai) = ix.accounts.first() else {
            continue;
        };
        if tx.account_keys.get(usize::from(ai)) != Some(pool) {
            continue;
        }
        return match ix.data.get(0..8) {
            Some(d) if d == [198, 46, 21, 82, 180, 217, 232, 112] => "exact_quote_in",
            Some(d) if d == [102, 6, 61, 18, 1, 218, 235, 234] => "exact_out",
            _ => "other",
        };
    }
    "no_ix"
}

fn ceil_div(a: u128, b: u128) -> u128 {
    a.div_ceil(b)
}

fn main() {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    let mut buy: BTreeMap<&'static str, Tally> = BTreeMap::new();
    let mut sell: BTreeMap<&'static str, Tally> = BTreeMap::new();
    for dir in &dirs {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        files.sort();
        for f in files {
            for line in BufReader::new(std::fs::File::open(f).unwrap()).lines() {
                let line = line.unwrap();
                if !line.contains("pAMMBay6") {
                    continue;
                }
                let Some(LaserStreamUpdate::Transaction(tx)) = parse_ndjson_line(&line) else {
                    continue;
                };
                if tx.tx_ok != Some(true) {
                    continue;
                }
                // canonical-ness per pool from the production decoder (mint+pool bound by PDA); events not in `facts` are non-canonical
                let (facts, _) = pump_quant_junction::laserstream::decode_amm_swaps(&tx);
                let canon: std::collections::HashSet<[u8; 32]> = facts
                    .iter()
                    .filter(|f| f.canonical && f.quote_is_wsol)
                    .map(|f| f.pool)
                    .collect();
                for ix in &tx.instructions {
                    if ix.program_id != pump_quant_junction::laserstream::PUMP_SWAP_PROGRAM {
                        continue;
                    }
                    match decode_pumpswap_event(&ix.data) {
                        Some(PumpSwapEvent::Buy(b)) => {
                            let k: &'static str =
                                match (canon.contains(&b.pool), ix_kind(&tx, &b.pool)) {
                                    (true, "exact_quote_in") => "canonical_wsol|buy_exact_quote_in",
                                    (true, "exact_out") => "canonical_wsol|buy_exact_out",
                                    (true, _) => "canonical_wsol|buy_other_ix",
                                    (false, "exact_quote_in") => "other|buy_exact_quote_in",
                                    (false, "exact_out") => "other|buy_exact_out",
                                    (false, _) => "other|buy_other_ix",
                                };
                            let t = buy.entry(k).or_default();
                            t.n += 1;
                            let (Some(vq), Some(cbps)) =
                                (b.virtual_quote_reserves, b.coin_creator_fee_basis_points)
                            else {
                                if b.virtual_quote_reserves.is_none() {
                                    t.no_vq += 1
                                } else {
                                    t.no_creator_tail += 1
                                }
                                continue;
                            };
                            match buy_exact_quote_in(
                                u128::from(b.pool_base_token_reserves),
                                u128::from(b.pool_quote_token_reserves),
                                u128::from(vq),
                                // gross spend X = user_quote_amount_in + lp + protocol + creator fee (the vector definition in pumpswap_quote_vectors.json)
                                u128::from(b.user_quote_amount_in)
                                    + u128::from(b.lp_fee)
                                    + u128::from(b.protocol_fee)
                                    + u128::from(b.coin_creator_fee.unwrap_or(0)),
                                u128::from(b.lp_fee_basis_points),
                                u128::from(b.protocol_fee_basis_points),
                                u128::from(cbps),
                            ) {
                                Some(q) => {
                                    if q.base_out == u128::from(b.base_amount_out) {
                                        t.gross_or_out_match += 1;
                                    } else {
                                        t.err_net.push(
                                            q.base_out as i128 - i128::from(b.base_amount_out),
                                        );
                                    }
                                }
                                None => t.err_net.push(i128::MIN),
                            }
                        }
                        Some(PumpSwapEvent::Sell(s)) => {
                            let k = if canon.contains(&s.pool) {
                                "canonical_wsol"
                            } else {
                                "other"
                            };
                            let t = sell.entry(k).or_default();
                            t.n += 1;
                            let (Some(vq), Some(cbps)) =
                                (s.virtual_quote_reserves, s.coin_creator_fee_basis_points)
                            else {
                                if s.virtual_quote_reserves.is_none() {
                                    t.no_vq += 1
                                } else {
                                    t.no_creator_tail += 1
                                }
                                continue;
                            };
                            let Some(gross) = sell_gross_quote_out(
                                u128::from(s.pool_base_token_reserves),
                                u128::from(s.pool_quote_token_reserves),
                                u128::from(vq),
                                u128::from(s.base_amount_in),
                            ) else {
                                t.err_net.push(i128::MIN);
                                continue;
                            };
                            if gross == u128::from(s.quote_amount_out) {
                                t.gross_or_out_match += 1;
                            }
                            let net = gross
                                .saturating_sub(ceil_div(
                                    gross * u128::from(s.lp_fee_basis_points),
                                    10_000,
                                ))
                                .saturating_sub(ceil_div(
                                    gross * u128::from(s.protocol_fee_basis_points),
                                    10_000,
                                ))
                                .saturating_sub(ceil_div(gross * u128::from(cbps), 10_000));
                            if net == u128::from(s.user_quote_amount_out) {
                                t.net_match += 1;
                            } else {
                                t.err_net
                                    .push(net as i128 - i128::from(s.user_quote_amount_out));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let show = |name: &str, m: &BTreeMap<&'static str, Tally>| {
        for (k, t) in m {
            let mut e = t.err_net.clone();
            e.sort();
            let med = e.get(e.len() / 2).copied();
            println!(
                "{name} pop={k} n={} no_virtual_quote={} no_creator_tail={} leg1_match={} net_match={} misses={} miss_min={:?} miss_median={:?} miss_max={:?}",
                t.n, t.no_vq, t.no_creator_tail, t.gross_or_out_match, t.net_match, e.len(), e.first(), med, e.last()
            );
        }
    };
    show("BUY ", &buy);
    show("SELL", &sell);
}
