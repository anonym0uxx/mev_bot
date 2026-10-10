//! RESTART launch recovery, daemon side: a SEALED capture of the market-evidence events the engine applied, bound to
//! the held-ledger lineage and the decision clock, and the startup pass that rebuilds the decision cache from it.
//!
//! * The seal (`data/capture_seal.json`, atomic) is written at each durable barrier AFTER the capture is flushed and the
//!   held ledger is persisted: `{lineage, held_generation, through_ms, capture_bytes, sha256}`. The sha256 is over the
//!   capture prefix `[0, capture_bytes)`; it is maintained incrementally (only new bytes are hashed).
//! * Startup (after the held restore and the flow attach, before the first tick): the seal must name the restored
//!   lineage, its `through_ms` must reach the restored `decision.through_ms`, and the prefix must hash to the seal.
//!   Then every event of the prefix with receive time `<= restart_ms` is applied through
//!   `Engine::model_rehydrate_event` in capture order. Anything else is a NAMED refusal: nothing is applied, every
//!   launch before the restart that is not covered refuses `join_tape_truncated_at_restart`, and the lost coverage
//!   (mints + launch times) is reported. That is never called parity.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use pump_quant_app::engine::model_rehydrate::RehydrateStats;
use pump_quant_app::engine::Engine;
use sha2::{Digest, Sha256};

/// Default seal location (next to the capture).
pub const DEFAULT_CAPTURE_SEAL_FILE: &str = "data/capture_seal.json";

/// Capture seal schema written by a sealer that also seals the curve-binding log (`curve_binding`). Schema 1 seals
/// (no binding log) stay readable: they restore the capture exactly as before and report the bindings NOT RECORDED.
pub const CAPTURE_SEAL_SCHEMA: u64 = 2;

/// Incremental sha256 of an append-only file prefix.
struct PrefixHash {
    path: PathBuf,
    hasher: Sha256,
    hashed: u64,
}

impl PrefixHash {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            hasher: Sha256::new(),
            hashed: 0,
        }
    }

    /// Hash the bytes appended since the last call; returns (bytes, sha256 hex). With `absent_ok`, a file that never
    /// existed is an empty prefix (a binding log before its first record); the capture itself must exist.
    fn advance(&mut self, absent_ok: bool) -> std::io::Result<(u64, String)> {
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if absent_ok && e.kind() == std::io::ErrorKind::NotFound && self.hashed == 0 => {
                return Ok((0, hex(&self.hasher.clone().finalize())));
            }
            Err(e) => return Err(e),
        };
        let len = f.metadata()?.len();
        if len < self.hashed {
            return Err(std::io::Error::other("sealed file shrank"));
        }
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(self.hashed))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut left = len - self.hashed;
        while left > 0 {
            let n = f.read(&mut buf[..usize::try_from(left.min(1 << 20)).unwrap_or(1 << 20)])?;
            if n == 0 {
                break;
            }
            self.hasher.update(&buf[..n]);
            left -= n as u64;
            self.hashed += n as u64;
        }
        Ok((self.hashed, hex(&self.hasher.clone().finalize())))
    }
}

/// Incremental sealer of an append-only capture file (and, from schema 2, of the curve-binding log).
pub struct CaptureSealer {
    seal: PathBuf,
    capture: PrefixHash,
    bindings: Option<PrefixHash>,
}

impl CaptureSealer {
    /// A schema-1 sealer (capture only). Kept for callers without a binding log.
    #[must_use]
    pub fn new(capture: &Path, seal: &Path) -> Self {
        Self {
            seal: seal.to_path_buf(),
            capture: PrefixHash::new(capture),
            bindings: None,
        }
    }

    /// A schema-2 sealer: the curve-binding log is sealed with the capture, at the same barrier.
    #[must_use]
    pub fn with_bindings(capture: &Path, seal: &Path, bindings: &Path) -> Self {
        Self {
            seal: seal.to_path_buf(),
            capture: PrefixHash::new(capture),
            bindings: Some(PrefixHash::new(bindings)),
        }
    }

