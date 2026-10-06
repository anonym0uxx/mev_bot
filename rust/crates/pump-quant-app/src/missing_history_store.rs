//! Durable missing-history ledger: the unresolved gaps survive a process restart.
//!
//! Contract
//! * ONE small JSON file (schema 1), written atomically (temp, fsync, rename, fsync dir).
//! * LOAD never returns "no gap" for a file it cannot trust: unreadable / wrong schema / bad
//!   record => [`StoreLoad::Untrusted`], which the engine turns into the named refusal
//!   `join_history_continuity_unknown` for every prompt.
//! * A MISSING file is `NeverWritten` (clean first start). The engine additionally treats a
//!   missing file as untrusted when exposure was restored from the held ledger (a deleted record
//!   must not read as "no gap").
//! * WRITES happen on a background thread with a latest-wins slot, so serialising a snapshot is
//!   the only work on the engine thread. Feed processing and protection never wait on disk.
//!   Failures are counted and observable; the in-memory gap keeps refusing regardless.
//! * Acknowledging or persisting a gap is not repairing it: nothing here clears a gap.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::decision_join::{MissingDeps, MissingKind, MissingObservation};

pub const SCHEMA: u64 = 1;

/// Result of reading the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreLoad {
    /// No file: nothing was ever written here.
    NeverWritten,
    /// A trusted file and its unresolved records.
    Records(Vec<([u8; 32], MissingObservation)>),
    /// Present but not trustworthy (reason is a stable label).
    Untrusted(&'static str),
}

fn hex(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
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

/// Serialise the unresolved records (deterministic order: the caller's order).
#[must_use]
pub fn encode(records: &[([u8; 32], MissingObservation)]) -> String {
    let recs: Vec<Value> = records
        .iter()
        .map(|(m, o)| {
            json!({
                "mint": hex(m),
                "drop_ms": o.drop_ms,
                "kind": o.kind.as_str(),
                "count": o.count,
                "source_id": o.source_id,
                "deps": {
                    "rolling_300s": o.deps.rolling_300s,
                    "ledger_cumulative": o.deps.ledger_cumulative,
                    "holder_enrichment": o.deps.holder_enrichment,
                    "wallet_derived_uncertain": o.deps.wallet_derived_uncertain,
                },
            })
        })
        .collect();
    serde_json::to_string(&json!({ "schema": SCHEMA, "records": recs })).unwrap_or_default()
}

/// Parse a ledger body. Any structural problem is `Untrusted`.
#[must_use]
pub fn decode(body: &str) -> StoreLoad {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return StoreLoad::Untrusted("json");
    };
    if v.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
        return StoreLoad::Untrusted("schema");
    }
    let Some(arr) = v.get("records").and_then(Value::as_array) else {
        return StoreLoad::Untrusted("records");
    };
    let mut out = Vec::with_capacity(arr.len());
    for r in arr {
        let Some(mint) = r.get("mint").and_then(Value::as_str).and_then(unhex) else {
            return StoreLoad::Untrusted("mint");
        };
        let Some(drop_ms) = r.get("drop_ms").and_then(Value::as_i64) else {
            return StoreLoad::Untrusted("drop_ms");
        };
        let kind = match r.get("kind").and_then(Value::as_str) {
            Some("possible_trade") => MissingKind::PossibleTrade,
            Some("invalid_observation_unknown") => MissingKind::InvalidObservation,
            _ => return StoreLoad::Untrusted("kind"),
        };
        let Some(count) = r
            .get("count")
            .and_then(Value::as_u64)
            .and_then(|c| u32::try_from(c).ok())
        else {
            return StoreLoad::Untrusted("count");
        };
        let Some(source_id) = r.get("source_id").and_then(Value::as_str) else {
            return StoreLoad::Untrusted("source_id");
        };
        let Some(d) = r.get("deps") else {
            return StoreLoad::Untrusted("deps");
        };
        let flag = |k: &str| d.get(k).and_then(Value::as_bool);
        let (Some(a), Some(b), Some(c), Some(e)) = (
            flag("rolling_300s"),
            flag("ledger_cumulative"),
            flag("holder_enrichment"),
            flag("wallet_derived_uncertain"),
        ) else {
            return StoreLoad::Untrusted("deps");
        };
        out.push((
            mint,
            MissingObservation {
                drop_ms,
                kind,
                count,
                source_id: source_id.to_string(),
                deps: MissingDeps {
                    rolling_300s: a,
                    ledger_cumulative: b,
                    holder_enrichment: c,
                    wallet_derived_uncertain: e,
                },
                receipt: None,
            },
        ));
    }
    StoreLoad::Records(out)
}

/// Read the ledger at `path`.
#[must_use]
pub fn load(path: &Path) -> StoreLoad {
    match fs::read_to_string(path) {
        Ok(body) => decode(&body),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreLoad::NeverWritten,
        Err(_) => StoreLoad::Untrusted("io"),
    }
}

/// Atomic durable write (temp, fsync, rename, fsync dir).
///
/// # Errors
/// The io error.
pub fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
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

