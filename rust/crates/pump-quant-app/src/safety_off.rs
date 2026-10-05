//! Durable SAFETY_OFF for the model lane.
//!
//! CONTRACT (operator-agreed): block entries on endpoint failure; invalidate queued risk-increasing
//! actions; keep pending-order reconciliation and held-position monitoring running; persist the
//! blocked state; alert; require an EXPLICIT re-arm — a restart must never re-arm.
//!
//! Fail-closed everywhere:
//! * a state file that exists but cannot be read or parsed is BLOCKED (never "no file => armed");
//! * only a file that records `blocked=false` (written by an explicit re-arm) or no file at all (a lane
//!   that has never tripped) is armed;
//! * if persisting the block fails, the in-memory block still stands and the failure is counted, never
//!   swallowed into "armed".
//!
//! The record also carries the held positions and pending orders at the moment of the trip/shutdown so a
//! restart can be told exactly what is unreconciled. RESTORING positions from it is NOT implemented here.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Why entries are blocked, as a stable label.
pub const REASON_ENDPOINT_HUNG: &str = "model_endpoint_hung";
/// Controlled daemon shutdown.
pub const REASON_SHUTDOWN: &str = "controlled_shutdown";
/// A state file existed but could not be trusted.
pub const REASON_UNREADABLE: &str = "state_file_unreadable";
/// Operator trip.
pub const REASON_OPERATOR: &str = "operator";

/// Consecutive abandoned (deadline-expired, no answer) model requests that mean the endpoint is hung.
pub const CONSECUTIVE_ABANDONED_TRIP: u32 = 3;

/// A held position as recorded in the durable file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldRecord {
    /// Market.
    pub mint: [u8; 32],
    /// Entry price, fixed point.
    pub entry_price_fp: u64,
    /// Inventory in raw tokens, `None` when never established by a fill (unknown is not zero).
    pub inventory_tokens: Option<u64>,
    /// Whether the model owned its management.
    pub model_managed: bool,
}

/// A pending order as recorded in the durable file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRecord {
    /// `entry` or `reduce` or `exit`.
    pub kind: &'static str,
    /// Order id (own namespace per kind).
    pub id: u64,
    /// Market.
    pub mint: [u8; 32],
    /// Quantity: lamports for an entry, raw tokens for a sell.
    pub quantity: u64,
    /// Whether the acknowledgement is unknown (the order may have landed).
    pub uncertain: bool,
}

/// What the durable file said at load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyLoad {
    /// No file: this lane has never tripped.
    NeverTripped,
    /// A file that records an explicit re-arm.
    Armed {
        /// Epoch of the last transition.
        epoch: u64,
    },
    /// Blocked — either recorded, or the file could not be trusted.
    Blocked {
        /// Why.
        reason: String,
        /// Epoch of the last transition.
        epoch: u64,
        /// Held positions the file lists (unreconciled until a restore exists).
        held: usize,
        /// Pending orders the file lists.
        pending: usize,
    },
}

/// Why a re-arm was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RearmRefusal {
    /// Not blocked.
    NotBlocked,
    /// The caller named nobody.
    NoOperator,
    /// A conflicting execution report is unresolved.
    UnresolvedReconFault,
    /// An uncertain-ack order is still unreconciled.
    UncertainOrderPending,
    /// The re-arm could not be made durable, so it did not happen.
    PersistFailed,
}

/// In-memory safety state.
#[derive(Debug, Clone, Default)]
pub struct SafetyOff {
    /// Entries blocked.
    pub blocked: bool,
    /// Why.
    pub reason: String,
    /// Transition counter (monotone, persisted).
    pub epoch: u64,
    /// Who re-armed last, if anyone.
    pub rearmed_by: Option<String>,
    /// Where it persists.
    pub path: Option<PathBuf>,
    /// Persist attempts that failed.
    pub persist_failures: u64,
    /// Consecutive abandoned requests since the last accepted answer.
    pub consecutive_abandoned: u32,
}

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Atomic, durable write: temp file, fsync, rename, fsync the directory.
fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
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
}

