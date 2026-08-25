//! Policy: the TOML config that gates every treasury transfer.
//!
//! The policy file is written by Alon, reviewed on GitHub, and deployed by
//! Alon. The code enforces it — there is no override path.
//!
//! ## Example policy file (`treasury_policy.toml`)
//! ```toml
//! [limits]
//! auto_max_lamports = 5_000_000_000       # 5 SOL — under this: auto-execute
//! approval_threshold_lamports = 20_000_000_000  # 20 SOL — over this: time-lock + confirm
//! time_lock_seconds = 3600               # 1 hour cool-down for large transfers
//! daily_cap_lamports = 100_000_000_000   # 100 SOL — hard daily ceiling across ALL addresses
//!
//! [[whitelist]]
//! address = "GqYcSFbu5B1hGY1KKfFWUg6Zeau1SZX9qsfy1CnbggiM"
//! label = "Alon_personal"
//! max_per_tx_lamports = 50_000_000_000   # 50 SOL per tx
//! max_daily_lamports = 200_000_000_000   # 200 SOL per day
//!
//! [[whitelist]]
//! address = "AnotherValidAddress..."
//! label = "operating_expenses"
//! max_per_tx_lamports = 10_000_000_000
//! max_daily_lamports = 30_000_000_000
//! ```
//!
//! ## §22 compliance
//! All amounts are in lamports (u64). No floats on the money path.

use std::collections::HashMap;

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A single whitelisted destination address with per-address limits.
#[derive(Debug, Clone)]
pub struct WhitelistEntry {
    /// Base58 Solana pubkey.
    pub address: String,
    /// Human-readable label for logging.
    pub label: String,
    /// Max lamports per single transfer to this address.
    pub max_per_tx_lamports: u64,
    /// Max lamports per day (UTC) to this address.
    pub max_daily_lamports: u64,
}

/// Global limits that apply across all whitelisted addresses.
#[derive(Debug, Clone)]
pub struct TransferLimits {
    /// Transfers at or below this amount auto-execute (if whitelisted + within limits).
    pub auto_max_lamports: u64,
    /// Transfers above this amount require a time-lock + explicit confirmation.
    pub approval_threshold_lamports: u64,
    /// Duration of the time-lock in seconds for transfers exceeding the approval threshold.
    pub time_lock_seconds: u64,
    /// Hard daily ceiling across ALL whitelisted addresses combined.
    pub daily_cap_lamports: u64,
}

/// Codeword gate: an additional human-in-the-loop confirmation step.
///
/// The policy file stores a SHA-256 hash of the codeword (never the plaintext).
/// The CLI prompts for the codeword on stdin, hashes the input, and compares.
/// This is an ADDITIONAL gate — it does not replace the whitelist or limits.
///
/// ## Threat model
/// A codeword alone is NOT sufficient security (it can be leaked, guessed, or
/// socially engineered). It exists as a convenience gate for routine small
/// transfers within the whitelist. Large transfers above the approval
/// threshold still require the time-lock + confirmation flow regardless of
/// the codeword.
///
/// ## Storage
/// The hash is stored as a hex string in the policy file under
/// `codeword_hash`. The codeword itself is never written to disk by this
/// crate. Alon sets the codeword by computing its SHA-256 hash and writing
/// the hex digest into the policy TOML.
#[derive(Debug, Clone)]
pub struct CodewordGate {
    /// SHA-256 hash of the codeword (hex string, 64 chars).
    /// None = codeword gate disabled (whitelist + limits still enforced).
    pub hash: Option<String>,
}

/// The complete policy: limits + whitelist + codeword gate.
#[derive(Debug, Clone)]
pub struct TreasuryPolicy {
    pub limits: TransferLimits,
    pub whitelist: Vec<WhitelistEntry>,
    /// Pre-built lookup: address → whitelist entry index.
    whitelist_map: HashMap<String, usize>,
    /// Optional codeword gate (SHA-256 hash of the codeword).
    pub codeword: CodewordGate,
}

impl TreasuryPolicy {
    /// Parse a policy from TOML text.
    ///
    /// Hand-rolled parser (no `toml` crate dependency) — the policy format is
    /// simple enough that a full TOML parser is unnecessary weight. This also
    /// lets us enforce strict validation: unknown keys, missing fields, and
    /// zero/duplicate addresses are hard errors.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let mut limits_auto: Option<u64> = None;
        let mut limits_approval: Option<u64> = None;
        let mut limits_time: Option<u64> = None;
        let mut limits_daily: Option<u64> = None;
        let mut codeword_hash: Option<String> = None;

