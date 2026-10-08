//! `event_stream` — capture raw AppEvents for deterministic replay.
//!
//! The daemon writes each AppEvent to `data/event_stream.jsonl` so the
//! replay engine can re-execute the engine with mutated configs without
//! needing live network feeds. Each line is a compact JSON object with
//! the event kind, the slot it was processed at, and the key fields.
//!
//! Operator: §13 (paper/live parity), §16 (no look-ahead), §22 (integer-only).
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
                #[allow(clippy::arithmetic_side_effects)]
                // LINT-ALLOW(hot_arith): u64 skipped counter
                {
                    skipped += 1;
                }
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
                price_fp: extract_int_field(line, "price_fp").ok_or("missing price_fp")? as i128,
                quote_lamports: u64_field(
                    extract_int_field(line, "quote_lamports"),
                    "quote_lamports",
                )?,
                liquidity_lamports: u64_field(
                    extract_int_field(line, "liquidity_lamports"),
                    "liquidity_lamports",
                )?,
                signed_base: extract_int_field(line, "signed_base").ok_or("missing signed_base")?,
                buyer_entity: u64_field(extract_int_field(line, "buyer_entity"), "buyer_entity")?,
                age_slots: u32::try_from(
                    extract_int_field(line, "age_slots").ok_or("missing age_slots")?,
                )
                .map_err(|_| "age_slots out of range")?,
                // OPTIONAL by design: every tape written before this field existed stays
                // readable, and `read_event_stream` keeps skipping only genuinely malformed
                // lines. Making this key REQUIRED would turn every legacy tape into a stream
                // of dropped trades that still reports success.
                recv_unix_ms: extract_int_field(line, "recv_unix_ms"),
                // Also optional, and also never guessed: a tape without the address simply has
                // no address, and the address-keyed derivations refuse rather than hash.
                trader_pubkey: extract_string_field(line, "trader_pubkey")
                    .and_then(|s| parse_mint(&s).ok())
                    .map(|m| *m.as_bytes()),
                slot: None,
                fee_lamports: None,
                cu_consumed: None,
                venue: None,
                event_id: None,
                feature: None,
            })
        }
        "OnchainConfirm" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::OnchainConfirm {
                mint,
                virtual_sol_lamports: u64_field(
                    extract_int_field(line, "virtual_sol_lamports"),
                    "virtual_sol_lamports",
                )?,
                real_sol_lamports: u64_field(
                    extract_int_field(line, "real_sol_lamports"),
                    "real_sol_lamports",
                )?,
            })
        }
        "NarrativeSample" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::NarrativeSample {
                mint,
                prior_active: u64_field(extract_int_field(line, "prior_active"), "prior_active")?,
                new_mentions: u64_field(extract_int_field(line, "new_mentions"), "new_mentions")?,
            })
        }
        "SocialCall" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::SocialCall {
                mint,
                source_quality_bp: u32::try_from(
                    extract_int_field(line, "source_quality_bp")
                        .ok_or("missing source_quality_bp")?,
                )
                .map_err(|_| "source_quality_bp out of range")?,
            })
        }
        "WalletAction" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::WalletAction {
                mint,
                followable: extract_int_field(line, "followable").ok_or("missing followable")? != 0,
                size_lamports: u64_field(
                    extract_int_field(line, "size_lamports"),
                    "size_lamports",
                )?,
            })
        }
        "TokenMetadata" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::TokenMetadata {
                mint,
                category_id: u64_field(extract_int_field(line, "category_id"), "category_id")?,
                taxonomy_version: u32::try_from(
                    extract_int_field(line, "taxonomy_version")
                        .ok_or("missing taxonomy_version")?,
                )
                .map_err(|_| "taxonomy_version out of range")?,
                creator: u64_field(extract_int_field(line, "creator"), "creator")?,
                slot: u64_field(extract_int_field(line, "metadata_slot"), "metadata_slot")?,
            })
        }
        "CreatorAction" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            let slot = u64_field(extract_int_field(line, "action_slot"), "action_slot")?;
            // Parse the creator action kind from the nested JSON fragment.
            let kind_str = extract_creator_action_kind(line)?;
            let kind = match kind_str.as_str() {
                "creator_init" => CreatorActionKind::Init {
                    initial_tokens: u64_field(
                        extract_nested_int(line, "initial_tokens"),
                        "initial_tokens",
                    )?,
                    total_supply: u64_field(
                        extract_nested_int(line, "total_supply"),
                        "total_supply",
                    )?,
                },
                "creator_buy" => CreatorActionKind::Buy {
                    tokens: u64_field(extract_nested_int(line, "tokens"), "tokens")?,
                    quote_lamports: u64_field(
                        extract_nested_int(line, "quote_lamports"),
                        "quote_lamports",
                    )?,
                },
                "creator_sell" => CreatorActionKind::Sell {
                    tokens: u64_field(extract_nested_int(line, "tokens"), "tokens")?,
                    quote_lamports: u64_field(
                        extract_nested_int(line, "quote_lamports"),
                        "quote_lamports",
                    )?,
                },
                "creator_linked_buy" => CreatorActionKind::LinkedBuy {
                    cluster: u64_field(extract_nested_int(line, "cluster"), "cluster")?,
                    tokens: u64_field(extract_nested_int(line, "tokens"), "tokens")?,
                },
                _ => return Err(format!("unknown creator action kind: {kind_str}")),
            };
            Ok(AppEvent::CreatorAction { mint, kind, slot })
        }
        "Migration" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::Migration {
                mint,
                slot: u64_field(extract_int_field(line, "migration_slot"), "migration_slot")?,
            })
        }
        // Rev-14 wangr intelligence: parse auxiliary + time signal events.
        "MarketAuxiliary" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::MarketAuxiliary {
                mint,
                token_standard: u8::try_from(
                    extract_int_field(line, "token_standard").ok_or("missing token_standard")?,
                )
                .map_err(|_| "token_standard out of range")?,
                symbol_len: u8::try_from(
                    extract_int_field(line, "symbol_len").ok_or("missing symbol_len")?,
                )
                .map_err(|_| "symbol_len out of range")?,
            })
        }
        "TimeSignal" => Ok(AppEvent::TimeSignal {
            dow: u8::try_from(extract_int_field(line, "dow").ok_or("missing dow")?)
                .map_err(|_| "dow out of range")?,
            hour_utc: u8::try_from(extract_int_field(line, "hour_utc").ok_or("missing hour_utc")?)
                .map_err(|_| "hour_utc out of range")?,
        }),
        // Narrative precondition (operator ruling 2026-09-26): parse the resolved
        // narrative verdict so a replay reproduces the gate's decision exactly.
        "NarrativeResolved" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            Ok(AppEvent::NarrativeResolved {
                mint,
                verdict: u8::try_from(extract_int_field(line, "verdict").ok_or("missing verdict")?)
                    .map_err(|_| "verdict out of range")?,
                stage: u8::try_from(extract_int_field(line, "stage").ok_or("missing stage")?)
                    .map_err(|_| "stage out of range")?,
                family: u8::try_from(extract_int_field(line, "family").ok_or("missing family")?)
                    .map_err(|_| "family out of range")?,
                lexicon_version: u32::try_from(
                    extract_int_field(line, "lexicon_version").ok_or("missing lexicon_version")?,
                )
                .map_err(|_| "lexicon_version out of range")?,
            })
        }
        // Rev-19 on-chain feedback: parse confirmation events.
        "OurBuyConfirmed" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            let sig_str = extract_string_field(line, "signature").ok_or("missing signature")?;
            let signature = hex_to_sig(&sig_str)?;
            Ok(AppEvent::OurBuyConfirmed {
                mint,
                signature,
                slot: u64_field(extract_int_field(line, "confirm_slot"), "confirm_slot")?,
            })
        }
        "OurBuyFailed" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            let sig_str = extract_string_field(line, "signature").ok_or("missing signature")?;
            let signature = hex_to_sig(&sig_str)?;
            Ok(AppEvent::OurBuyFailed {
                mint,
                signature,
                err_code: u8::try_from(
                    extract_int_field(line, "err_code").ok_or("missing err_code")?,
                )
                .map_err(|_| "err_code out of range")?,
                slot: u64_field(extract_int_field(line, "confirm_slot"), "confirm_slot")?,
            })
        }
        "OurSellConfirmed" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            let sig_str = extract_string_field(line, "signature").ok_or("missing signature")?;
            let signature = hex_to_sig(&sig_str)?;
            Ok(AppEvent::OurSellConfirmed {
                mint,
                signature,
                slot: u64_field(extract_int_field(line, "confirm_slot"), "confirm_slot")?,
            })
        }
        "OurSellFailed" => {
            let mint_str = extract_string_field(line, "mint").ok_or("missing mint")?;
            let mint = parse_mint(&mint_str)?;
            let sig_str = extract_string_field(line, "signature").ok_or("missing signature")?;
            let signature = hex_to_sig(&sig_str)?;
            Ok(AppEvent::OurSellFailed {
                mint,
                signature,
                err_code: u8::try_from(
                    extract_int_field(line, "err_code").ok_or("missing err_code")?,
                )
                .map_err(|_| "err_code out of range")?,
                slot: u64_field(extract_int_field(line, "confirm_slot"), "confirm_slot")?,
            })
        }
        other => Err(format!("unknown event kind: {other}")),
    }
}

