//! Vertical slice: raw wire -> production parser -> event decoder + corpus resolver -> production join -> RENDERED prompt.
//! usage: slice_prompt <wire_dir> <mint_b58> <creator_b58> <launch_ms> <t_dec_ms>[,<t_dec_ms>...]
//! Output: one JSON line per clock {t, prompt|refusal, counters}. Causality: a clock is served BEFORE any line whose
//! recv_unix_ms >= t is applied (a late line can never reach an earlier clock).
use std::io::BufRead;
use std::str::FromStr;

use pump_quant_app::decision_join::{DecisionCache, TradeObs};
use pump_quant_app::event::AppEvent;
use pump_quant_app::state_ledger::VenueLabel;
use pump_quant_junction::curve_trade_events::{ingest_curve_tx, EventDedup};
use pump_quant_junction::laserstream::{parse_ndjson_line, LaserStreamUpdate, PUMP_FUN_PROGRAM};
use pump_quant_protocol::decode::decode_pump_curve;
use pump_quant_protocol::pda::find_program_address;

fn pk(s: &str) -> [u8; 32] {
    solana_program::pubkey::Pubkey::from_str(s)
        .expect("pubkey")
        .to_bytes()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let mint = pk(&a[2]);
    let creator = pk(&a[3]);
    let launch_ms: i64 = a[4].parse().expect("launch_ms");
    let mut clocks: Vec<i64> = a[5].split(',').map(|x| x.parse().expect("t")).collect();
    clocks.sort_unstable();
    let curve_pda = find_program_address(&[b"bonding-curve", &mint], &PUMP_FUN_PROGRAM)
        .expect("pda")
        .0;
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("wire_"))
        })
        .collect();
    files.sort();
    let mut cache = DecisionCache::new();
    assert!(cache.observe_launch(mint, creator, launch_ms));
    let mut dedup = EventDedup::new(1 << 16);
    let mut ci = 0usize;
    let mut n_trades = 0u64;
    let mut n_other_mint = 0u64;
    let mut emit = |cache: &DecisionCache, t: i64, n_trades: u64, dedup: &EventDedup| {
        let rec = match cache.snapshot(&mint, t) {
            Ok(s) => {
                serde_json::json!({"t": t, "prompt": s.user_prompt, "system": s.system_prompt, "n_prior": s.n_prior_trades,
                "venue": s.venue, "trades_fed": n_trades, "counters": format!("{:?}", cache.counters()),
                "unsupported_quote": dedup.unsupported_quote, "corpus_basis_resolved": dedup.corpus_basis_resolved,
                "outside_corpus": format!("{:?}", dedup.outside_corpus)})
            }
            Err(r) => serde_json::json!({"t": t, "refusal": r.as_str(), "trades_fed": n_trades,
                "counters": format!("{:?}", cache.counters())}),
        };
        println!("{rec}");
    };
    for f in &files {
        let r = std::io::BufReader::new(std::fs::File::open(f).expect("open"));
        for l in r.lines().map_while(Result::ok) {
            let Some(u) = parse_ndjson_line(&l) else {
                continue;
            };
            let recv = match &u {
                LaserStreamUpdate::Transaction(t) => t.recv_unix_ms,
                LaserStreamUpdate::Account { recv_unix_ms, .. } => *recv_unix_ms,
                _ => None,
            };
            let Some(recv) = recv else { continue };
            while ci < clocks.len() && clocks[ci] <= recv {
                emit(&cache, clocks[ci], n_trades, &dedup);
                ci += 1;
            }
            match u {
                LaserStreamUpdate::Transaction(tx) => {
                    let mut evs = Vec::new();
                    let _ = ingest_curve_tx(&tx, &mut dedup, &mut evs);
                    for pe in evs {
                        if let AppEvent::MarketTrade {
                            mint: m,
                            price_fp,
                            quote_lamports,
                            signed_base,
                            buyer_entity,
                            trader_pubkey,
                            recv_unix_ms,
                            slot,
                            fee_lamports,
                            cu_consumed,
                            event_id,
                            feature,
                            ..
                        } = pe.event
                        {
                            if *m.as_bytes() != mint {
                                n_other_mint += 1;
                                continue;
                            }
                            // MEASUREMENT ONLY (never in serving): SLICE_BAND_MEDIAN=<whole-run median price> applies the corpus's
                            // lookahead band [median/10, median*10] to the trained inputs, to isolate that lookahead from every
                            // other difference. Unset = the production causal path, untouched.
                            if let (Ok(m), Some(f)) = (std::env::var("SLICE_BAND_MEDIAN"), feature)
                            {
                                if let Ok(med) = m.parse::<f64>() {
                                    let px = f.sol_lamports.unsigned_abs() as f64
                                        / f.tokens_raw.unsigned_abs().max(1) as f64;
                                    if f.sol_lamports.unsigned_abs() >= 100_000
                                        && f.tokens_raw.unsigned_abs() >= 1_000_000
                                        && (px < med / 10.0 || px > med * 10.0)
                                    {
                                        continue;
                                    }
                                }
                            }
                            n_trades += 1;
                            if let Ok(p) = std::env::var("SLICE_DUMP") {
                                use std::io::Write as _;
                                if let Ok(mut f) = std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(p)
                                {
                                    let (sol, tok, tr) = feature.map_or((0, 0, [0u8; 32]), |f| {
                                        (f.sol_lamports, f.tokens_raw, f.trader)
                                    });
                                    let _ = writeln!(
                                        f,
                                        "{}\t{}\t{}\t{}\t{}\t{}",
                                        recv_unix_ms.unwrap_or(0),
                                        u8::from(feature.is_some()),
                                        sol,
                                        tok,
                                        quote_lamports,
                                        signed_base + i64::from(tr[0]) * 0
                                    );
                                }
                            }
                            cache.observe_trade(&TradeObs {
                                mint,
                                price_fp,
                                quote_lamports,
                                signed_base,
                                buyer_entity,
                                trader: trader_pubkey,
                                recv_unix_ms,
                                slot,
                                fee_lamports,
                                cu_consumed,
                                venue: VenueLabel::Pumpfun,
                                event_id,
                                feature,
                            });
                        }
                    }
                }
                LaserStreamUpdate::Account {
                    pubkey,
                    data,
                    slot,
                    recv_unix_ms,
                    ..
                } => {
                    if pubkey == curve_pda {
                        if let (Some(c), Some(ts)) = (decode_pump_curve(&data), recv_unix_ms) {
                            let _ = cache.observe_curve(
                                mint,
                                pump_quant_app::curve_annotation::CurveObservation {
                                    v_sol_lamports: c.virtual_sol,
                                    v_tokens: c.virtual_token,
                                    real_sol_lamports: c.real_sol,
                                    real_tokens: c.real_token,
                                    ts_ms: ts,
                                    slot,
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }
    while ci < clocks.len() {
        emit(&cache, clocks[ci], n_trades, &dedup);
        ci += 1;
    }
    eprintln!("other-mint events skipped: {n_other_mint}");
}
