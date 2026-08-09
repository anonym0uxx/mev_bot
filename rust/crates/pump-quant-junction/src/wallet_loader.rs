//! Wallet loader — bridges the JSON candidate file to the pure-math
//! `TrackedWalletMatcher`.
//!
//! This module lives in the junction crate because it needs:
//! 1. `pump_quant_ingest::base58::decode_pubkey` — base58 decoding (no `bs58` crate)
//! 2. `pump_quant_wallet_graph::tracked_wallet_matcher::TrackedWalletMatcher`
//! 3. File I/O (the daemon calls this at startup)
//!
//! The matcher itself is dependency-free and lives in the wallet-graph crate.
//! This module is the adapter that reads the curated candidate list (base58
//! Solana pubkeys in a JSON file), decodes them to `[u8;32]`, and hands them
//! to the matcher. The daemon owns the file path; this module owns the
//! decoding + construction.
//!
//! §22: integer-only, no floats. §24/criterion 109: no panics, no per-event
//! allocation — this runs once at startup, not on the hot path.

use pump_quant_ingest::base58::decode_pubkey;
use pump_quant_wallet_graph::tracked_wallet_matcher::TrackedWalletMatcher;

/// A parsed entry from the candidate wallet JSON file.
///
/// Each entry has a base58 Solana pubkey and optional metadata (tier label,
/// notes). The tier label is informational at this stage — the §28 PnL truth
/// screen gates followable status, not the tier label in the candidate file.
#[derive(Clone, Debug)]
pub struct CandidateWallet {
    /// Base58-encoded Solana pubkey (32 bytes when decoded).
    pub pubkey_str: String,
    /// Optional tier label from the candidate file ("T0", "T1", etc.).
    /// Informational only — does NOT confer followable status.
    pub tier_label: Option<String>,
    /// Optional notes (e.g., "known sniper", "dev wallet pattern").
    pub notes: Option<String>,
}

/// Load the tracked-wallet candidate list from a JSON file.
///
/// The JSON format is a simple array of objects with a `pubkey` field (base58
/// Solana address) and optional `tier` / `notes` fields. Example:
///
/// ```json
/// [
///   {"pubkey": "5Jj7...", "tier": "T2", "notes": "frequent minter"},
///   {"pubkey": "7Bzj...", "tier": "T3"}
/// ]
/// ```
///
/// Returns a `TrackedWalletMatcher` with O(1) lookup over all decoded
/// pubkeys. Wallets that fail base58 decoding are skipped and counted in
/// the returned `LoadStats` (the daemon logs them).
///
/// # Errors
/// Returns `Err` only if the file cannot be read or the JSON is malformed.
/// Individual wallet decode failures are soft-skipped, not hard errors.
pub fn load_tracked_wallets_from_json(
    path: &str,
) -> Result<(TrackedWalletMatcher, LoadStats), LoadError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| LoadError::FileRead(path.to_string(), e.to_string()))?;

    parse_tracked_wallets_json(&text)
        .map_err(|e| LoadError::JsonParse(e.to_string()))
        .map(|(matcher, stats)| (matcher, stats))
}