/// Extract a string field value from a JSON line: `"field":"value"`.
#[allow(clippy::arithmetic_side_effects)] // LINT-ALLOW(hot_arith,hot_cast): offset start+needle.len() from find()?, slicing <=len
fn extract_string_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    // Find the closing quote.
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Non-negative u64 from a tape field. Rejects a MISSING field and a NEGATIVE
/// value: `i64 -> u64` via `as` would wrap a negative to a huge u64, silently
/// corrupting a slot/lamport quantity. The field is externally supplied tape text,
/// so it is validated here rather than assumed non-negative.
fn u64_field(v: Option<i64>, key: &str) -> Result<u64, String> {
    let v = v.ok_or_else(|| format!("missing {key}"))?;
    u64::try_from(v).map_err(|_| format!("{key} negative: {v}"))
}

/// Extract an integer field value from a JSON line: `"field":N`.
#[allow(clippy::arithmetic_side_effects)] // LINT-ALLOW(hot_arith,hot_cast): offset start+needle.len() from find()?, <=len
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
#[allow(clippy::arithmetic_side_effects)] // LINT-ALLOW(hot_arith,hot_cast): offset start+needle.len() from find()?, <=len
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
        #[allow(clippy::arithmetic_side_effects)] // LINT-ALLOW(hot_arith): u64 events counter
        {
            self.events_written += 1;
        }
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
        AppEvent::CurveObserved { .. } => "CurveObserved",
        AppEvent::AmmSwap { .. } => "AmmSwap",
        AppEvent::CorpusFlowRow { .. } => "CorpusFlowRow",
        AppEvent::LaunchObserved { .. } => "LaunchObserved",
        AppEvent::TokenMetadata { .. } => "TokenMetadata",
        AppEvent::CreatorAction { .. } => "CreatorAction",
        AppEvent::Migration { .. } => "Migration",
        // Rev-14 wangr intelligence: new event variants.
        AppEvent::MarketAuxiliary { .. } => "MarketAuxiliary",
        AppEvent::TimeSignal { .. } => "TimeSignal",
        AppEvent::NarrativeResolved { .. } => "NarrativeResolved",
        // Rev-19 on-chain feedback: new event variants.
        AppEvent::ModelOrderEvidence { .. } => "ModelOrderEvidence",
        AppEvent::ModelMgmtReport { .. } => "ModelMgmtReport",
        AppEvent::OurBuyConfirmed { .. } => "OurBuyConfirmed",
        AppEvent::OurBuyFailed { .. } => "OurBuyFailed",
        AppEvent::OurSellConfirmed { .. } => "OurSellConfirmed",
        AppEvent::OurSellFailed { .. } => "OurSellFailed",
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
            price_fp,
            quote_lamports,
            liquidity_lamports,
            signed_base,
            buyer_entity,
            trader_pubkey,
            age_slots,
            recv_unix_ms,
            ..
        } => {
            parts.push(format!(r#""price_fp":{}"#, price_fp));
            parts.push(format!(r#""quote_lamports":{}"#, quote_lamports));
            parts.push(format!(r#""liquidity_lamports":{}"#, liquidity_lamports));
            parts.push(format!(r#""signed_base":{}"#, signed_base));
            parts.push(format!(r#""buyer_entity":{}"#, buyer_entity));
            parts.push(format!(r#""age_slots":{}"#, age_slots));
            // Written only when the wire carried it. An absent key reads back as `None`, which
            // is the fail-closed state — the alternative (a default) would make every old tape
            // look like it had a clock.
            if let Some(ms) = recv_unix_ms {
                parts.push(format!(r#""recv_unix_ms":{}"#, ms));
            }
            // Written only when the producer knew it. The address is what address-keyed
            // derivations (the flow reducer's freshness / smart-wallet / co-entry rules) need,
            // and it cannot be recovered from `buyer_entity`.
            if let Some(pk) = trader_pubkey {
                parts.push(format!(
                    r#""trader_pubkey":"{}""#,
                    mint_to_base58(&Mint(*pk))
                ));
            }
        }
        AppEvent::NarrativeSample {
            prior_active,
            new_mentions,
            ..
        } => {
            parts.push(format!(r#""prior_active":{}"#, prior_active));
            parts.push(format!(r#""new_mentions":{}"#, new_mentions));
        }
        AppEvent::SocialCall {
            source_quality_bp, ..
        } => {
            parts.push(format!(r#""source_quality_bp":{}"#, source_quality_bp));
        }
        AppEvent::WalletAction {
            followable,
            size_lamports,
            ..
        } => {
            parts.push(format!(r#""followable":{}"#, followable));
            parts.push(format!(r#""size_lamports":{}"#, size_lamports));
        }
        AppEvent::OnchainConfirm {
            virtual_sol_lamports,
            real_sol_lamports,
            ..
        } => {
            parts.push(format!(
                r#""virtual_sol_lamports":{}"#,
                virtual_sol_lamports
            ));
            parts.push(format!(r#""real_sol_lamports":{}"#, real_sol_lamports));
        }
        AppEvent::CurveObserved {
            v_sol_lamports,
            v_tokens,
            real_sol_lamports,
            real_tokens,
            recv_unix_ms,
            slot,
            ..
        } => {
            parts.push(format!(r#""v_sol_lamports":{}"#, v_sol_lamports));
            parts.push(format!(r#""v_tokens":{}"#, v_tokens));
            parts.push(format!(r#""real_sol_lamports":{}"#, real_sol_lamports));
            parts.push(format!(r#""real_tokens":{}"#, real_tokens));
            if let Some(t) = recv_unix_ms {
                parts.push(format!(r#""recv_unix_ms":{}"#, t));
            }
            parts.push(format!(r#""slot":{}"#, slot));
        }
        AppEvent::AmmSwap {
            token_reserve_pre,
            quote_reserve_pre,
            is_buy,
            token_amount,
            quote_lamports,
            recv_unix_ms,
            slot,
            ..
        } => {
            parts.push(format!(r#""token_reserve_pre":{}"#, token_reserve_pre));
            parts.push(format!(r#""quote_reserve_pre":{}"#, quote_reserve_pre));
            parts.push(format!(r#""is_buy":{}"#, is_buy));
            parts.push(format!(r#""token_amount":{}"#, token_amount));
            parts.push(format!(r#""quote_lamports":{}"#, quote_lamports));
            if let Some(t) = recv_unix_ms {
                parts.push(format!(r#""recv_unix_ms":{}"#, t));
            }
            parts.push(format!(r#""slot":{}"#, slot));
        }
        AppEvent::CorpusFlowRow {
            feature,
            recv_unix_ms,
            slot,
            event_id,
            ..
        } => {
            parts.push(format!(r#""sol_lamports":{}"#, feature.sol_lamports));
            parts.push(format!(r#""tokens_raw":{}"#, feature.tokens_raw));
            parts.push(format!(r#""event_id":"{:032x}""#, event_id));
            if let Some(t) = recv_unix_ms {
                parts.push(format!(r#""recv_unix_ms":{}"#, t));
            }
            if let Some(sl) = slot {
                parts.push(format!(r#""slot":{}"#, sl));
            }
        }
        AppEvent::LaunchObserved { launch_unix_ms, .. } => {
            parts.push(format!(r#""launch_unix_ms":{}"#, launch_unix_ms));
        }
        AppEvent::TokenMetadata {
            category_id,
            taxonomy_version,
            creator,
            slot,
            ..
        } => {
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
        // Rev-14 wangr intelligence: serialize auxiliary + time signals.
        AppEvent::MarketAuxiliary {
            token_standard,
            symbol_len,
            ..
        } => {
            parts.push(format!(r#""token_standard":{}"#, token_standard));
            parts.push(format!(r#""symbol_len":{}"#, symbol_len));
        }
        AppEvent::TimeSignal { dow, hour_utc } => {
            parts.push(format!(r#""dow":{}"#, dow));
            parts.push(format!(r#""hour_utc":{}"#, hour_utc));
        }
        AppEvent::NarrativeResolved {
            verdict,
            stage,
            family,
            lexicon_version,
            ..
        } => {
            parts.push(format!(r#""verdict":{}"#, verdict));
            parts.push(format!(r#""stage":{}"#, stage));
            parts.push(format!(r#""family":{}"#, family));
            parts.push(format!(r#""lexicon_version":{}"#, lexicon_version));
        }
        // Paper-model execution evidence: order identity + quantity + outcome (no signature exists).
        AppEvent::ModelOrderEvidence {
            order_id,
            attempt,
            clip_lamports,
            filled,
            ..
        } => {
            parts.push(format!(r#""order_id":{order_id}"#));
            parts.push(format!(r#""attempt":{attempt}"#));
            parts.push(format!(r#""clip_lamports":{clip_lamports}"#));
            match filled {
                Some((px, res)) => {
                    parts.push(format!(
                        r#""filled":true,"entry_price_fp":{px},"reserve_sol_lamports":{res}"#
                    ));
                }
                None => parts.push(r#""filled":false"#.to_string()),
            }
        }
        AppEvent::ModelMgmtReport {
            order_id,
            action,
            intended,
            cumulative_tokens,
            value,
            ..
        } => {
            parts.push(format!(r#""action":{action}"#));
            parts.push(format!(r#""intended":{intended}"#));
            parts.push(format!(r#""order_id":{order_id}"#));
            parts.push(format!(r#""cumulative_tokens":{cumulative_tokens}"#));
            parts.push(format!(r#""value":{value}"#));
        }
        // Rev-19 on-chain feedback: serialize signature + slot for confirmation events.
        AppEvent::OurBuyConfirmed {
            signature, slot, ..
        } => {
            parts.push(format!(r#""signature":"{}""#, sig_to_hex(signature)));
            parts.push(format!(r#""confirm_slot":{}"#, slot));
        }
        AppEvent::OurBuyFailed {
            signature,
            err_code,
            slot,
            ..
        } => {
            parts.push(format!(r#""signature":"{}""#, sig_to_hex(signature)));
            parts.push(format!(r#""err_code":{}"#, err_code));
            parts.push(format!(r#""confirm_slot":{}"#, slot));
        }
        AppEvent::OurSellConfirmed {
            signature, slot, ..
        } => {
            parts.push(format!(r#""signature":"{}""#, sig_to_hex(signature)));
            parts.push(format!(r#""confirm_slot":{}"#, slot));
        }
        AppEvent::OurSellFailed {
            signature,
            err_code,
            slot,
            ..
        } => {
            parts.push(format!(r#""signature":"{}""#, sig_to_hex(signature)));
            parts.push(format!(r#""err_code":{}"#, err_code));
            parts.push(format!(r#""confirm_slot":{}"#, slot));
        }
        AppEvent::Tick => {}
    }
    parts.join(",")
}

/// Encode a CreatorActionKind as a JSON key-value pair.
fn creator_action_kind_json(kind: &CreatorActionKind) -> String {
    match kind {
        CreatorActionKind::Init {
            initial_tokens,
            total_supply,
        } => {
            format!(
                r#""creator_init":{{"initial_tokens":{},"total_supply":{}}}"#,
                initial_tokens, total_supply
            )
        }
        CreatorActionKind::Buy {
            tokens,
            quote_lamports,
        } => {
            format!(
                r#""creator_buy":{{"tokens":{},"quote_lamports":{}}}"#,
                tokens, quote_lamports
            )
        }
        CreatorActionKind::Sell {
            tokens,
            quote_lamports,
        } => {
            format!(
                r#""creator_sell":{{"tokens":{},"quote_lamports":{}}}"#,
                tokens, quote_lamports
            )
        }
        CreatorActionKind::LinkedBuy { cluster, tokens } => {
            format!(
                r#""creator_linked_buy":{{"cluster":{},"tokens":{}}}"#,
                cluster, tokens
            )
        }
    }
}

/// Encode a Mint as a base58 string (Solana canonical format).
fn mint_to_base58(mint: &Mint) -> String {
    use solana_program::pubkey::Pubkey;
    Pubkey::from(*mint.as_bytes()).to_string()
}

/// **Rev-19**: Encode a 64-byte signature as a hex string (128 chars).
fn sig_to_hex(sig: &[u8; 64]) -> String {
    sig.iter().map(|b| format!("{:02x}", b)).collect()
}

/// **Rev-19**: Decode a hex string back into a 64-byte signature.
#[allow(clippy::arithmetic_side_effects)] // LINT-ALLOW(hot_arith,hot_cast): hex[i*2..i*2+2] over 0..64, i*2+2<=128=len of 64-byte sig
fn hex_to_sig(hex: &str) -> Result<[u8; 64], String> {
    if hex.len() != 128 {
        return Err(format!("signature hex length {} != 128", hex.len()));
    }
    let mut sig = [0u8; 64];
    for i in 0..64 {
        sig[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|e| format!("hex decode at byte {i}: {e}"))?;
    }
    Ok(sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_app::event::AppEvent;
    use pump_quant_domain::ids::Mint;
    use std::fs;
    /// A tape field outside its target type's range is REJECTED (checked conversion),
    /// not silently truncated.
    #[test]
    fn out_of_range_field_is_rejected_not_truncated() {
        let good = r#"{"slot":1,"kind":"MarketTrade","mint":"US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx","price_fp":1,"quote_lamports":1,"liquidity_lamports":1,"signed_base":1,"buyer_entity":1,"age_slots":5}"#;
        assert!(parse_event_line(good).is_ok(), "in-range age_slots parses");
        let bad = good.replace("\"age_slots\":5", "\"age_slots\":4294967296");
        assert!(
            parse_event_line(&bad).is_err(),
            "age_slots = 2^32 exceeds u32 and must be rejected"
        );
    }

    /// A NEGATIVE tape value for a u64 field is rejected rather than wrapping to a huge
    /// u64 via `as` (the enabled lint set does not flag i64 -> u64 sign loss).
    #[test]
    fn negative_u64_field_is_rejected_not_wrapped() {
        let good = r#"{"slot":1,"kind":"MarketTrade","mint":"US517G5965aydkZ46HS38QLi7UQiSojurfbQfKCELFx","price_fp":1,"quote_lamports":1,"liquidity_lamports":1,"signed_base":1,"buyer_entity":1,"age_slots":5}"#;
        let bad = good.replace("\"quote_lamports\":1", "\"quote_lamports\":-5");
        let err = parse_event_line(&bad).expect_err("negative quote_lamports must be rejected");
        assert!(err.contains("negative"), "error names the cause: {err}");
        let bad_slot = good.replace("\"age_slots\":5", "\"age_slots\":-1");
        assert!(
            parse_event_line(&bad_slot).is_err(),
            "negative age_slots rejected"
        );
    }

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
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
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
        assert_eq!(
            lines.len(),
            5,
            "append mode should preserve existing events"
        );
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
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
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
        writer
            .write_event(
                &AppEvent::MarketTrade {
                    mint,
                    price_fp: 2_000_000_000,
                    quote_lamports: 100_000,
                    liquidity_lamports: 500_000_000,
                    signed_base: 10_000,
                    buyer_entity: 5,
                    age_slots: 20,
                    recv_unix_ms: None,
                    trader_pubkey: None,
                    slot: None,
                    fee_lamports: None,
                    cu_consumed: None,
                    venue: None,
                    event_id: None,
                    feature: None,
                },
                2,
            )
            .expect("write");
        writer
            .write_event(
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
        let content = r#"{"slot":1,"kind":"Tick"}
garbage line 1
garbage line 2
{"slot":2,"kind":"Tick"}"#
            .to_string();
        fs::write(&tmp, &content).expect("write");
        let (events, skipped) = read_event_stream(&tmp).expect("read");
        assert_eq!(events.len(), 2, "two valid Tick events");
        assert_eq!(skipped, 2, "two malformed lines skipped");
        let _ = fs::remove_file(&tmp);
    }

    /// The receive time is written only when the wire carried one, and a tape that predates
    /// the field still reads back — as `None`, never as a substituted clock.
    #[test]
    fn recv_unix_ms_round_trips_and_legacy_tapes_stay_readable() {
        use pump_quant_domain::ids::Mint;
        let stamped = AppEvent::MarketTrade {
            mint: Mint([7u8; 32]),
            price_fp: 25_000_000_000,
            quote_lamports: 400_000_000,
            liquidity_lamports: 30_000_000_000,
            signed_base: 2_000_000,
            buyer_entity: 42,
            age_slots: 12,
            recv_unix_ms: Some(1_700_000_000_000),
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        };
        let line = event_to_json(&stamped, 9);
        assert!(
            line.contains(r#""recv_unix_ms":1700000000000"#),
            "stamped on the wire: {line}"
        );
        match parse_event_line(&line).expect("round trip") {
            AppEvent::MarketTrade { recv_unix_ms, .. } => {
                assert_eq!(recv_unix_ms, Some(1_700_000_000_000))
            }
            other => panic!("wrong variant: {other:?}"),
        }

        // An unstamped print omits the key entirely rather than writing a default.
        let unstamped = AppEvent::MarketTrade {
            mint: Mint([7u8; 32]),
            price_fp: 25_000_000_000,
            quote_lamports: 400_000_000,
            liquidity_lamports: 30_000_000_000,
            signed_base: 2_000_000,
            buyer_entity: 42,
            age_slots: 12,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        };
        let line = event_to_json(&unstamped, 9);
        assert!(!line.contains("recv_unix_ms"), "nothing fabricated: {line}");
        // And the legacy shape (the key absent) parses to `None`, NOT to a skipped line.
        match parse_event_line(&line).expect("legacy line still parses") {
            AppEvent::MarketTrade { recv_unix_ms, .. } => assert_eq!(recv_unix_ms, None),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// The trader's ADDRESS survives the tape, and its absence stays absent. The engine's hashed
    /// `buyer_entity` cannot be turned back into it, which is why it rides separately.
    #[test]
    fn the_traders_address_round_trips_through_the_tape() {
        use pump_quant_domain::ids::Mint;
        let wallet = [0x9Au8; 32];
        let stamped = AppEvent::MarketTrade {
            mint: Mint([7u8; 32]),
            price_fp: 25_000_000_000,
            quote_lamports: 400_000_000,
            liquidity_lamports: 30_000_000_000,
            signed_base: 2_000_000,
            buyer_entity: 42,
            age_slots: 12,
            recv_unix_ms: Some(1_700_000_000_000),
            trader_pubkey: Some(wallet),
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        };
        let line = event_to_json(&stamped, 9);
        assert!(line.contains("trader_pubkey"), "{line}");
        match parse_event_line(&line).expect("round trip") {
            AppEvent::MarketTrade { trader_pubkey, .. } => {
                assert_eq!(trader_pubkey, Some(wallet))
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // A print whose producer had no signer writes no address, and reads back as none.
        let anonymous = AppEvent::MarketTrade {
            mint: Mint([7u8; 32]),
            price_fp: 25_000_000_000,
            quote_lamports: 400_000_000,
            liquidity_lamports: 30_000_000_000,
            signed_base: 2_000_000,
            buyer_entity: 42,
            age_slots: 12,
            recv_unix_ms: Some(1_700_000_000_000),
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        };
        let line = event_to_json(&anonymous, 9);
        assert!(!line.contains("trader_pubkey"), "{line}");
        match parse_event_line(&line).expect("parses") {
            AppEvent::MarketTrade { trader_pubkey, .. } => assert_eq!(trader_pubkey, None),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