impl SafetyOff {
    /// Read the durable file. Never returns an armed result for a file it cannot trust.
    #[must_use]
    pub fn load(path: &Path) -> (SafetyLoad, SafetyOff) {
        let mut st = SafetyOff {
            path: Some(path.to_path_buf()),
            ..SafetyOff::default()
        };
        let raw = match fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (SafetyLoad::NeverTripped, st);
            }
            Err(_) => return Self::untrusted(st),
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            return Self::untrusted(st);
        };
        let (Some(blocked), Some(epoch)) = (v["blocked"].as_bool(), v["epoch"].as_u64()) else {
            return Self::untrusted(st);
        };
        if v["schema"].as_u64() != Some(1) {
            return Self::untrusted(st);
        }
        st.blocked = blocked;
        st.epoch = epoch;
        st.reason = v["reason"].as_str().unwrap_or("").to_string();
        st.rearmed_by = v["rearmed_by"].as_str().map(str::to_string);
        if blocked {
            let held = v["held"].as_array().map_or(0, Vec::len);
            let pending = v["pending"].as_array().map_or(0, Vec::len);
            (
                SafetyLoad::Blocked {
                    reason: st.reason.clone(),
                    epoch,
                    held,
                    pending,
                },
                st,
            )
        } else {
            (SafetyLoad::Armed { epoch }, st)
        }
    }

    fn untrusted(mut st: SafetyOff) -> (SafetyLoad, SafetyOff) {
        st.blocked = true;
        st.reason = REASON_UNREADABLE.to_string();
        (
            SafetyLoad::Blocked {
                reason: REASON_UNREADABLE.to_string(),
                epoch: 0,
                held: 0,
                pending: 0,
            },
            st,
        )
    }

    /// Persist the current state with the supplied snapshot. Returns whether it is durable.
    pub fn persist(&mut self, held: &[HeldRecord], pending: &[PendingRecord]) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        let body = json!({
            "schema": 1,
            "blocked": self.blocked,
            "reason": self.reason,
            "epoch": self.epoch,
            "rearmed_by": self.rearmed_by,
            "written_wall_ms": wall_ms(),
            "held": held.iter().map(|h| json!({
                "mint": hex(&h.mint),
                "entry_price_fp": h.entry_price_fp,
                "inventory_tokens": h.inventory_tokens,
                "model_managed": h.model_managed,
            })).collect::<Vec<_>>(),
            "pending": pending.iter().map(|p| json!({
                "kind": p.kind,
                "id": p.id,
                "mint": hex(&p.mint),
                "quantity": p.quantity,
                "uncertain": p.uncertain,
            })).collect::<Vec<_>>(),
        });
        match write_atomic(&path, &serde_json::to_string_pretty(&body).unwrap_or_default()) {
            Ok(()) => true,
            Err(_) => {
                self.persist_failures += 1;
                false
            }
        }
    }

    /// Held positions listed in a file at `path` (inspection for a restart).
    #[must_use]
    pub fn read_held(path: &Path) -> Vec<HeldRecord> {
        let Ok(raw) = fs::read_to_string(path) else {
            return Vec::new();
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            return Vec::new();
        };
        v["held"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|h| {
                        Some(HeldRecord {
                            mint: unhex(h["mint"].as_str()?)?,
                            entry_price_fp: h["entry_price_fp"].as_u64()?,
                            inventory_tokens: h["inventory_tokens"].as_u64(),
                            model_managed: h["model_managed"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pq_safety_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.join("SAFETY_STATE.json")
    }

    #[test]
    fn no_file_means_never_tripped_and_an_unreadable_file_means_blocked() {
        let p = tmp("a");
        assert_eq!(SafetyOff::load(&p).0, SafetyLoad::NeverTripped);
        fs::write(&p, "{ not json").unwrap();
        let (l, st) = SafetyOff::load(&p);
        assert!(matches!(l, SafetyLoad::Blocked { .. }));
        assert!(st.blocked, "an untrusted file must never read as armed");
        fs::write(&p, r#"{"schema":2,"blocked":false,"epoch":1}"#).unwrap();
        assert!(SafetyOff::load(&p).1.blocked, "unknown schema is untrusted");
        fs::write(&p, r#"{"schema":1,"epoch":1}"#).unwrap();
        assert!(SafetyOff::load(&p).1.blocked, "missing blocked flag is untrusted");
    }

    #[test]
    fn a_persisted_block_round_trips_with_its_positions_and_orders() {
        let p = tmp("b");
        let (_, mut st) = SafetyOff::load(&p);
        st.blocked = true;
        st.reason = REASON_ENDPOINT_HUNG.into();
        st.epoch = 7;
        let held = [HeldRecord {
            mint: [3; 32],
            entry_price_fp: 45_085,
            inventory_tokens: Some(1_000),
            model_managed: true,
        }];
        let pend = [PendingRecord {
            kind: "exit",
            id: 9,
            mint: [3; 32],
            quantity: 500,
            uncertain: true,
        }];
        assert!(st.persist(&held, &pend));
        let (l, _) = SafetyOff::load(&p);
        assert_eq!(
            l,
            SafetyLoad::Blocked {
                reason: REASON_ENDPOINT_HUNG.into(),
                epoch: 7,
                held: 1,
                pending: 1
            }
        );
        assert_eq!(SafetyOff::read_held(&p), held.to_vec());
    }

    #[test]
    fn a_failed_persist_is_counted_not_swallowed() {
        let mut st = SafetyOff {
            blocked: true,
            path: Some(PathBuf::from("/nonexistent_dir_pq/SAFETY_STATE.json")),
            ..SafetyOff::default()
        };
        assert!(!st.persist(&[], &[]));
        assert_eq!(st.persist_failures, 1);
        assert!(st.blocked);
    }
}