    /// Hash the bytes appended since the last seal and write the seal atomically. Call only after the capture writer
    /// (and binding writer) was flushed and the held ledger persisted (so `through_ms`/`generation` describe what is
    /// durable).
    pub fn seal(&mut self, lineage: &str, generation: u64, through_ms: i64) -> std::io::Result<()> {
        let (capture_bytes, sha) = self.capture.advance(false)?;
        let mut body = serde_json::json!({
            "schema": 1,
            "lineage": lineage,
            "held_generation": generation,
            "through_ms": through_ms,
            "capture_bytes": capture_bytes,
            "sha256": sha,
        });
        if let Some(b) = self.bindings.as_mut() {
            let (n, s) = b.advance(true)?;
            body["schema"] = serde_json::json!(CAPTURE_SEAL_SCHEMA);
            body["bindings_bytes"] = serde_json::json!(n);
            body["bindings_sha256"] = serde_json::json!(s);
        }
        let body = body.to_string();
        let tmp = self.seal.with_extension("tmp");
        {
            let mut t = std::fs::File::create(&tmp)?;
            t.write_all(body.as_bytes())?;
            t.sync_all()?;
        }
        std::fs::rename(&tmp, &self.seal)
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What the startup pass did.
#[derive(Debug)]
pub enum RehydrateOutcome {
    /// Not a restart (no held ledger restored): nothing to do.
    NotARestart,
    /// The sealed capture covered the interval up to the restart instant and was applied.
    Applied {
        restart_ms: i64,
        covered_from_ms: i64,
        stats: RehydrateStats,
        lost: Vec<([u8; 32], i64)>,
    },
    /// Named refusal: nothing was applied. `lost` = launches before the restart whose evidence is not recovered.
    Refused {
        why: String,
        restart_ms: i64,
        lost: Vec<([u8; 32], i64)>,
    },
}

/// Startup pass (see module doc). `capture`/`seal` are the PREVIOUS process's files (read-only).
pub fn rehydrate_from_capture(
    engine: &mut Engine,
    capture: &Path,
    seal: &Path,
) -> RehydrateOutcome {
    if !engine.model_held_restored_from_file() {
        return RehydrateOutcome::NotARestart;
    }
    let restart_ms = engine.model_replay_through_ms();
    match load_verified(engine, capture, seal, restart_ms) {
        Ok(lines) => {
            let covered_from = first_recv(&lines).unwrap_or(i64::MAX);
            engine.model_rehydrate_begin(restart_ms, covered_from);
            let mut stats = RehydrateStats::default();
            for (_, ev) in &lines {
                engine.model_rehydrate_event(ev, &mut stats);
            }
            RehydrateOutcome::Applied {
                restart_ms,
                covered_from_ms: covered_from,
                stats,
                lost: engine.model_restart_lost_coverage(),
            }
        }
        Err(why) => {
            engine.model_rehydrate_begin(restart_ms, i64::MAX);
            RehydrateOutcome::Refused {
                why,
                restart_ms,
                lost: engine.model_restart_lost_coverage(),
            }
        }
    }
}

type Lines = Vec<(String, pump_quant_app::event::AppEvent)>;

fn first_recv(lines: &Lines) -> Option<i64> {
    use pump_quant_app::event::AppEvent as E;
    lines.iter().find_map(|(_, e)| match e {
        E::MarketTrade { recv_unix_ms, .. }
        | E::CorpusFlowRow { recv_unix_ms, .. }
        | E::CurveObserved { recv_unix_ms, .. }
        | E::AmmSwap { recv_unix_ms, .. } => *recv_unix_ms,
        E::LaunchObserved { launch_unix_ms, .. } => Some(*launch_unix_ms),
        _ => None,
    })
}

fn load_verified(
    engine: &Engine,
    capture: &Path,
    seal: &Path,
    restart_ms: i64,
) -> Result<Lines, String> {
    let s = std::fs::read_to_string(seal).map_err(|_| "capture_seal_absent".to_string())?;
    let v: serde_json::Value =
        serde_json::from_str(&s).map_err(|_| "capture_seal_malformed".to_string())?;
    let lineage = v["lineage"].as_str().ok_or("capture_seal_malformed")?;
    if lineage.is_empty() || lineage != engine.model_held_lineage() {
        return Err("capture_seal_lineage_mismatch".into());
    }
    let through = v["through_ms"].as_i64().ok_or("capture_seal_malformed")?;
    if through < restart_ms {
        return Err(format!(
            "capture_seal_behind_restart:{through}<{restart_ms}"
        ));
    }
    let n = v["capture_bytes"]
        .as_u64()
        .ok_or("capture_seal_malformed")?;
    let want = v["sha256"].as_str().ok_or("capture_seal_malformed")?;
    let mut f = std::fs::File::open(capture).map_err(|_| "capture_absent".to_string())?;
    let mut buf = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    (&mut f)
        .take(n)
        .read_to_end(&mut buf)
        .map_err(|_| "capture_unreadable".to_string())?;
    if buf.len() as u64 != n {
        return Err("capture_shorter_than_seal".into());
    }
    if hex(&Sha256::digest(&buf)) != want {
        return Err("capture_sha256_mismatch".into());
    }
    let text = String::from_utf8(buf).map_err(|_| "capture_not_utf8".to_string())?;
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        match crate::event_codec::decode(l) {
            Ok(x) => out.push(x),
            Err(e) => return Err(format!("capture_line_undecodable:{}:{e}", i + 1)),
        }
    }
    Ok(out)
}

fn aside_name(capture: &Path, what: &str) -> PathBuf {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    capture.with_extension(format!("{what}.{ms}.jsonl"))
}

/// After an APPLIED rehydration: the bytes after the sealed prefix (written after the last seal, before the crash) are
/// MOVED to a side file (kept as evidence, never replayed) so the capture stays one causal sequence that the next seal
/// extends. Returns the side file, if there was a tail.
pub fn set_aside_unsealed_tail(capture: &Path, seal: &Path) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(seal).ok()?).ok()?;
    let n = v["capture_bytes"].as_u64()?;
    let len = std::fs::metadata(capture).ok()?.len();
    if len <= n {
        return None;
    }
    let mut f = std::fs::File::open(capture).ok()?;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(n)).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let side = aside_name(capture, "unsealed_tail");
    {
        let mut t = std::fs::File::create(&side).ok()?;
        t.write_all(&tail).ok()?;
        t.sync_all().ok()?;
    }
    let f = std::fs::OpenOptions::new().write(true).open(capture).ok()?;
    f.set_len(n).ok()?;
    f.sync_all().ok()?;
    Some(side)
}