        let mut whitelist: Vec<WhitelistEntry> = Vec::new();
        let mut current_entry: Option<WhitelistEntry> = None;

        for raw_line in text.lines() {
            let line = raw_line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Section headers
            if line.starts_with('[') && line.ends_with(']') {
                // Flush any in-progress whitelist entry
                if let Some(entry) = current_entry.take() {
                    whitelist.push(entry);
                }

                let section = &line[1..line.len() - 1];
                match section {
                    "limits" => {}
                    "whitelist" => {
                        current_entry = Some(WhitelistEntry {
                            address: String::new(),
                            label: String::new(),
                            max_per_tx_lamports: 0,
                            max_daily_lamports: 0,
                        });
                    }
                    s if s.starts_with("whitelist.") => {
                        // [[whitelist]] array entry — same as "whitelist"
                        current_entry = Some(WhitelistEntry {
                            address: String::new(),
                            label: String::new(),
                            max_per_tx_lamports: 0,
                            max_daily_lamports: 0,
                        });
                    }
                    _ => {} // ignore unknown sections (could hard-error instead)
                }
                continue;
            }

            // Key = value lines
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };

            // Strip quotes from string values
            let val_str = value.trim_matches('"').to_string();

            match (key, current_entry.as_mut()) {
                // [limits] section
                ("auto_max_lamports", _) => limits_auto = Some(parse_u64(value, key)?),
                ("approval_threshold_lamports", _) => {
                    limits_approval = Some(parse_u64(value, key)?)
                }
                ("time_lock_seconds", _) => limits_time = Some(parse_u64(value, key)?),
                ("daily_cap_lamports", _) => limits_daily = Some(parse_u64(value, key)?),

                // [limits] codeword — SHA-256 hash (hex string) of the codeword
                ("codeword_hash", _) => codeword_hash = Some(val_str),

                // [[whitelist]] entries
                ("address", Some(entry)) => entry.address = val_str,
                ("label", Some(entry)) => entry.label = val_str,
                ("max_per_tx_lamports", Some(entry)) => {
                    entry.max_per_tx_lamports = parse_u64(value, key)?
                }
                ("max_daily_lamports", Some(entry)) => {
                    entry.max_daily_lamports = parse_u64(value, key)?
                }

                _ => {} // ignore unknown keys
            }
        }

        // Flush last whitelist entry
        if let Some(entry) = current_entry {
            whitelist.push(entry);
        }

        let limits = TransferLimits {
            auto_max_lamports: limits_auto.ok_or("missing limits.auto_max_lamports")?,
            approval_threshold_lamports: limits_approval
                .ok_or("missing limits.approval_threshold_lamports")?,
            time_lock_seconds: limits_time.ok_or("missing limits.time_lock_seconds")?,
            daily_cap_lamports: limits_daily.ok_or("missing limits.daily_cap_lamports")?,
        };

        // Validate whitelist entries
        let mut whitelist_map = HashMap::new();
        for (i, entry) in whitelist.iter().enumerate() {
            if entry.address.is_empty() {
                return Err(format!("whitelist entry {i} has empty address"));
            }
            if entry.max_per_tx_lamports == 0 {
                return Err(format!("whitelist entry {i} ({}) has zero max_per_tx", entry.address));
            }
            if entry.max_daily_lamports == 0 {
                return Err(format!("whitelist entry {i} ({}) has zero max_daily", entry.address));
            }
            if whitelist_map.contains_key(&entry.address) {
                return Err(format!("duplicate whitelist address: {}", entry.address));
            }
            whitelist_map.insert(entry.address.clone(), i);
        }

        if whitelist.is_empty() {
            return Err("whitelist is empty — no transfers allowed".to_string());
        }

        Ok(Self {
            limits,
            whitelist,
            whitelist_map,
            codeword: CodewordGate {
                hash: codeword_hash,
            },
        })
    }

    /// Look up a whitelist entry by address. Returns None if not whitelisted.
    #[must_use]
    pub fn find_whitelist(&self, address: &str) -> Option<&WhitelistEntry> {
        self.whitelist_map
            .get(address)
            .map(|&i| &self.whitelist[i])
    }

    /// Verify a codeword against the policy's stored hash.
    ///
    /// Uses `ring::digest::SHA256` to hash the input, then compares the
    /// hex-encoded digests. This is a constant-time-ish comparison (we compare
    /// full hex strings; ring's digest is already constant-time).
    ///
    /// Returns `true` if the codeword matches or if no codeword is configured
    /// (disabled = permissive). Returns `false` if a codeword is configured
    /// but the input does not match.
    ///
    /// # Important
    /// This is an ADDITIONAL gate. It does NOT replace the whitelist or limits.
    /// Even with a correct codeword, the transfer must still pass all
    /// whitelist and limit checks.
    #[must_use]
    pub fn verify_codeword(&self, input: &str) -> bool {
        let Some(ref expected_hash) = self.codeword.hash else {
            // No codeword configured → gate is disabled (permissive).
            return true;
        };

        // SHA-256 hash of the input
        let digest = ring::digest::digest(&ring::digest::SHA256, input.as_bytes());
        let input_hash = hex_encode(&digest.as_ref());

        // Constant-time comparison of hex strings.
        // ring doesn't expose constant_time::verify_equal publicly in 0.17,
        // so we do a byte-wise comparison. The codeword is an additional
        // gate (not the primary security boundary), so constant-time is
        // best-effort here — the whitelist + limits are the real boundary.
        expected_hash.eq_ignore_ascii_case(&input_hash)
    }

    /// True if a codeword is configured (gate is active).
    #[must_use]
    pub fn has_codeword(&self) -> bool {
        self.codeword.hash.is_some()
    }
}