/// Parse the tracked-wallet JSON text and construct a `TrackedWalletMatcher`.
///
/// This is the pure-function core (no file I/O) — testable without touching
/// the filesystem.
pub fn parse_tracked_wallets_json(
    text: &str,
) -> Result<(TrackedWalletMatcher, LoadStats), String> {
    // We use a simple regex-free scanner: find every occurrence of the
    // literal `"pubkey"` key, then extract the quoted string value that
    // follows it. This avoids the fragility of a general state machine
    // and handles the known flat-array format of our candidate file.
    //
    // The wallet-graph crate has zero dependencies and we keep the junction
    // crate lean too (no serde_json), so this manual scanner avoids pulling
    // in a JSON dependency just for a flat array of objects.
    let mut entries: Vec<CandidateWallet> = Vec::new();
    let needle = b"\"pubkey\"";

    let bytes = text.as_bytes();
    let mut search_from = 0usize;

    while let Some(pos) = find_subslice(bytes, needle, search_from) {
        // Skip past the "pubkey" key and find the ':' after it
        let mut i = pos + needle.len();
        // Skip whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
        if i >= bytes.len() || bytes[i] != b':' {
            // Not a key-value pair — skip
            search_from = pos + 1;
            continue;
        }
        i += 1; // consume ':'
        // Skip whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
        if i >= bytes.len() || bytes[i] != b'"' {
            // Value is not a string — skip
            search_from = pos + 1;
            continue;
        }
        // Extract the quoted string value
        let vstart = i + 1;
        let mut vj = vstart;
        while vj < bytes.len() && bytes[vj] != b'"' {
            if bytes[vj] == b'\\' { vj += 1; } // skip escaped char
            vj += 1;
        }
        if vj >= bytes.len() {
            return Err("unterminated string in pubkey value".to_string());
        }
        let val = std::str::from_utf8(&bytes[vstart..vj])
            .map_err(|_| "invalid utf8 in pubkey value".to_string())?
            .to_string();
        i = vj + 1;

        entries.push(CandidateWallet {
            pubkey_str: val,
            tier_label: None,
            notes: None,
        });

        search_from = i;
    }

    // Decode base58 pubkeys and build the matcher
    let mut stats = LoadStats::default();
    let mut pubkeys: Vec<[u8; 32]> = Vec::with_capacity(entries.len());

    for entry in &entries {
        stats.total_entries += 1;
        match decode_pubkey(&entry.pubkey_str) {
            Some(pk) => {
                pubkeys.push(pk);
                stats.decoded_ok += 1;
            }
            None => {
                stats.decode_failures += 1;
                if stats.decode_failures <= 5 {
                    eprintln!("[wallet_loader] base58 decode failed for: {}", entry.pubkey_str);
                }
            }
        }
    }

    let matcher = TrackedWalletMatcher::from_pubkeys(&pubkeys);
    Ok((matcher, stats))
}

/// Find the first occurrence of `needle` in `haystack` starting from `from`.
/// Returns the byte position, or `None` if not found.
fn find_subslice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Statistics from loading the candidate wallet file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadStats {
    /// Total entries parsed from the JSON.
    pub total_entries: u32,
    /// Successfully decoded and added to the matcher.
    pub decoded_ok: u32,
    /// Failed base58 decoding (skipped).
    pub decode_failures: u32,
}

/// Error from loading the tracked wallet file.
#[derive(Debug)]
pub enum LoadError {
    /// Could not read the file. Includes path and error message.
    FileRead(String, String),
    /// JSON parse error.
    JsonParse(String),
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_wallet_json() {
        // Use valid base58 Solana pubkeys (32 bytes when decoded).
        // 32 '1' chars decodes to 32 zero bytes = valid all-zeros pubkey.
        let json = r#"[
            {"pubkey": "11111111111111111111111111111111", "tier": "T2"},
            {"pubkey": "9WzDXgBxk6nuoT3K4XqqLHJQ2T2Fm3A2xqMFq6gRZqHz", "tier": "T1", "notes": "known minter"}
        ]"#;
        let (matcher, stats) = parse_tracked_wallets_json(json).unwrap();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.decoded_ok, 2);
        assert_eq!(stats.decode_failures, 0);
        assert_eq!(matcher.len(), 2);
    }

    #[test]
    fn test_parse_with_decode_failures() {
        // "I" is NOT in the base58 alphabet (Bitcoin/Solana alphabet excludes I, O, 0, l)
        let json = r#"[
            {"pubkey": "9WzDXgBxk6nuoT3K4XqqLHJQ2T2Fm3A2xqMFq6gRZqHz"},
            {"pubkey": "INVALID_BASE58_STRING_WITH_INVALID_CHARS"}
        ]"#;
        let (matcher, stats) = parse_tracked_wallets_json(json).unwrap();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.decoded_ok, 1);
        assert_eq!(stats.decode_failures, 1);
        assert_eq!(matcher.len(), 1);
    }

    #[test]
    fn test_empty_json_array() {
        let (matcher, stats) = parse_tracked_wallets_json("[]").unwrap();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(matcher.len(), 0);
        let test_pk: [u8; 32] = [0x42; 32];
        assert!(!matcher.contains(&test_pk));
    }

    #[test]
    fn test_null_notes_field() {
        let json = r#"[
            {"pubkey": "9WzDXgBxk6nuoT3K4XqqLHJQ2T2Fm3A2xqMFq6gRZqHz", "notes": null}
        ]"#;
        let (matcher, stats) = parse_tracked_wallets_json(json).unwrap();
        assert_eq!(stats.decoded_ok, 1);
        assert_eq!(matcher.len(), 1);
    }
}
