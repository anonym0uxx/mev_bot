//! M2 — the PERSISTENT CAUSAL observed-launch creator registry.
//!
//! The trained `creator_past_launches` is an OBSERVED-launch count (see [`crate::creator_history`]):
//! mint first-seen wins, creator = the create transaction's signer (account key 0), and the count is
//! the creator's observed launches with `recv_unix_ms` STRICTLY before this mint's own. The daemon
//! previously fed that registry only from live `LaunchObserved`, so every restart erased history.
//! This module closes that gap with two inputs and no new counting rule:
//!
//! 1. A read-only SEED from the trained launch table, filtered to rows with `recv_unix_ms < cutoff_ms`
//!    (the replay/run boundary). The sha256 is over the FILTERED content (each kept raw line + `\n`),
//!    pinned by the caller and persisted in the log header with the provenance label and cutoff. A
//!    row at or after the cutoff never enters: no decision can read a launch from its own future.
//! 2. An APPEND-ONLY durable log of our own `LaunchObserved` (one fsynced line per new mint; header
//!    written atomically: temp, fsync, rename, fsync dir). It is restored at startup BEFORE entry
//!    inference resumes. An unreadable / torn / incompatible log is a NAMED refusal
//!    ([`RegistryRefusal`]), never a silently empty registry.
//!
//! Dedup identity is the MINT on both paths: a replay that re-delivers a launch already seeded or
//! already logged changes no count and appends nothing.
//!
//! What it is NOT: a lifetime count, or complete chain coverage. Every decision record that uses it
//! carries [`PROVENANCE_LABEL`]. The superseded OW4H proposal (4-hour warm-up window) is RETIRED:
//! nothing here computes, serves or renders an OW4H value.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Log schema.
pub const LOG_SCHEMA: u64 = 1;
/// The label every decision record that reads the registry carries.
pub const PROVENANCE_LABEL: &str = "observed history, not lifetime / complete chain coverage";
/// The dedup identity on every insert path.
pub const DEDUP_IDENTITY: &str = "mint";
/// OW4H is retired: it is never computed or served. Kept as a named constant so a grep finds the decision.
pub const OW4H_STATUS: &str = "retired_unused_never_served";

/// Why the registry could not be established. Stable labels via [`RegistryRefusal::as_str`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryRefusal {
    SeedUnreadable,
    SeedRowMalformed {
        line: u64,
    },
    SeedSha256Mismatch {
        got: String,
    },
    LogUnreadable,
    LogHeaderMalformed,
    LogSchemaIncompatible,
    /// The log was written against a different seed (sha / label / cutoff / dedup identity).
    LogSeedMismatch,
    LogRecordMalformed {
        line: u64,
    },
    /// The last line has no terminating newline: an interrupted append. Never silently dropped.
    LogTornTail,
    LogCreateFailed,
    /// A log record contradicts an earlier record or the seed for the same mint.
    LogConflict {
        line: u64,
    },
    /// A durable append failed at runtime: a restart could not reproduce this run's counts.
    LogAppendFailed,
}

impl RegistryRefusal {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            RegistryRefusal::SeedUnreadable => "creator_registry_seed_unreadable",
            RegistryRefusal::SeedRowMalformed { .. } => "creator_registry_seed_row_malformed",
            RegistryRefusal::SeedSha256Mismatch { .. } => "creator_registry_seed_sha256_mismatch",
            RegistryRefusal::LogUnreadable => "creator_registry_log_unreadable",
            RegistryRefusal::LogHeaderMalformed => "creator_registry_log_header_malformed",
            RegistryRefusal::LogSchemaIncompatible => "creator_registry_log_schema_incompatible",
            RegistryRefusal::LogSeedMismatch => "creator_registry_log_seed_mismatch",
            RegistryRefusal::LogRecordMalformed { .. } => "creator_registry_log_record_malformed",
            RegistryRefusal::LogTornTail => "creator_registry_log_torn_tail",
            RegistryRefusal::LogCreateFailed => "creator_registry_log_create_failed",
            RegistryRefusal::LogConflict { .. } => "creator_registry_log_conflict",
            RegistryRefusal::LogAppendFailed => "creator_registry_log_append_failed",
        }
    }
}

/// Where the seed comes from and where it stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSpec {
    pub path: PathBuf,
    /// Only rows with `recv_unix_ms < cutoff_ms` are loaded.
    pub cutoff_ms: i64,
    pub label: String,
    /// Optional pinned sha256 (hex) of the FILTERED content; a mismatch is a refusal.
    pub expect_sha256: Option<String>,
}

/// One launch (seed row or log record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchRow {
    pub mint: [u8; 32],
    pub creator: [u8; 32],
    pub recv_unix_ms: i64,
}

