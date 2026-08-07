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

// ─── EventStreamReader ──────────────────────────────────────────────────────
// Phase 3: the reader side of the event stream. The writer has been capturing
// raw AppEvents since the daemon first ran; the reader loads them back into
// `Vec<AppEvent>` for config-driven engine replay. Different configs produce
// different admission/sizing/exit decisions against the SAME event stream —
// this is what lets the refiner differentiate challengers.

/// Read an event stream JSONL file back into a flat `Vec<AppEvent>`.
///
/// The slot field is discarded (the engine re-derives slot ordering from the
/// event sequence itself; the stream is strictly append-ordered).
/// Malformed lines are skipped (fail-soft) but counted in the return.
pub fn read_event_stream<P: AsRef<Path>>(path: P) -> io::Result<(Vec<AppEvent>, usize)> {
    let text = fs::read_to_string(path)?;
    let mut events = Vec::new();
    let mut skipped = 0usize;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match parse_event_line(line) {
            Ok(evt) => events.push(evt),
            Err(_) => {
                skipped += 1;
            }
        }
    }

    Ok((events, skipped))
}

/// Parse one JSONL line into an `AppEvent`. The format is:
/// `{"slot":N,"kind":"<Kind>","mint":"<base58>","fields":{...}}`
fn parse_event_line(line: &str) -> Result<AppEvent, String> {
    // Minimal JSON parsing — we control the writer format, so we can parse
    // the known structure without a full JSON crate dependency.
    let kind = extract_string_field(line, "kind").ok_or("missing kind")?;

    match kind.as_str() {
        "Tick" => Ok(AppEvent::Tick),
        "MarketTrade" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::MarketTrade {
                mint,
                price_fp: extract_int_field(line, "price_fp")
                    .ok_or("missing price_fp")? as i128,
                quote_lamports: extract_int_field(line, "quote_lamports")
                    .ok_or("missing quote_lamports")? as u64,
                liquidity_lamports: extract_int_field(line, "liquidity_lamports")
                    .ok_or("missing liquidity_lamports")? as u64,
                signed_base: extract_int_field(line, "signed_base")
                    .ok_or("missing signed_base")? as i64,
                buyer_entity: extract_int_field(line, "buyer_entity")
                    .ok_or("missing buyer_entity")? as u64,
                age_slots: extract_int_field(line, "age_slots")
                    .ok_or("missing age_slots")? as u32,
            })
        }
        "OnchainConfirm" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::OnchainConfirm {
                mint,
                virtual_sol_lamports: extract_int_field(line, "virtual_sol_lamports")
                    .ok_or("missing virtual_sol_lamports")? as u64,
                real_sol_lamports: extract_int_field(line, "real_sol_lamports")
                    .ok_or("missing real_sol_lamports")? as u64,
            })
        }
        "NarrativeSample" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::NarrativeSample {
                mint,
                prior_active: extract_int_field(line, "prior_active")
                    .ok_or("missing prior_active")? as u64,
                new_mentions: extract_int_field(line, "new_mentions")
                    .ok_or("missing new_mentions")? as u64,
            })
        }
        "SocialCall" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::SocialCall {
                mint,
                source_quality_bp: extract_int_field(line, "source_quality_bp")
                    .ok_or("missing source_quality_bp")? as u32,
            })
        }
        "WalletAction" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::WalletAction {
                mint,
                followable: extract_int_field(line, "followable")
                    .ok_or("missing followable")? != 0,
                size_lamports: extract_int_field(line, "size_lamports")
                    .ok_or("missing size_lamports")? as u64,
            })
        }
        "TokenMetadata" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::TokenMetadata {
                mint,
                category_id: extract_int_field(line, "category_id")
                    .ok_or("missing category_id")? as u64,
                taxonomy_version: extract_int_field(line, "taxonomy_version")
                    .ok_or("missing taxonomy_version")? as u32,
                creator: extract_int_field(line, "creator")
                    .ok_or("missing creator")? as u64,
                slot: extract_int_field(line, "metadata_slot")
                    .ok_or("missing metadata_slot")? as u64,
            })
        }
        "CreatorAction" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            let slot = extract_int_field(line, "action_slot").ok_or("missing action_slot")? as u64;
            // Parse the creator action kind from the nested JSON fragment.
            let kind_str = extract_creator_action_kind(line)?;
            let kind = match kind_str.as_str() {
                "creator_init" => {
                    CreatorActionKind::Init {
                        initial_tokens: extract_nested_int(line, "initial_tokens")
                            .ok_or("missing initial_tokens")? as u64,
                        total_supply: extract_nested_int(line, "total_supply")
                            .ok_or("missing total_supply")? as u64,
                    }
                }
                "creator_buy" => {
                    CreatorActionKind::Buy {
                        tokens: extract_nested_int(line, "tokens")
                            .ok_or("missing tokens")? as u64,
                        quote_lamports: extract_nested_int(line, "quote_lamports")
                            .ok_or("missing quote_lamports")? as u64,
                    }
                }
                "creator_sell" => {
                    CreatorActionKind::Sell {
                        tokens: extract_nested_int(line, "tokens")
                            .ok_or("missing tokens")? as u64,
                        quote_lamports: extract_nested_int(line, "quote_lamports")
                            .ok_or("missing quote_lamports")? as u64,
                    }
                }
                "creator_linked_buy" => {
                    CreatorActionKind::LinkedBuy {
                        cluster: extract_nested_int(line, "cluster")
                            .ok_or("missing cluster")? as u64,
                        tokens: extract_nested_int(line, "tokens")
                            .ok_or("missing tokens")? as u64,
                    }
                }
                _ => return Err(format!("unknown creator action kind: {kind_str}")),
            };
            Ok(AppEvent::CreatorAction { mint, kind, slot })
        }
        "Migration" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::Migration {
                mint,
                slot: extract_int_field(line, "migration_slot")
                    .ok_or("missing migration_slot")? as u64,
            })
        }
        other => Err(format!("unknown event kind: {other}")),
    }
}

