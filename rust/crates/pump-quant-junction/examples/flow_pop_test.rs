//! DIAGNOSTIC (not serving): controlled-population flow replay.
//! Seed = tape rows (status=success) with recv < SEED_BEFORE (capture start), applied once.
//! In-session = tape rows with recv >= SEED_BEFORE in recv order; each clock is served BEFORE any event with
//! recv >= clock is applied. Per-row population: pumpfun row fed by the wire path ("cur"), pumpfun row not fed
//! ("excl"), pumpswap row ("ps"). Modes: cur | cur+ps | cur+excl | all.
//! usage: flow_pop_test <tape> <fed_keys.tsv> <mint> <creator> <seed_before_ms> <t,t,..> <mode>
//! fed_keys.tsv: sig<TAB>side<TAB>sol_lamports of wire-path events that carried a corpus basis.
use std::collections::{HashMap, HashSet};
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
    let fed: HashSet<(String, bool, i64)> =
        std::io::BufReader::new(std::fs::File::open(&a[2]).expect("fed"))
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| {
                let p: Vec<&str> = l.split('\t').collect();
                Some((
                    p.first()?.to_string(),
                    *p.get(1)? == "buy",
                    p.get(2)?.parse().ok()?,
                ))
            })
            .collect();
    let mint = pk(&a[3]);
    let creator = pk(&a[4]);
    let seed_before: i64 = a[5].parse().expect("seed_before");
    let mut clocks: Vec<i64> = a[6].split(',').map(|x| x.parse().expect("t")).collect();
    clocks.sort_unstable();
    assert!(seed_before <= clocks[0]);
    let mode = a[7].as_str();
    let (want_ps, want_excl) = match mode {
        "cur" => (false, false),
        "cur+ps" => (true, false),
        "cur+excl" => (false, true),
        "all" => (true, true),
        _ => panic!("mode"),
    };
    let last = *clocks.last().expect("clock");
    let mut r = FlowReducer::new();
    r.track_mint(mint);
    r.set_creator(mint, creator);
    let mut keys: HashMap<String, [u8; 32]> = HashMap::new();
    let mut key = |s: &str| *keys.entry(s.to_string()).or_insert_with(|| pk(s));
    let (mut n_seed, mut n_cur, mut n_ps, mut n_excl, mut n_skip) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut ci = 0usize;
    let rd = std::io::BufReader::with_capacity(1 << 22, std::fs::File::open(&a[1]).expect("tape"));
    for line in rd.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("status").and_then(|s| s.as_str()) != Some("success") {
            continue;
        }
        let t = v["recv_unix_ms"].as_i64().unwrap_or(0);
        while ci < clocks.len() && clocks[ci] <= t {
            match r.serve(&mint, clocks[ci]) {
                FlowOutcome::NoPriorFlow => {
                    println!(
                        "{}",
                        serde_json::json!({"t": clocks[ci], "no_prior_flow": true})
                    )
                }
                FlowOutcome::Aggregates(g) => {
                    let c = g.to_corpus_values();
                    println!(
                        "{}",
                        serde_json::json!({"t": clocks[ci], "entrants_60s": g.entrants_60s,
                        "entrants_300s": g.entrants_300s, "net_flow_sol_300s": c.net_flow_sol_300s,
                        "fresh_wallet_share_300s": c.fresh_wallet_share_300s,
                        "flow_lookback_d": c.flow_lookback_d, "sniper_share_300s": c.sniper_share_300s,
                        "bot_uniform_share_300s": c.bot_uniform_share_300s,
                        "smart_entrants_300s": g.smart_entrants_300s,
                        "smart_net_flow_sol_300s": c.smart_net_flow_sol_300s,
                        "coentry_wallets_300s": g.coentry_wallets_300s,
                        "mode": mode, "seed": n_seed, "cur": n_cur, "ps": n_ps, "excl": n_excl, "skipped": n_skip})
                    );
                }
            }
            ci += 1;
        }
        if t > last {
            break;
        }
        let venue = v["venue"].as_str().unwrap_or("");
        let is_buy = v["side"].as_str() == Some("buy");
        let sol = v["sol_lamports"].as_i64().unwrap_or(0);
        if t < seed_before {
            n_seed += 1;
        } else {
            let is_fed = venue == "pumpfun"
                && fed.contains(&(
                    v["signature"].as_str().unwrap_or("").to_string(),
                    is_buy,
                    sol,
                ));
            let take = if venue == "pumpswap" {
                want_ps
            } else if is_fed {
                true
            } else {
                want_excl
            };
            if !take {
                n_skip += 1;
                continue;
            }
            if venue == "pumpswap" {
                n_ps += 1;
            } else if is_fed {
                n_cur += 1;
            } else {
                n_excl += 1;
            }
        }
        r.on_event(&FlowEvent {
            mint: key(v["mint"].as_str().unwrap_or("")),
            trader: key(v["trader"].as_str().unwrap_or("")),
            side: if is_buy { Side::Buy } else { Side::Sell },
            slot: v["slot"].as_u64().unwrap_or(0),
            recv_unix_ms: t,
            sol_lamports: sol,
            fee_lamports: v["fee_lamports"].as_u64().unwrap_or(0),
            cu_consumed: v["cu_consumed"].as_u64(),
        });
    }
}
