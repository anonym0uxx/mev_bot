//! Port of `feeds/helius.rs` decode logic (leaf `in_helius_parse`).
//!
//! Responsibility: turn a Helius `logsNotification` payload into a
//! [`CanonicalTx`]. This ports two behaviors from the legacy file:
//!   1. `check_graduation_logs` — byte-level scan of log lines for Raydium /
//!      PumpSwap / pump.fun-migrate markers → a `Graduation` record.
//!   2. `parse_helius_log` — detect a pump.fun program invocation plus a
//!      Buy/Sell instruction → a `Trade` record.
//!
//! Graduation is checked first (it is higher priority and mutually exclusive
//! with a normal trade), matching the legacy read loop. `logsSubscribe` carries
//! no account keys, so mint/trader/amounts are zero and only the signature,
//! slot, direction, and kind are populated — faithful to the legacy behavior.
//! Pure byte/JSON parsing, no floats, no wall clock (§22).

use crate::base58;
use crate::canonical::{CanonicalTx, SourceKind, TradeDirection, TxKind};
use crate::json::{self, JsonValue};

/// pump.fun program id (bonding-curve program).
pub const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// The runtime log line that proves a transaction actually entered the pump.fun
/// program, precomputed as a `const` rather than `format!`ed per call.
///
/// This was `format!("Program {PUMP_PROGRAM_ID} invoke")` built once per parsed
/// transaction — a heap allocation on the per-transaction decode path, and the only
/// production `hot_alloc_fmt` violation in this crate when it was brought into the
/// enforced hot scope (`rust/lint_rules.yaml`). `concat!` requires literals, so the
/// program id is spelled out here and `prefix_matches_program_id` asserts the two
/// spellings can never diverge.
const PUMP_INVOKE_PREFIX: &str = concat!(
    "Program ",
    "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
    " invoke"
);

/// Raydium AMM v4 invoke marker — primary (pre-March-2025) graduation signal.
pub const GRADUATION_LOG_MARKER: &[u8] =
    b"Program 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8 invoke";
/// PumpSwap `CreatePool` marker — newer (post-March-2025) graduation signal.
pub const PUMPSWAP_LOG_MARKER: &[u8] = b"Instruction: CreatePool";
/// pump.fun `Migrate` marker — emitted by the pump.fun program at graduation.
pub const PUMPFUN_MIGRATE_MARKER: &[u8] = b"Instruction: Migrate";

/// Byte-level substring search (ports the legacy `bytes_contains`, minus the
/// `memchr` SIMD dependency). Returns `true` iff `needle` occurs in `haystack`.
/// A needle longer than the haystack yields `false` (no panic).
pub fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Parse a Helius `logsNotification` payload into a [`CanonicalTx`].
///
/// Returns `None` for: malformed JSON, non-`logsNotification` messages, failed
/// transactions (`err != null`), and pump.fun trades whose logs contain no
/// pump.fun invocation. A graduation is returned as `TxKind::Graduation` with
/// `TradeDirection::Unknown`; a normal trade as `TxKind::Trade` with the
/// detected Buy/Sell direction.
pub fn parse_helius(payload: &[u8]) -> Option<CanonicalTx> {
    let root = json::parse(payload)?;

    if root.get("method")?.as_str()? != "logsNotification" {
        return None;
    }

    let result = root.path("params/result")?;
    let value = result.get("value")?;

    // Skip failed transactions.
    if !value.get("err")?.is_null() {
        return None;
    }

    let sig_str = value.get("signature")?.as_str()?;
    let signature = base58::decode_signature(sig_str)?;

    let slot = result
        .path("context/slot")
        .and_then(|s| s.as_number_str())
        .and_then(json::number_to_u128_trunc)
        .unwrap_or(0) as u64;

    let logs = value.get("logs").and_then(|l| l.as_array()).unwrap_or(&[]);

    // ── Graduation detection (checked first, higher priority) ──
    if logs_contain_graduation_marker(logs) {
        return Some(CanonicalTx {
            slot,
            signature,
            mint: [0u8; 32],
            trader: [0u8; 32],
            sol_delta: 0,
            token_delta: 0,
            vsol_reserves: 0,
            vtoken_reserves: 0,
            market_cap_lamports: 0,
            timestamp_ms: 0,
            direction: TradeDirection::Unknown,
            kind: TxKind::Graduation,
            source: SourceKind::HeliusWsLogs,
        });
    }

    // ── Normal pump.fun trade: require a pump.fun invoke line ──
    let mut is_pump_trade = false;
    let mut direction = TradeDirection::Buy; // legacy default assumption

    for entry in logs {
        if let Some(s) = entry.as_str() {
            if s.starts_with(PUMP_INVOKE_PREFIX) {
                is_pump_trade = true;
            }
            if s.contains("Instruction: Buy") {
                direction = TradeDirection::Buy;
            } else if s.contains("Instruction: Sell") {
                direction = TradeDirection::Sell;
            }
        }
    }

    if !is_pump_trade {
        return None;
    }

    Some(CanonicalTx {
        slot,
        signature,
        mint: [0u8; 32],
        trader: [0u8; 32],
        sol_delta: 0,
        token_delta: 0,
        vsol_reserves: 0,
        vtoken_reserves: 0,
        market_cap_lamports: 0,
        timestamp_ms: 0,
        direction,
        kind: TxKind::Trade,
        source: SourceKind::HeliusWsLogs,
    })
}

/// Scan all log lines for any graduation marker (ports
/// `logs_contain_graduation_marker`).
fn logs_contain_graduation_marker(logs: &[JsonValue]) -> bool {
    for entry in logs {
        if let Some(s) = entry.as_str() {
            let b = s.as_bytes();
            if bytes_contains(b, GRADUATION_LOG_MARKER)
                || bytes_contains(b, PUMPSWAP_LOG_MARKER)
                || bytes_contains(b, PUMPFUN_MIGRATE_MARKER)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod prefix_tests {
    use super::*;

    /// The `concat!`-built prefix duplicates the program id as a literal because
    /// `concat!` cannot take a `const`. This proves the duplication can never drift:
    /// if `PUMP_PROGRAM_ID` is ever corrected, this fails until the literal is too.
    #[test]
    fn prefix_matches_program_id() {
        assert_eq!(
            PUMP_INVOKE_PREFIX,
            format!("Program {PUMP_PROGRAM_ID} invoke"),
            "the const invoke prefix and PUMP_PROGRAM_ID have diverged — the prefix \
             spells the id out as a literal because concat! requires literals"
        );
    }
}
