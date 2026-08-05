//! `event_stream` — capture raw AppEvents for deterministic replay.
//!
//! The daemon writes each AppEvent to `data/event_stream.jsonl` so the
//! replay engine can re-execute the engine with mutated configs without
//! needing live network feeds. Each line is a compact JSON object with
//! the event kind, the slot it was processed at, and the key fields.
//!
//! Constitution: §13 (paper/live parity), §16 (no look-ahead), §22 (integer-only).
//! The event stream is the raw input — replaying it deterministically
//! guarantees that any config mutation is tested against identical input.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use pump_quant_app::event::AppEvent;
use pump_quant_app::event::CreatorActionKind;
use pump_quant_domain::ids::Mint;

/// Compact JSON line writer for the event stream.
pub struct EventStreamWriter {
    writer: BufWriter<std::fs::File>,
    events_written: u64,
}

impl EventStreamWriter {
    /// Open (or create) an event stream file at `path`. The file is opened
    /// in append mode so restarts continue from where the last session left off.
    /// Returns `None` if the file cannot be opened (fail-safe: daemon continues
    /// without event capture).
    pub fn open<P: AsRef<Path>>(path: P) -> Option<Self> {
        // Ensure parent dir exists.
        if let Some(parent) = path.as_ref().parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Self {
            writer: BufWriter::with_capacity(64 * 1024, file),
            events_written: 0,
        })
    }

    /// Write one event as a compact JSON line.
    ///
    /// Format: `{"slot":N,"kind":"MarketTrade","mint":"<base58>","fields":{...}}\n`
    /// All values are integers or quoted strings. No floats (§22).
    pub fn write_event(&mut self, event: &AppEvent, slot: u64) -> io::Result<()> {
        let json = event_to_json(event, slot);
        self.writer.write_all(json.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.events_written += 1;
        Ok(())
    }

    /// Flush buffered writes to disk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    /// Number of events written since open.
    #[must_use]
    pub fn events_written(&self) -> u64 {
        self.events_written
    }
}