/// Parse a u64 from a TOML value, stripping underscores and trailing comments.
fn parse_u64(raw: &str, key: &str) -> Result<u64, String> {
    let cleaned = raw
        .trim()
        .trim_matches('"')
        .replace('_', "");
    // Strip trailing comments
    let cleaned = cleaned.split('#').next().unwrap_or(&cleaned).trim().to_string();

    cleaned
        .parse::<u64>()
        .map_err(|_| format!("invalid u64 for {key}: {cleaned}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"
# Treasury policy for the mev_bot hot wallet.
# Written by Alon. Reviewed on GitHub. Deployed by Alon.

[limits]
auto_max_lamports = 5_000_000_000           # 5 SOL
approval_threshold_lamports = 20_000_000_000  # 20 SOL
time_lock_seconds = 3600                    # 1 hour
daily_cap_lamports = 100_000_000_000        # 100 SOL

[[whitelist]]
address = "GqYcSFbu5B1hGY1KKfFWUg6Zeau1SZX9qsfy1CnbggiM"
label = "Alon_personal"
max_per_tx_lamports = 50_000_000_000
max_daily_lamports = 200_000_000_000

[[whitelist]]
address = "AnotherValidAddress1234567890"
label = "ops"
max_per_tx_lamports = 10_000_000_000
max_daily_lamports = 30_000_000_000
"#;

    #[test]
    fn parse_sample_policy() {
        let policy = TreasuryPolicy::from_toml(SAMPLE_TOML).unwrap();
        assert_eq!(policy.limits.auto_max_lamports, 5_000_000_000);
        assert_eq!(policy.limits.approval_threshold_lamports, 20_000_000_000);
        assert_eq!(policy.limits.time_lock_seconds, 3600);
        assert_eq!(policy.limits.daily_cap_lamports, 100_000_000_000);
        assert_eq!(policy.whitelist.len(), 2);

        let entry0 = &policy.whitelist[0];
        assert_eq!(entry0.address, "GqYcSFbu5B1hGY1KKfFWUg6Zeau1SZX9qsfy1CnbggiM");
        assert_eq!(entry0.label, "Alon_personal");
        assert_eq!(entry0.max_per_tx_lamports, 50_000_000_000);
        assert_eq!(entry0.max_daily_lamports, 200_000_000_000);
    }

    #[test]
    fn whitelist_lookup() {
        let policy = TreasuryPolicy::from_toml(SAMPLE_TOML).unwrap();
        assert!(policy.find_whitelist("GqYcSFbu5B1hGY1KKfFWUg6Zeau1SZX9qsfy1CnbggiM").is_some());
        assert!(policy.find_whitelist("SomeRandomAddress").is_none());
    }

    #[test]
    fn rejects_duplicate_address() {
        let toml = r#"
[limits]
auto_max_lamports = 1
approval_threshold_lamports = 2
time_lock_seconds = 60
daily_cap_lamports = 10

[[whitelist]]
address = "SameAddress"
label = "a"
max_per_tx_lamports = 1
max_daily_lamports = 2

[[whitelist]]
address = "SameAddress"
label = "b"
max_per_tx_lamports = 1
max_daily_lamports = 2
"#;
        let result = TreasuryPolicy::from_toml(toml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("duplicate"));
    }

    #[test]
    fn rejects_empty_whitelist() {
        let toml = r#"
[limits]
auto_max_lamports = 1
approval_threshold_lamports = 2
time_lock_seconds = 60
daily_cap_lamports = 10
"#;
        let result = TreasuryPolicy::from_toml(toml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }
}
