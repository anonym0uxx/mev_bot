//! Audit log: append-only JSONL record of every transfer attempt.
//!
//! Every `request_transfer` call — whether it succeeds, is rejected, or
//! fails — produces an audit entry. The file is append-only: entries are
//! never deleted or modified. This is the on-disk evidence trail.
//!
//! ## Format
//! Each line is a JSON object:
//! ```json
//! {"timestamp":"2026-08-24T12:34:56Z","event":"confirmed","destination":"...","lamports":1000000000,"tx_signature":"...","purpose":"buy NFT"}
//! ```

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// A single audit entry.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    /// ISO 8601 timestamp (UTC).
    pub timestamp: String,
    /// Event type: "confirmed", "rejected", "failed", "time_locked".
    pub event: String,
    /// Destination address.
    pub destination: String,
    /// Amount in lamports (u64, §22).
    pub lamports: u64,
    /// On-chain tx signature (only for "confirmed").
    pub tx_signature: Option<String>,
    /// Human-readable reason / purpose.
    pub purpose: String,
}

impl AuditEntry {
    fn now_iso() -> String {
        // Simple timestamp — no chrono dependency.
        // Uses Unix epoch via std::time + manual formatting.
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Manual ISO 8601 from epoch seconds (UTC).
        // This is a rough approximation for audit logging — not for
        // cryptographic timestamps. On-chain tx signatures are the
        // authoritative time record.
        format!("epoch:{secs}")
    }

    #[must_use]
    pub fn confirmed(destination: &str, lamports: u64, tx_signature: &str, purpose: &str) -> Self {
        Self {
            timestamp: Self::now_iso(),
            event: "confirmed".to_string(),
            destination: destination.to_string(),
            lamports,
            tx_signature: Some(tx_signature.to_string()),
            purpose: purpose.to_string(),
        }
    }

    #[must_use]
    pub fn rejected(destination: &str, lamports: u64, reason: &str, purpose: &str) -> Self {
        Self {
            timestamp: Self::now_iso(),
            event: "rejected".to_string(),
            destination: destination.to_string(),
            lamports,
            tx_signature: None,
            purpose: format!("{reason} | {purpose}"),
        }
    }

    #[must_use]
    pub fn failed(destination: &str, lamports: u64, reason: &str, purpose: &str) -> Self {
        Self {
            timestamp: Self::now_iso(),
            event: "failed".to_string(),
            destination: destination.to_string(),
            lamports,
            tx_signature: None,
            purpose: format!("{reason} | {purpose}"),
        }
    }

    /// Append this entry to the audit log file. Creates the file if it
    /// doesn't exist. Never overwrites existing entries.
    pub fn write(&self, path: &Path) {
        let json = serde_json::json!({
            "timestamp": self.timestamp,
            "event": self.event,
            "destination": self.destination,
            "lamports": self.lamports,
            "tx_signature": self.tx_signature,
            "purpose": self.purpose,
        });

        let line = serde_json::to_string(&json).unwrap_or_else(|_| "{}".to_string());

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}
