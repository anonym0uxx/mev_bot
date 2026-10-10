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

/// Incremental sealer of an append-only capture file.
pub struct CaptureSealer {
    capture: PathBuf,
    seal: PathBuf,
    hasher: Sha256,
    hashed: u64,
}

impl CaptureSealer {
    #[must_use]
    pub fn new(capture: &Path, seal: &Path) -> Self {
        Self {
            capture: capture.to_path_buf(),
            seal: seal.to_path_buf(),
            hasher: Sha256::new(),
            hashed: 0,
        }
    }

    /// Hash the bytes appended since the last seal and write the seal atomically. Call only after the capture writer
    /// was flushed and the held ledger persisted (so `through_ms`/`generation` describe what is durable).
    pub fn seal(&mut self, lineage: &str, generation: u64, through_ms: i64) -> std::io::Result<()> {
        let mut f = std::fs::File::open(&self.capture)?;
        let len = f.metadata()?.len();
        if len < self.hashed {
            return Err(std::io::Error::other("capture shrank"));
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
        let sha = hex(&self.hasher.clone().finalize());
        let body = serde_json::json!({
            "schema": 1,
            "lineage": lineage,
            "held_generation": generation,
            "through_ms": through_ms,
            "capture_bytes": self.hashed,
            "sha256": sha,
        })
        .to_string();
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
