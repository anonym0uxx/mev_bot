//! DIAGNOSTIC (not serving): replay the frozen corpus tape (all venues, all mints, status=success, recv order) through the
//! production Rust `FlowReducer`, serve the given (mint, t) clocks BEFORE applying any event with recv >= t (the builder's
//! own causality rule), and print the aggregates as JSON. Compared offline with the persisted C11_FLOW_ENRICHMENT.jsonl.
//! usage: flow_tape_replay <tape.jsonl> <mint_b58> <creator_b58> <t,t,...> [--start-ms N]
//!   --start-ms N: first event applied has recv >= N (cold-start experiment: what a collector that began at N would see).
use std::collections::HashMap;
use std::io::BufRead;
use std::str::FromStr;

use pump_quant_market_state::flow_reducer::{FlowEvent, FlowOutcome, FlowReducer, Side};

fn pk(s: &str) -> [u8; 32] {
    solana_program::pubkey::Pubkey::from_str(s)
        .expect("pubkey")
        .to_bytes()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let tape = &a[1];
    let mint = pk(&a[2]);
    let creator = pk(&a[3]);
    let mut clocks: Vec<i64> = a[4].split(',').map(|x| x.parse().expect("t")).collect();
    clocks.sort_unstable();
    let start_ms: i64 = a
        .iter()
        .position(|x| x == "--start-ms")
        .map_or(i64::MIN, |i| a[i + 1].parse().expect("start"));
    let last = *clocks.last().expect("clock");
    let mut r = FlowReducer::new();
    r.track_mint(mint);
    r.set_creator(mint, creator);
    let mut cache: HashMap<String, [u8; 32]> = HashMap::new();
    let mut key = |s: &str| *cache.entry(s.to_string()).or_insert_with(|| pk(s));
    let mut ci = 0usize;
    let mut applied = 0u64;
    let rd = std::io::BufReader::with_capacity(1 << 22, std::fs::File::open(tape).expect("tape"));
    for line in rd.lines().map_while(Result::ok) {
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("status").and_then(|s| s.as_str()) != Some("success") {
            continue;
        }
        let t = v["recv_unix_ms"].as_i64().unwrap_or(0);
        while ci < clocks.len() && clocks[ci] <= t {
            let o = r.serve(&mint, clocks[ci]);
            match o {
                FlowOutcome::NoPriorFlow => println!(
                    "{}",
                    serde_json::json!({"t": clocks[ci], "no_prior_flow": true})
                ),
                FlowOutcome::Aggregates(g) => {
                    let c = g.to_corpus_values();
                    println!(
                        "{}",
                        serde_json::json!({"t": clocks[ci], "entrants_60s": g.entrants_60s, "entrants_300s": g.entrants_300s,
                        "net_flow_sol_300s": c.net_flow_sol_300s, "fresh_wallet_share_300s": c.fresh_wallet_share_300s,
                        "flow_lookback_d": c.flow_lookback_d, "sniper_share_300s": c.sniper_share_300s,
                        "bot_uniform_share_300s": c.bot_uniform_share_300s, "smart_entrants_300s": g.smart_entrants_300s,
                        "smart_net_flow_sol_300s": c.smart_net_flow_sol_300s, "coentry_wallets_300s": g.coentry_wallets_300s,
                        "applied": applied})
                    );
                }
            }
            ci += 1;
        }
        if t > last {
            break;
        }
        if t < start_ms {
            continue;
        }
        let side = match v["side"].as_str() {
            Some("buy") => Side::Buy,
            _ => Side::Sell,
        };
        r.on_event(&FlowEvent {
            mint: key(v["mint"].as_str().unwrap_or("")),
            trader: key(v["trader"].as_str().unwrap_or("")),
            side,
            slot: v["slot"].as_u64().unwrap_or(0),
            recv_unix_ms: t,
            sol_lamports: v["sol_lamports"].as_i64().unwrap_or(0),
            fee_lamports: v["fee_lamports"].as_u64().unwrap_or(0),
            cu_consumed: v["cu_consumed"].as_u64(),
        });
        applied += 1;
    }
}