/// A loaded, filtered seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    pub rows: Vec<LaunchRow>,
    pub sha256: String,
    pub label: String,
    pub cutoff_ms: i64,
    pub rows_read: u64,
    pub rows_at_or_after_cutoff: u64,
}

/// What the log header pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub seed_sha256: String,
    pub seed_label: String,
    pub seed_cutoff_ms: i64,
}

impl Provenance {
    /// The no-seed provenance (registry starts empty at the cutoff, declared).
    #[must_use]
    pub fn unseeded(cutoff_ms: i64) -> Self {
        Self {
            seed_sha256: "none".into(),
            seed_label: "none".into(),
            seed_cutoff_ms: cutoff_ms,
        }
    }

    /// Compact provenance string carried on every decision record.
    #[must_use]
    pub fn record_tag(&self) -> String {
        format!(
            "creator_registry=seed:{}@sha256:{}|cutoff_ms:{}|dedup:{}|+observed_launch_log|{}",
            self.seed_label,
            self.seed_sha256,
            self.seed_cutoff_ms,
            DEDUP_IDENTITY,
            PROVENANCE_LABEL
        )
    }
}

const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Decode a base58 Solana pubkey into exactly 32 bytes.
#[must_use]
pub fn b58_32(s: &str) -> Option<[u8; 32]> {
    if s.is_empty() || s.len() > 44 {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(32);
    for c in s.bytes() {
        let mut carry = B58.iter().position(|&x| x == c)? as u32;
        for b in out.iter_mut() {
            carry += u32::from(*b) * 58;
            *b = (carry & 0xff) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    for c in s.bytes() {
        if c == b'1' {
            out.push(0);
        } else {
            break;
        }
    }
    out.reverse();
    <[u8; 32]>::try_from(out.as_slice()).ok()
}

fn hex(b: &[u8; 32]) -> String {
    pump_quant_protocol::sha256::to_hex(b)
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Read the trained launch table, keeping ONLY rows strictly before `spec.cutoff_ms`. The table is
/// streamed; rows at/after the cutoff are counted and discarded, never held.
pub fn load_seed(spec: &SeedSpec) -> Result<Seed, RegistryRefusal> {
    let f = fs::File::open(&spec.path).map_err(|_| RegistryRefusal::SeedUnreadable)?;
    let mut h = pump_quant_protocol::sha256::Sha256::new();
    let mut rows = Vec::new();
    let (mut read, mut after) = (0u64, 0u64);
    for line in std::io::BufReader::new(f).lines() {
        let line = line.map_err(|_| RegistryRefusal::SeedUnreadable)?;
        if line.trim().is_empty() {
            continue;
        }
        read += 1;
        let v: Value = serde_json::from_str(&line)
            .map_err(|_| RegistryRefusal::SeedRowMalformed { line: read })?;
        let bad = RegistryRefusal::SeedRowMalformed { line: read };
        let t = v
            .get("recv_unix_ms")
            .and_then(Value::as_i64)
            .ok_or(bad.clone())?;
        // THE CUTOFF GUARD: strictly before the run boundary.
        if t >= spec.cutoff_ms {
            after += 1;
            continue;
        }
        let mint = v
            .get("mint")
            .and_then(Value::as_str)
            .and_then(b58_32)
            .ok_or(bad.clone())?;
        let creator = v
            .get("creator")
            .and_then(Value::as_str)
            .and_then(b58_32)
            .ok_or(bad)?;
        h.update(line.as_bytes());
        h.update(b"\n");
        rows.push(LaunchRow {
            mint,
            creator,
            recv_unix_ms: t,
        });
    }
    let sha256 = pump_quant_protocol::sha256::to_hex(&h.finalize());
    if let Some(want) = &spec.expect_sha256 {
        if !want.eq_ignore_ascii_case(&sha256) {
            return Err(RegistryRefusal::SeedSha256Mismatch { got: sha256 });
        }
    }
    Ok(Seed {
        rows,
        sha256,
        label: spec.label.clone(),
        cutoff_ms: spec.cutoff_ms,
        rows_read: read,
        rows_at_or_after_cutoff: after,
    })
}

fn header_line(p: &Provenance) -> String {
    json!({
        "schema": LOG_SCHEMA,
        "kind": "observed_launch_log",
        "seed_sha256": p.seed_sha256,
        "seed_label": p.seed_label,
        "seed_cutoff_ms": p.seed_cutoff_ms,
        "dedup": DEDUP_IDENTITY,
        "label": PROVENANCE_LABEL,
    })
    .to_string()
}

/// Append-only log of our own `LaunchObserved`.
#[derive(Debug)]
pub struct LaunchLog {
    path: PathBuf,
    file: fs::File,
    appended: u64,
}

impl LaunchLog {
    /// Open an existing log (validating header + every record) or create a fresh one. Returns the
    /// trusted records in file order.
    pub fn open(path: &Path, prov: &Provenance) -> Result<(Self, Vec<LaunchRow>), RegistryRefusal> {
        let records = if path.exists() {
            Self::read(path, prov)?
        } else {
            Self::create(path, prov)?;
            Vec::new()
        };
        let file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|_| RegistryRefusal::LogUnreadable)?;
        Ok((
            Self {
                path: path.to_path_buf(),
                file,
                appended: 0,
            },
            records,
        ))
    }

    fn create(path: &Path, prov: &Provenance) -> Result<(), RegistryRefusal> {
        let tmp = path.with_extension("tmp");
        let body = format!("{}\n", header_line(prov));
        let res = (|| -> std::io::Result<()> {
            {
                let mut f = fs::File::create(&tmp)?;
                f.write_all(body.as_bytes())?;
                f.sync_all()?;
            }
            fs::rename(&tmp, path)?;
            if let Some(dir) = path.parent() {
                if let Ok(d) = fs::File::open(dir) {
                    let _ = d.sync_all();
                }
            }
            Ok(())
        })();
        res.map_err(|_| RegistryRefusal::LogCreateFailed)
    }

    /// Parse a log. Any structural problem is a named refusal.
    pub fn read(path: &Path, prov: &Provenance) -> Result<Vec<LaunchRow>, RegistryRefusal> {
        let body = fs::read(path).map_err(|_| RegistryRefusal::LogUnreadable)?;
        let body = String::from_utf8(body).map_err(|_| RegistryRefusal::LogUnreadable)?;
        if !body.ends_with('\n') {
            return Err(if body.is_empty() {
                RegistryRefusal::LogHeaderMalformed
            } else {
                RegistryRefusal::LogTornTail
            });
        }
        let mut lines = body.lines();
        let hdr: Value = lines
            .next()
            .and_then(|l| serde_json::from_str(l).ok())
            .ok_or(RegistryRefusal::LogHeaderMalformed)?;
        if hdr.get("kind").and_then(Value::as_str) != Some("observed_launch_log") {
            return Err(RegistryRefusal::LogHeaderMalformed);
        }
        if hdr.get("schema").and_then(Value::as_u64) != Some(LOG_SCHEMA) {
            return Err(RegistryRefusal::LogSchemaIncompatible);
        }
        let s = |k: &str| hdr.get(k).and_then(Value::as_str).map(str::to_string);
        if s("seed_sha256").as_deref() != Some(prov.seed_sha256.as_str())
            || s("seed_label").as_deref() != Some(prov.seed_label.as_str())
            || hdr.get("seed_cutoff_ms").and_then(Value::as_i64) != Some(prov.seed_cutoff_ms)
            || s("dedup").as_deref() != Some(DEDUP_IDENTITY)
        {
            return Err(RegistryRefusal::LogSeedMismatch);
        }
        let mut out = Vec::new();
        for (i, l) in lines.enumerate() {
            let line = i as u64 + 2;
            let bad = RegistryRefusal::LogRecordMalformed { line };
            let v: Value = serde_json::from_str(l).map_err(|_| bad.clone())?;
            let mint = v
                .get("mint")
                .and_then(Value::as_str)
                .and_then(unhex)
                .ok_or(bad.clone())?;
            let creator = v
                .get("creator")
                .and_then(Value::as_str)
                .and_then(unhex)
                .ok_or(bad.clone())?;
            let t = v.get("launch_unix_ms").and_then(Value::as_i64).ok_or(bad)?;
            out.push(LaunchRow {
                mint,
                creator,
                recv_unix_ms: t,
            });
        }
        Ok(out)
    }

    /// Durably append one NEW launch (fsync before returning).
    pub fn append(&mut self, row: &LaunchRow, session: &str) -> std::io::Result<()> {
        let line = json!({
            "mint": hex(&row.mint),
            "creator": hex(&row.creator),
            "launch_unix_ms": row.recv_unix_ms,
            "session": session,
        })
        .to_string();
        self.file.write_all(format!("{line}\n").as_bytes())?;
        self.file.sync_data()?;
        self.appended += 1;
        Ok(())
    }

    #[must_use]
    pub fn appended(&self) -> u64 {
        self.appended
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What a successful startup restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryStartup {
    pub provenance: Provenance,
    pub seeded: u64,
    pub seed_rows_at_or_after_cutoff: u64,
    pub restored_from_log: u64,
    /// Seed/log rows that named an already-present mint (dedup by mint; counted once).
    pub dedup_overlap: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b58_round_trips_known_keys() {
        assert_eq!(b58_32("11111111111111111111111111111111"), Some([0u8; 32]));
        let k = b58_32("So11111111111111111111111111111111111111112").expect("wsol");
        assert_eq!(k[0], 0x06);
        assert!(b58_32("0OIl").is_none());
    }
}