/// Encode an AppEvent into a compact JSON string (no trailing newline).
/// All numeric values are integers. Mint addresses are base58-encoded.
fn event_to_json(event: &AppEvent, slot: u64) -> String {
    let kind = event_kind(event);
    let mint_b58 = event_mint(event).map(|m| mint_to_base58(&m));

    let mut out = String::with_capacity(256);
    out.push('{');
    out.push_str(&format!(r#""slot":{}"#, slot));
    out.push_str(&format!(r#","kind":"{}""#, kind));
    if let Some(m) = &mint_b58 {
        out.push_str(&format!(r#","mint":"{}""#, m));
    }
    // Append key fields based on the event variant.
    let fields = event_fields_json(event);
    if !fields.is_empty() {
        out.push_str(&format!(r#","fields":{{{}}}"#, fields));
    }
    out.push('}');
    out
}

/// Get the kind name for an AppEvent.
fn event_kind(event: &AppEvent) -> &'static str {
    match event {
        AppEvent::MarketTrade { .. } => "MarketTrade",
        AppEvent::NarrativeSample { .. } => "NarrativeSample",
        AppEvent::SocialCall { .. } => "SocialCall",
        AppEvent::WalletAction { .. } => "WalletAction",
        AppEvent::OnchainConfirm { .. } => "OnchainConfirm",
        AppEvent::TokenMetadata { .. } => "TokenMetadata",
        AppEvent::CreatorAction { .. } => "CreatorAction",
        AppEvent::Migration { .. } => "Migration",
        AppEvent::Tick => "Tick",
    }
}

/// Extract the mint from an event (if it has one).
fn event_mint(event: &AppEvent) -> Option<Mint> {
    event.mint()
}

/// Extract key fields as JSON key-value pairs (without surrounding braces).
/// Only the most important fields for replay are captured — the replay
/// engine re-derives the rest from the engine's internal state.
fn event_fields_json(event: &AppEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    match event {
        AppEvent::MarketTrade {
            price_fp, quote_lamports, liquidity_lamports,
            signed_base, buyer_entity, age_slots, ..
        } => {
            parts.push(format!(r#""price_fp":{}"#, price_fp));
            parts.push(format!(r#""quote_lamports":{}"#, quote_lamports));
            parts.push(format!(r#""liquidity_lamports":{}"#, liquidity_lamports));
            parts.push(format!(r#""signed_base":{}"#, signed_base));
            parts.push(format!(r#""buyer_entity":{}"#, buyer_entity));
            parts.push(format!(r#""age_slots":{}"#, age_slots));
        }
        AppEvent::NarrativeSample { prior_active, new_mentions, .. } => {
            parts.push(format!(r#""prior_active":{}"#, prior_active));
            parts.push(format!(r#""new_mentions":{}"#, new_mentions));
        }
        AppEvent::SocialCall { source_quality_bp, .. } => {
            parts.push(format!(r#""source_quality_bp":{}"#, source_quality_bp));
        }
        AppEvent::WalletAction { followable, size_lamports, .. } => {
            parts.push(format!(r#""followable":{}"#, followable));
            parts.push(format!(r#""size_lamports":{}"#, size_lamports));
        }
        AppEvent::OnchainConfirm { virtual_sol_lamports, real_sol_lamports, .. } => {
            parts.push(format!(r#""virtual_sol_lamports":{}"#, virtual_sol_lamports));
            parts.push(format!(r#""real_sol_lamports":{}"#, real_sol_lamports));
        }
        AppEvent::TokenMetadata { category_id, taxonomy_version, creator, slot, .. } => {
            parts.push(format!(r#""category_id":{}"#, category_id));
            parts.push(format!(r#""taxonomy_version":{}"#, taxonomy_version));
            parts.push(format!(r#""creator":{}"#, creator));
            parts.push(format!(r#""metadata_slot":{}"#, slot));
        }
        AppEvent::CreatorAction { kind, slot, .. } => {
            parts.push(format!(r#""action_slot":{}"#, slot));
            parts.push(creator_action_kind_json(kind));
        }
        AppEvent::Migration { slot, .. } => {
            parts.push(format!(r#""migration_slot":{}"#, slot));
        }
        AppEvent::Tick => {}
    }
    parts.join(",")
}

/// Encode a CreatorActionKind as a JSON key-value pair.
fn creator_action_kind_json(kind: &CreatorActionKind) -> String {
    match kind {
        CreatorActionKind::Init { initial_tokens, total_supply } => {
            format!(r#""creator_init":{{"initial_tokens":{},"total_supply":{}}}"#, initial_tokens, total_supply)
        }
        CreatorActionKind::Buy { tokens, quote_lamports } => {
            format!(r#""creator_buy":{{"tokens":{},"quote_lamports":{}}}"#, tokens, quote_lamports)
        }
        CreatorActionKind::Sell { tokens, quote_lamports } => {
            format!(r#""creator_sell":{{"tokens":{},"quote_lamports":{}}}"#, tokens, quote_lamports)
        }
        CreatorActionKind::LinkedBuy { cluster, tokens } => {
            format!(r#""creator_linked_buy":{{"cluster":{},"tokens":{}}}"#, cluster, tokens)
        }
    }
}

/// Encode a Mint as a base58 string (Solana canonical format).
fn mint_to_base58(mint: &Mint) -> String {
    use solana_program::pubkey::Pubkey;
    Pubkey::from(*mint.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_domain::ids::Mint;
    use pump_quant_app::event::AppEvent;
    use std::fs;

    #[test]
    fn write_and_read_event_stream() {
        let tmp = std::env::temp_dir().join("pq_event_stream_test.jsonl");
        let _ = fs::remove_file(&tmp);
        let mut writer = EventStreamWriter::open(&tmp).expect("open");
        let mint = Mint([1u8; 32]);
        let event = AppEvent::MarketTrade {
            mint,
            price_fp: 1_000_000_000,
            quote_lamports: 50_000,
            liquidity_lamports: 1_000_000,
            signed_base: 50_000,
            buyer_entity: 42,
            age_slots: 100,
        };
        writer.write_event(&event, 12345).expect("write");
        writer.flush().expect("flush");
        drop(writer);
        let content = fs::read_to_string(&tmp).expect("read");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(r#""kind":"MarketTrade""#));
        assert!(lines[0].contains(r#""slot":12345"#));
        assert!(lines[0].contains(r#""price_fp":1000000000"#));
        assert!(lines[0].contains(r#""buyer_entity":42"#));
        assert!(lines[0].contains(r#""age_slots":100"#));
        assert!(lines[0].contains(r#""mint":"#));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn event_count_increments() {
        let tmp = std::env::temp_dir().join("pq_event_stream_count_test.jsonl");
        let _ = fs::remove_file(&tmp);
        let mut writer = EventStreamWriter::open(&tmp).expect("open");
        let event = AppEvent::Tick;
        for _ in 0..10 {
            writer.write_event(&event, 1).expect("write");
        }
        assert_eq!(writer.events_written(), 10);
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn append_mode_preserves_existing() {
        let tmp = std::env::temp_dir().join("pq_event_stream_append_test.jsonl");
        let _ = fs::remove_file(&tmp);
        // Write 3 events.
        {
            let mut writer = EventStreamWriter::open(&tmp).expect("open");
            let event = AppEvent::Tick;
            for _ in 0..3 {
                writer.write_event(&event, 1).expect("write");
            }
            writer.flush().expect("flush");
        }
        // Re-open and write 2 more — should append, not truncate.
        {
            let mut writer = EventStreamWriter::open(&tmp).expect("open");
            let event = AppEvent::Tick;
            for _ in 0..2 {
                writer.write_event(&event, 2).expect("write");
            }
            writer.flush().expect("flush");
        }
        let content = fs::read_to_string(&tmp).expect("read");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5, "append mode should preserve existing events");
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn tick_event_has_no_mint() {
        let tmp = std::env::temp_dir().join("pq_event_stream_tick_test.jsonl");
        let _ = fs::remove_file(&tmp);
        let mut writer = EventStreamWriter::open(&tmp).expect("open");
        writer.write_event(&AppEvent::Tick, 99).expect("write");
        writer.flush().expect("flush");
        drop(writer);
        let content = fs::read_to_string(&tmp).expect("read");
        assert!(content.contains(r#""kind":"Tick""#));
        assert!(content.contains(r#""slot":99"#));
        assert!(!content.contains(r#""mint""#));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn onchain_confirm_serializes_reserves() {
        let tmp = std::env::temp_dir().join("pq_event_stream_confirm_test.jsonl");
        let _ = fs::remove_file(&tmp);
        let mut writer = EventStreamWriter::open(&tmp).expect("open");
        let mint = Mint([2u8; 32]);
        let event = AppEvent::OnchainConfirm {
            mint,
            virtual_sol_lamports: 30_000_000_000,
            real_sol_lamports: 5_000_000_000,
        };
        writer.write_event(&event, 200).expect("write");
        writer.flush().expect("flush");
        drop(writer);
        let content = fs::read_to_string(&tmp).expect("read");
        assert!(content.contains(r#""kind":"OnchainConfirm""#));
        assert!(content.contains(r#""virtual_sol_lamports":30000000000"#));
        assert!(content.contains(r#""real_sol_lamports":5000000000"#));
        let _ = fs::remove_file(&tmp);
    }
}