struct Slot {
    pending: Option<(u64, String)>,
    shutdown: bool,
}

/// Background latest-wins writer. `submit` never blocks on disk.
pub struct Writer {
    path: PathBuf,
    slot: Arc<(Mutex<Slot>, Condvar)>,
    /// Sequence of the newest snapshot handed in.
    submitted: AtomicU64,
    /// Sequence of the newest snapshot durably written.
    written: Arc<AtomicU64>,
    /// Consecutive failed writes (reset by a success).
    consecutive_failures: Arc<AtomicU64>,
    total_failures: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer").field("path", &self.path).finish()
    }
}

impl Writer {
    /// Start the writer for `path`. `fail_hook` (tests) can force a write to fail.
    #[must_use]
    pub fn start(path: PathBuf) -> Self {
        Self::start_with(path, None)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn start_with(
        path: PathBuf,
        fail_hook: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> Self {
        let slot = Arc::new((
            Mutex::new(Slot {
                pending: None,
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let written = Arc::new(AtomicU64::new(0));
        let consecutive_failures = Arc::new(AtomicU64::new(0));
        let total_failures = Arc::new(AtomicU64::new(0));
        let (s2, w2, c2, t2, p2) = (
            Arc::clone(&slot),
            Arc::clone(&written),
            Arc::clone(&consecutive_failures),
            Arc::clone(&total_failures),
            path.clone(),
        );
        let handle = std::thread::Builder::new()
            .name("missing-history-writer".into())
            .spawn(move || loop {
                let job = {
                    let (m, cv) = &*s2;
                    let Ok(mut g) = m.lock() else { return };
                    while g.pending.is_none() && !g.shutdown {
                        g = match cv.wait(g) {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                    }
                    match g.pending.take() {
                        Some(j) => j,
                        None => return, // shutdown with nothing pending
                    }
                };
                let forced = fail_hook.as_ref().is_some_and(|f| f.load(Ordering::SeqCst));
                let r = if forced {
                    Err(std::io::Error::other("forced failure"))
                } else {
                    write_atomic(&p2, &job.1)
                };
                match r {
                    Ok(()) => {
                        w2.store(job.0, Ordering::SeqCst);
                        c2.store(0, Ordering::SeqCst);
                    }
                    Err(_) => {
                        c2.fetch_add(1, Ordering::SeqCst);
                        t2.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
            .ok();
        Self {
            path,
            slot,
            submitted: AtomicU64::new(0),
            written,
            consecutive_failures,
            total_failures,
            handle,
        }
    }

    /// Hand the newest snapshot to the writer (replaces an unwritten older one). Returns its seq.
    pub fn submit(&self, body: String) -> u64 {
        let seq = self.submitted.fetch_add(1, Ordering::SeqCst) + 1;
        let (m, cv) = &*self.slot;
        if let Ok(mut g) = m.lock() {
            g.pending = Some((seq, body));
            cv.notify_one();
        }
        seq
    }

    #[must_use]
    pub fn written_seq(&self) -> u64 {
        self.written.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn total_failures(&self) -> u64 {
        self.total_failures.load(Ordering::SeqCst)
    }

    /// Block until snapshot `seq` is durable, or fail (shutdown / tests only; never the tick path).
    pub fn wait_durable(&self, seq: u64, timeout: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if self.written_seq() >= seq {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.written_seq() >= seq
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        {
            let (m, cv) = &*self.slot;
            if let Ok(mut g) = m.lock() {
                g.shutdown = true;
                cv.notify_one();
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(drop_ms: i64) -> MissingObservation {
        MissingObservation {
            drop_ms,
            kind: MissingKind::PossibleTrade,
            count: 3,
            source_id: "slot:9".into(),
            deps: MissingDeps {
                rolling_300s: true,
                ledger_cumulative: false,
                holder_enrichment: true,
                wallet_derived_uncertain: true,
            },
            receipt: None,
        }
    }

    #[test]
    fn round_trips_every_field_including_the_dependency_union() {
        let recs = vec![([7u8; 32], obs(10)), ([8u8; 32], obs(20))];
        assert_eq!(decode(&encode(&recs)), StoreLoad::Records(recs));
    }

    #[test]
    fn anything_untrustworthy_is_untrusted_never_empty() {
        assert_eq!(decode("{ nope"), StoreLoad::Untrusted("json"));
        assert_eq!(
            decode(r#"{"schema":2,"records":[]}"#),
            StoreLoad::Untrusted("schema")
        );
        assert_eq!(decode(r#"{"schema":1}"#), StoreLoad::Untrusted("records"));
        let mut v: Value = serde_json::from_str(&encode(&[([1u8; 32], obs(1))])).unwrap();
        v["records"][0]["deps"]
            .as_object_mut()
            .unwrap()
            .remove("holder_enrichment");
        assert_eq!(decode(&v.to_string()), StoreLoad::Untrusted("deps"));
        v["records"][0]["kind"] = json!("mystery");
        assert_eq!(decode(&v.to_string()), StoreLoad::Untrusted("kind"));
    }
}