/// After an APPLIED capture rehydration: restore the durable mint -> curve bindings (`curve_binding`) the previous
/// process validated at or before the restart instant. The seal was already verified for lineage and barrier by
/// `rehydrate_from_capture`; this re-reads it and verifies the binding-log prefix against `bindings_bytes` /
/// `bindings_sha256`. Schema 1 (an older seal) is readable and restores nothing, by name; an unknown schema, a
/// tampered or short log, or an invalid record is a named refusal that restores nothing.
pub fn restore_curve_bindings(
    engine: &Engine,
    seal: &Path,
    bindings: &Path,
) -> crate::curve_binding::BindingRestore {
    use crate::curve_binding::BindingRestore as R;
    let restart_ms = engine.model_replay_through_ms();
    let v: serde_json::Value = match std::fs::read_to_string(seal)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(v) => v,
        None => return R::Refused("capture_seal_unreadable".into()),
    };
    if v["lineage"].as_str() != Some(engine.model_held_lineage()) {
        return R::Refused("capture_seal_lineage_mismatch".into());
    }
    match v["schema"].as_u64() {
        Some(1) => return R::NotRecordedSchema1,
        Some(CAPTURE_SEAL_SCHEMA) => {}
        other => return R::Refused(format!("capture_seal_schema_unsupported:{other:?}")),
    }
    let (Some(n), Some(want)) = (v["bindings_bytes"].as_u64(), v["bindings_sha256"].as_str())
    else {
        return R::Refused("capture_seal_malformed:bindings".into());
    };
    let mut buf = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    if n > 0 {
        let Ok(f) = std::fs::File::open(bindings) else {
            return R::Refused("curve_bindings_absent".into());
        };
        if f.take(n).read_to_end(&mut buf).is_err() {
            return R::Refused("curve_bindings_unreadable".into());
        }
    }
    if buf.len() as u64 != n {
        return R::Refused("curve_bindings_shorter_than_seal".into());
    }
    if hex(&Sha256::digest(&buf)) != want {
        return R::Refused("curve_bindings_sha256_mismatch".into());
    }
    let Ok(text) = String::from_utf8(buf) else {
        return R::Refused("curve_bindings_not_utf8".into());
    };
    match crate::curve_binding::bindings_through(&text, restart_ms) {
        Ok((b, after, undated)) => R::Restored {
            bindings: b,
            after_restart_skipped: after,
            undated_skipped: undated,
        },
        Err(e) => R::Refused(e),
    }
}

/// After a restore from a schema-2 seal: binding records after the sealed prefix are MOVED aside (evidence, never
/// restored) so the log stays one causal sequence the next seal extends. Returns the side file, if any.
pub fn set_aside_unsealed_binding_tail(bindings: &Path, seal: &Path) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(seal).ok()?).ok()?;
    let n = v["bindings_bytes"].as_u64()?;
    let len = std::fs::metadata(bindings).ok()?.len();
    if len <= n {
        return None;
    }
    let mut f = std::fs::File::open(bindings).ok()?;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(n)).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let side = aside_name(bindings, "unsealed_tail");
    {
        let mut t = std::fs::File::create(&side).ok()?;
        t.write_all(&tail).ok()?;
        t.sync_all().ok()?;
    }
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(bindings)
        .ok()?;
    f.set_len(n).ok()?;
    f.sync_all().ok()?;
    Some(side)
}

/// A capture that could not be verified (or that belongs to no restored ledger) is MOVED aside whole, so the new
/// process seals only what it captures itself. Returns the side file, if any.
pub fn set_aside_unverified(capture: &Path) -> Option<PathBuf> {
    if !capture.exists() {
        return None;
    }
    let side = aside_name(capture, "unverified");
    std::fs::rename(capture, &side).ok()?;
    Some(side)
}