/// Extract a string field value from a JSON line: `"field":"value"`.
fn extract_string_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    // Find the closing quote.
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract an integer field value from a JSON line: `"field":N`.
fn extract_int_field(line: &str, field: &str) -> Option<i64> {
    let needle = format!("\"{field}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    // Read until we hit a non-digit, non-minus character.
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    let num_str = &rest[..end];
    num_str.parse().ok()
}

/// Extract a nested integer from a JSON fragment like `"creator_init":{"initial_tokens":N,...}`.
fn extract_nested_int(line: &str, field: &str) -> Option<i64> {
    let needle = format!("\"{field}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    let num_str = &rest[..end];
    num_str.parse().ok()
}

/// Extract the creator action kind key from the nested JSON.
fn extract_creator_action_kind(line: &str) -> Result<String, String> {
    // Look for one of the known kind keys.
    for kind in &[
        "creator_init",
        "creator_buy",
        "creator_sell",
        "creator_linked_buy",
    ] {
        let needle = format!(r#""{kind}":"#);
        if line.contains(&needle) {
            return Ok(kind.to_string());
        }
    }
    Err("no creator action kind found".to_string())
}

/// Parse a base58-encoded mint string into a `Mint`.
fn parse_mint(s: &str) -> Result<Mint, String> {
    use solana_program::pubkey::Pubkey;
    let pk = s
        .parse::<Pubkey>()
        .map_err(|e| format!("invalid pubkey: {e}"))?;
    Ok(Mint::from_bytes(pk.to_bytes()))
}

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

    // ─── Phase 3: EventStreamReader round-trip tests ──────────────────────

    /// Write events, read them back, verify all fields survive the round-trip.
    #[test]
    fn read_back_market_trade_round_trips() {
        // First, verify the parser works on a known-good line
        let test_line = r#"{"slot":12345,"kind":"MarketTrade","mint":"US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx","fields":{"price_fp":1234567890,"quote_lamports":500000,"liquidity_lamports":1000000000,"signed_base":-50000,"buyer_entity":42,"age_slots":100}}"#;
        match parse_event_line(test_line) {
            Ok(evt) => match evt {
                AppEvent::MarketTrade { price_fp, .. } => {
                    assert_eq!(price_fp, 1_234_567_890, "price_fp must round-trip");
                }
                _ => panic!("expected MarketTrade, got something else"),
            },
            Err(e) => panic!("parse failed on known-good line: {e}"),
        }

        // Now write and read back
        let tmp = std::env::temp_dir().join("pq_event_stream_readback_test.jsonl");
        let _ = fs::remove_file(&tmp);
        let mut writer = EventStreamWriter::open(&tmp).expect("open");
        let mint = Mint([7u8; 32]);
        let event = AppEvent::MarketTrade {
            mint,
            price_fp: 1_234_567_890,
            quote_lamports: 500_000,
            liquidity_lamports: 1_000_000_000,
            signed_base: -50_000,
            buyer_entity: 42,
            age_slots: 100,
        };
        writer.write_event(&event, 12345).expect("write");
        writer.flush().expect("flush");
        drop(writer);

        let (events, skipped) = read_event_stream(&tmp).expect("read");
        assert_eq!(skipped, 0, "no lines should be skipped");
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::MarketTrade {
                price_fp,
                quote_lamports,
                liquidity_lamports,
                signed_base,
                buyer_entity,
                age_slots,
                ..
            } => {
                assert_eq!(*price_fp, 1_234_567_890);
                assert_eq!(*quote_lamports, 500_000);
                assert_eq!(*liquidity_lamports, 1_000_000_000);
                assert_eq!(*signed_base, -50_000);
                assert_eq!(*buyer_entity, 42);
                assert_eq!(*age_slots, 100);
            }
            _ => panic!("expected MarketTrade"),
        }
        let _ = fs::remove_file(&tmp);
    }

    /// Write multiple event types, read them back, verify the sequence.
    #[test]
    fn read_back_mixed_event_types() {
        let tmp = std::env::temp_dir().join("pq_event_stream_mixed_test.jsonl");
        let _ = fs::remove_file(&tmp);
        let mut writer = EventStreamWriter::open(&tmp).expect("open");
        let mint = Mint([3u8; 32]);

        // Write a Tick, a MarketTrade, an OnchainConfirm, and another Tick.
        writer.write_event(&AppEvent::Tick, 1).expect("write");
        writer.write_event(
            &AppEvent::MarketTrade {
                mint,
                price_fp: 2_000_000_000,
                quote_lamports: 100_000,
                liquidity_lamports: 500_000_000,
                signed_base: 10_000,
                buyer_entity: 5,
                age_slots: 20,
            },
            2,
        )
        .expect("write");
        writer.write_event(
            &AppEvent::OnchainConfirm {
                mint,
                virtual_sol_lamports: 80_000_000_000,
                real_sol_lamports: 30_000_000_000,
            },
            3,
        )
        .expect("write");
        writer.write_event(&AppEvent::Tick, 4).expect("write");
        writer.flush().expect("flush");
        drop(writer);

        let (events, skipped) = read_event_stream(&tmp).expect("read");
        assert_eq!(skipped, 0);
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], AppEvent::Tick));
        assert!(matches!(events[1], AppEvent::MarketTrade { .. }));
        assert!(matches!(events[2], AppEvent::OnchainConfirm { .. }));
        assert!(matches!(events[3], AppEvent::Tick));
        let _ = fs::remove_file(&tmp);
    }

    /// An empty file reads back as zero events, zero skipped.
    #[test]
    fn empty_file_reads_as_zero_events() {
        let tmp = std::env::temp_dir().join("pq_event_stream_empty_test.jsonl");
        let _ = fs::remove_file(&tmp);
        fs::write(&tmp, "").expect("write empty");
        let (events, skipped) = read_event_stream(&tmp).expect("read");
        assert_eq!(events.len(), 0);
        assert_eq!(skipped, 0);
        let _ = fs::remove_file(&tmp);
    }

    /// Malformed lines are skipped (fail-soft), valid lines are kept.
    #[test]
    fn malformed_lines_are_skipped() {
        let tmp = std::env::temp_dir().join("pq_event_stream_malformed_test.jsonl");
        let _ = fs::remove_file(&tmp);
        // One valid Tick line + two garbage lines + one valid Tick line.
        let content = format!(
            r#"{{"slot":1,"kind":"Tick"}}
garbage line 1
garbage line 2
{{"slot":2,"kind":"Tick"}}"#,
        );
        fs::write(&tmp, &content).expect("write");
        let (events, skipped) = read_event_stream(&tmp).expect("read");
        assert_eq!(events.len(), 2, "two valid Tick events");
        assert_eq!(skipped, 2, "two malformed lines skipped");
        let _ = fs::remove_file(&tmp);
    }
}
