//! Dependency-free event-format parser (one event per line; `#` comments).
//!
//! This module is the SOLE implementation of the text-event parser. The binary
//! in `main.rs` re-exports it as `parse_events` for its existing call site; the
//! junction wire-up binary calls it via `pump_quant_app::parse::parse_events`.
//! Both callers get the exact same function — the golden digest is a property
//! of this parser's output, so having one implementation is the correctness
//! guarantee (§54).

use crate::event::{AppEvent, CreatorActionKind};
use pump_quant_domain::ids::Mint as DomainMint;

/// Parse the dependency-free event format (one event per line; `#` comments).
pub fn parse_events(text: &str) -> Result<Vec<AppEvent>, String> {
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
            "migrate" if f.len() == 3 => AppEvent::Migration {
                mint: mint(f[1])?,
                slot: num(f[2])?.max(0) as u64,
            },
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
            // `confirm <mint> <virtual_sol> <real_sol>` — ONE decode of the curve
            // account, both SOL-side reserves. The 3-field form is gone: a single
            // "depth" number could not say which reserve it was, which is exactly
            // how a 30x capacity overstatement survived
            // (`docs/DEPTH_AND_MOVE_PROVENANCE_PLAN_2026-07-28.md`).
            "confirm" if f.len() == 4 => AppEvent::OnchainConfirm {
                mint: mint(f[1])?,
                virtual_sol_lamports: num(f[2])?.max(0) as u64,
                real_sol_lamports: num(f[3])?.max(0) as u64,
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
