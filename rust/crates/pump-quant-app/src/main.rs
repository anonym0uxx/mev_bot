//! `pump-quant-app` — laptop paper/replay runner for the Hermes nervous system.
//!
//! Usage:
//!
//! ```text
//! pump-quant-app <paper|replay> <config-file> <events-file>
//! ```
//!
//! Loads an operator config and a recorded event journal, drives the engine, and
//! prints the net-SOL report and the journal digest. A `live` mode is intentionally
//! unrepresentable: live capital is Tier-0 human-gated and this binary refuses to
//! synthesize authorization for it.

use std::process::ExitCode;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, CreatorActionKind};
use pump_quant_domain::ids::Mint as DomainMint;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!(
            "usage: {} <paper|replay> <config-file> <events-file>",
            args[0]
        );
        return ExitCode::from(2);
    }

    let mode = match args[1].as_str() {
        "paper" => RunMode::Paper,
        "replay" => RunMode::Replay,
        "live" => {
            eprintln!(
                "refused: live capital is Tier-0 human-gated and is not available from this binary"
            );
            return ExitCode::from(3);
        }
        other => {
            eprintln!("unknown mode '{other}' (expected paper|replay)");
            return ExitCode::from(2);
        }
    };

    let cfg_text = match std::fs::read_to_string(&args[2]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read config {}: {e}", args[2]);
            return ExitCode::from(1);
        }
    };
    let cfg = match Config::from_str_over_default(&cfg_text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bad config: {e}");
            return ExitCode::from(1);
        }
    };

    let events_text = match std::fs::read_to_string(&args[3]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read events {}: {e}", args[3]);
            return ExitCode::from(1);
        }
    };
    let events = match parse_events(&events_text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("bad events: {e}");
            return ExitCode::from(1);
        }
    };

    let mut engine = Engine::new(cfg, mode);
    let report = engine.run(&events);

    println!("mode              {:?}", engine.mode());
    println!("ticks             {}", report.ticks);
    println!("promoted          {}", report.promoted);
    println!("admitted          {}", report.admitted);
    println!("rejected          {}", report.rejected);
    println!("net_lamports      {}", report.net_lamports);
    for (lane, net) in report.per_lane_net {
        println!("  lane {lane:?}: net {net}");
    }
    for (lane, w) in report.final_weights {
        println!("  weight {lane:?}: {w} bp");
    }
    println!("journal_digest    {:#018x}", report.journal_digest);

    ExitCode::SUCCESS
}

/// Parse the dependency-free event format (one event per line; `#` comments).
fn parse_events(text: &str) -> Result<Vec<AppEvent>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let err = |m: &str| format!("line {}: {m}", i + 1);
        let mint = |s: &str| DomainMint::from_hex(s).map_err(|e| err(&format!("bad mint: {e}")));
        let num = |s: &str| s.parse::<i64>().map_err(|_| err("bad integer"));
        let ev = match f[0] {
            "tick" => AppEvent::Tick,
            "trade" if f.len() == 8 => AppEvent::MarketTrade {
                mint: mint(f[1])?,
                price_fp: f[2].parse::<i128>().map_err(|_| err("bad price_fp"))?,
                quote_lamports: num(f[3])?.max(0) as u64,
                liquidity_lamports: num(f[4])?.max(0) as u64,
                signed_base: num(f[5])?,
                buyer_entity: num(f[6])?.max(0) as u64,
                age_slots: num(f[7])?.max(0) as u32,
            },
            "narr" if f.len() == 4 => AppEvent::NarrativeSample {
                mint: mint(f[1])?,
                prior_active: num(f[2])?.max(0) as u64,
                new_mentions: num(f[3])?.max(0) as u64,
            },
            "social" if f.len() == 3 => AppEvent::SocialCall {
                mint: mint(f[1])?,
                source_quality_bp: num(f[2])?.max(0) as u32,
            },
            "wallet" if f.len() == 4 => AppEvent::WalletAction {
                mint: mint(f[1])?,
                followable: num(f[2])? != 0,
                size_lamports: num(f[3])?.max(0) as u64,
            },
            "confirm" if f.len() == 3 => AppEvent::OnchainConfirm {
                mint: mint(f[1])?,
                sellable_depth_lamports: num(f[2])?.max(0) as u64,
            },
            // Factual, on-chain-led category assignment (the classifier ran upstream;
            // the journal carries the resolved integer id, §85).
            "tokenmeta" if f.len() == 6 => AppEvent::TokenMetadata {
                mint: mint(f[1])?,
                category_id: num(f[2])?.max(0) as u64,
                taxonomy_version: num(f[3])?.max(0) as u32,
                creator: num(f[4])?.max(0) as u64,
                slot: num(f[5])?.max(0) as u64,
            },
            "creator_init" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::Init {
                    initial_tokens: num(f[2])?.max(0) as u64,
                    total_supply: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            "creator_buy" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::Buy {
                    tokens: num(f[2])?.max(0) as u64,
                    quote_lamports: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            "creator_sell" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::Sell {
                    tokens: num(f[2])?.max(0) as u64,
                    quote_lamports: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            "creator_link" if f.len() == 5 => AppEvent::CreatorAction {
                mint: mint(f[1])?,
                kind: CreatorActionKind::LinkedBuy {
                    cluster: num(f[2])?.max(0) as u64,
                    tokens: num(f[3])?.max(0) as u64,
                },
                slot: num(f[4])?.max(0) as u64,
            },
            other => return Err(err(&format!("unknown or malformed event '{other}'"))),
        };
        out.push(ev);
    }
    Ok(out)
}
