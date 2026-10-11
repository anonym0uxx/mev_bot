//! Disk budget over EVERY required write destination of the paper daemon, grouped by filesystem (`st_dev`).
//!
//! * Startup: one row per distinct filesystem, the need of every destination on it SUMMED (two paths on one
//!   filesystem are counted once against its free space), the LIMITING row (smallest margin) named, and an
//!   unmeasurable required destination makes the overall verdict UNKNOWN (never PASS, never dropped).
//! * Runtime ([`DiskMonitor`]): through the run AND the drain, samples the bytes actually written per destination
//!   and the free space per filesystem, projects time-to-floor from the measured growth, and stops NEW exposure
//!   (BUY/ADD) once a filesystem gets within the EXIT RESERVE of the hard floor (bytes or time), so reconciliation,
//!   protective execution, durable-state writes and the handoff keep room to finish. Nothing is ever deleted.
//!
//! The per-destination needs are a SCENARIO estimate (23.02 GB total for 6 h + 30 min): the stress rate is the
//! busiest 1-min window of ~26 min of ONE day's capture (proc/RESOURCE_BUDGET.md section 6) at a heavier event
//! mix. It is NOT a guaranteed upper bound; the runtime monitor is what bounds the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a destination holds (report label).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DestKind {
    /// `data/event_stream.jsonl`.
    EventStream,
    /// Trade tape, session history, barrier log.
    Journal,
    /// Flow-history checkpoint (atomic replace: 2 copies on disk at the swap).
    Checkpoint,
    /// Held-position ledger (`PQ_MODEL_HELD_FILE`).
    HeldLedger,
    /// Durable SAFETY_OFF latch (`PQ_MODEL_SAFETY_FILE`).
    SafetyFile,
    /// The daemon's stdout / stderr redirect targets (when they are regular files).
    Log,
    /// Handoff request/ack, live status, model lane report.
    Handoff,
}

/// A required write destination and its need over the run, bytes (SCENARIO estimate, see module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dest {
    /// Kind.
    pub kind: DestKind,
    /// Path written.
    pub path: PathBuf,
    /// Bytes this destination may add over the run (scenario estimate).
    pub need: u64,
}

/// One filesystem's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsRow {
    /// `st_dev` of the filesystem.
    pub dev: u64,
    /// Every destination on it.
    pub paths: Vec<PathBuf>,
    /// Free bytes (statvfs f_bavail * f_frsize).
    pub free: u64,
    /// Summed need of its destinations.
    pub need: u64,
    /// `free - need - floor` (negative = short).
    pub margin: i128,
}

/// Overall verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskVerdict {
    /// Every destination measured and every filesystem's margin >= 0.
    Pass,
    /// Measured and some filesystem's margin < 0.
    Short,
    /// Some required destination could not be measured. Never PASS.
    Unknown,
}

/// The per-filesystem table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskTable {
    /// One row per distinct filesystem, by `st_dev`.
    pub rows: Vec<FsRow>,
    /// Required destinations that could not be measured (kept, never dropped).
    pub unmeasurable: Vec<PathBuf>,
    /// Index into `rows` of the LIMITING filesystem (smallest margin).
    pub limiting: Option<usize>,
    /// Overall verdict.
    pub verdict: DiskVerdict,
}

/// `(st_dev, free bytes)` of the filesystem that holds `path` (its parent directory when `path` is not a
/// directory). `None` = unmeasurable (the directory does not exist or statvfs fails).
#[must_use]
pub fn probe_fs(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let dir = if path.is_dir() {
        path
    } else {
        match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        }
    };
    let dev = std::fs::metadata(dir).ok()?.dev();
    Some((dev, crate::model_lifecycle::free_bytes(dir)?))
}

/// Pure table: group `dests` by the probed filesystem, sum the need per filesystem, margin against `floor`.
#[must_use]
pub fn disk_table(
    dests: &[Dest],
    probe: &dyn Fn(&Path) -> Option<(u64, u64)>,
    floor: u64,
) -> DiskTable {
    let mut by_dev: BTreeMap<u64, FsRow> = BTreeMap::new();
    let mut unmeasurable = Vec::new();
    for d in dests {
        match probe(&d.path) {
            None => unmeasurable.push(d.path.clone()),
            Some((dev, free)) => {
                let r = by_dev.entry(dev).or_insert(FsRow {
                    dev,
                    paths: Vec::new(),
                    free,
                    need: 0,
                    margin: 0,
                });
                r.free = r.free.min(free);
                r.need = r.need.saturating_add(d.need);
                r.paths.push(d.path.clone());
            }
        }
    }
    let mut rows: Vec<FsRow> = by_dev.into_values().collect();
    for r in &mut rows {
        r.margin = i128::from(r.free)
            .saturating_sub(i128::from(r.need))
            .saturating_sub(i128::from(floor));
    }
    let limiting = rows
        .iter()
        .enumerate()
        .min_by_key(|(_, r)| r.margin)
        .map(|(i, _)| i);
    let verdict = if !unmeasurable.is_empty() {
        DiskVerdict::Unknown
    } else if rows.iter().any(|r| r.margin < 0) {
        DiskVerdict::Short
    } else {
        DiskVerdict::Pass
    };
    DiskTable {
        rows,
        unmeasurable,
        limiting,
        verdict,
    }
}

/// Split of [`crate::model_lifecycle::NON_STREAM_BYTES_6H`] (0.5 GB) over the non-stream destinations, bytes.
/// Checkpoint 0.34 GB (2 copies x 10x growth margin), held ledger 0.04 GB, journals 0.05 GB, logs 0.06 GB,
/// safety + handoff/report 0.01 GB. Sums to exactly 0.5 GB (pinned by a test).
pub const NEED_CHECKPOINT: u64 = 340_000_000;
/// Held-position ledger.
pub const NEED_HELD: u64 = 40_000_000;
/// Journals (tape, session history, barrier log), all together.
pub const NEED_JOURNALS: u64 = 50_000_000;
/// stdout + stderr redirect targets, together.
pub const NEED_LOGS: u64 = 60_000_000;
/// Safety latch file.
pub const NEED_SAFETY: u64 = 1_000_000;
/// Handoff request/ack, live status, model lane report, together.
pub const NEED_HANDOFF: u64 = 9_000_000;

/// The daemon's required write destinations for a run of `run_s` real-time seconds. `logs` are the stdout/stderr
/// redirect targets that are regular files (pipes/ttys are not disk destinations). The event stream's need is the
/// STRESS-rate SCENARIO estimate (not a guaranteed bound).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn required_destinations(
    event_stream: &Path,
    journals: &[&Path],
    checkpoint: &Path,
    held: &Path,
    safety: &Path,
    logs: &[PathBuf],
    handoff: &[&Path],
    run_s: u64,
) -> Vec<Dest> {
    let share = |total: u64, n: usize| total.checked_div(n.max(1) as u64).unwrap_or(0);
    let mut v = vec![Dest {
        kind: DestKind::EventStream,
        path: event_stream.to_path_buf(),
        need: crate::model_lifecycle::STREAM_RATE
            .stress_bps
            .saturating_mul(run_s),
    }];
    for j in journals {
        v.push(Dest {
            kind: DestKind::Journal,
            path: j.to_path_buf(),
            need: share(NEED_JOURNALS, journals.len()),
        });
    }
    v.push(Dest {
        kind: DestKind::Checkpoint,
        path: checkpoint.to_path_buf(),
        need: NEED_CHECKPOINT,
    });
    v.push(Dest {
        kind: DestKind::HeldLedger,
        path: held.to_path_buf(),
        need: NEED_HELD,
    });
    v.push(Dest {
        kind: DestKind::SafetyFile,
        path: safety.to_path_buf(),
        need: NEED_SAFETY,
    });
    for l in logs {
        v.push(Dest {
            kind: DestKind::Log,
            path: l.clone(),
            need: share(NEED_LOGS, logs.len()),
        });
    }
    for h in handoff {
        v.push(Dest {
            kind: DestKind::Handoff,
            path: h.to_path_buf(),
            need: share(NEED_HANDOFF, handoff.len()),
        });
    }
    v
}

/// Startup gate from the table's verdict: `(disk input for the stop table, named alert)`. PASS arms new exposure;
/// SHORT and UNKNOWN do NOT (the caller blocks entries before the first tick); UNKNOWN stays `None` (never PASS).
#[must_use]
pub fn startup_disk_gate(v: DiskVerdict) -> (Option<bool>, Option<&'static str>) {
    match v {
        DiskVerdict::Pass => (Some(true), None),
        DiskVerdict::Short => (Some(false), Some("ALERT_DISK_BUDGET_SHORT")),
        DiskVerdict::Unknown => (None, Some("ALERT_DISK_BUDGET_UNKNOWN")),
    }
}

/// This process's stdout/stderr redirect targets that are regular files (`/proc/self/fd/{1,2}`), deduplicated.
#[must_use]
pub fn std_redirect_targets() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for fd in [1, 2] {
        if let Ok(t) = std::fs::read_link(format!("/proc/self/fd/{fd}")) {
            if t.is_absolute() && t.is_file() && !v.contains(&t) {
                v.push(t);
            }
        }
    }
    v
}

/// Render the table for the startup log (one line per filesystem, the limiting one marked).
#[must_use]
pub fn render_table(t: &DiskTable) -> Vec<String> {
    let mut out = Vec::new();
    for (i, r) in t.rows.iter().enumerate() {
        out.push(format!(
            "DISK_FS{} dev={} free_bytes={} need_bytes={} margin_bytes={} paths={:?}",
            if Some(i) == t.limiting {
                " LIMITING"
            } else {
                ""
            },
            r.dev,
            r.free,
            r.need,
            r.margin,
            r.paths
        ));
    }
    for p in &t.unmeasurable {
        out.push(format!(
            "DISK_FS UNMEASURABLE path={} -> UNKNOWN",
            p.display()
        ));
    }
    out.push(format!(
        "DISK_VERDICT {:?} (need = SCENARIO estimate from ~26 min of one day's capture, not a guaranteed bound)",
        t.verdict
    ));
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Runtime monitoring (run + drain).
// ---------------------------------------------------------------------------------------------------------------

/// EXIT RESERVE, bytes above the HARD floor that new exposure may never eat into: the 30 min drain at the stress
/// rate (962_505 B/s x 1_800 s = 1.73 GB) + the whole non-stream allowance (0.5 GB: checkpoints, held ledger,
/// safety, journals, logs, handoff) = 2.23 GB, rounded up to 2.25 GB. Reconciliation, protective execution,
/// durable-state writes and the handoff run inside it.
pub const EXIT_RESERVE_BYTES: u64 = 2_250_000_000;

/// EXIT RESERVE, time: new exposure stops while the projected time-to-hard-floor is below the drain bound (30 min)
/// plus a 10 min margin.
pub const EXIT_RESERVE_SECS: u64 = 2_400;

/// One filesystem's runtime sample.
#[derive(Debug, Clone, PartialEq)]
pub struct FsSample {
    /// `st_dev`.
    pub dev: u64,
    /// Free now.
    pub free: u64,
    /// Bytes this daemon wrote to it since the monitor started (sum over its destinations' size growth).
    pub written: u64,
    /// Measured consumption rate, bytes/s: the larger of the free-space drop rate and this daemon's write rate.
    pub rate_bps: f64,
    /// Projected seconds until `free` reaches the hard floor at `rate_bps` (`None` = no measured growth).
    pub time_to_floor_s: Option<f64>,
    /// Inside the exit reserve (bytes or time).
    pub in_reserve: bool,
}

/// Runtime verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskRuntime {
    /// One sample per filesystem.
    pub fs: Vec<FsSample>,
    /// Required destinations unmeasurable now (UNKNOWN; counts as a breach for new exposure).
    pub unmeasurable: Vec<PathBuf>,
    /// New exposure allowed: every destination measured, every fs at/above the soft floor and outside the
    /// exit reserve. `Some(false)` on a breach; `None` = UNKNOWN (also refuses BUY/ADD).
    pub soft_ok: Option<bool>,
    /// Every fs at/above the hard floor (`None` = some destination unmeasurable: does NOT latch).
    pub hard_ok: Option<bool>,
}

/// Runtime disk monitor. Holds the first sample (baseline) per destination and per filesystem.
#[derive(Debug, Clone, Default)]
pub struct DiskMonitor {
    base_size: BTreeMap<PathBuf, u64>,
    base_free: BTreeMap<u64, (f64, u64)>,
}

/// One destination reading: `(path, Some((dev, free)) | None, current size)`.
pub type DiskReading = (PathBuf, Option<(u64, u64)>, u64);

/// Pure runtime evaluation of one sample.
/// `readings`: per destination `(path, Some((dev, free)) | None, current size)`; `now_s`: monotonic seconds.
#[must_use]
pub fn evaluate_runtime(
    mon: &mut DiskMonitor,
    readings: &[DiskReading],
    now_s: f64,
    soft_floor: u64,
    hard_floor: u64,
    reserve_bytes: u64,
    reserve_secs: u64,
) -> DiskRuntime {
    let mut per_dev: BTreeMap<u64, (u64, u64)> = BTreeMap::new(); // dev -> (free, written)
    let mut unmeasurable = Vec::new();
    for (p, fs, size) in readings {
        let base = *mon.base_size.entry(p.clone()).or_insert(*size);
        match fs {
            None => unmeasurable.push(p.clone()),
            Some((dev, free)) => {
                let e = per_dev.entry(*dev).or_insert((*free, 0));
                e.0 = e.0.min(*free);
                e.1 = e.1.saturating_add(size.saturating_sub(base));
            }
        }
    }
    let mut fs_out = Vec::new();
    for (dev, (free, written)) in per_dev {
        let (t0, f0) = *mon.base_free.entry(dev).or_insert((now_s, free));
        let dt = now_s - t0;
        let rate = if dt > 0.0 {
            let drop = f0.saturating_sub(free) as f64 / dt;
            let ours = written as f64 / dt;
            drop.max(ours)
        } else {
            0.0
        };
        let above = free.saturating_sub(hard_floor) as f64;
        let ttf = (rate > 0.0).then(|| above / rate);
        let in_reserve = free < hard_floor.saturating_add(reserve_bytes)
            || ttf.is_some_and(|t| t < reserve_secs as f64);
        fs_out.push(FsSample {
            dev,
            free,
            written,
            rate_bps: rate,
            time_to_floor_s: ttf,
            in_reserve,
        });
    }
    let (soft_ok, hard_ok) = if !unmeasurable.is_empty() || fs_out.is_empty() {
        (None, None)
    } else {
        (
            Some(fs_out.iter().all(|f| f.free >= soft_floor && !f.in_reserve)),
            Some(fs_out.iter().all(|f| f.free >= hard_floor)),
        )
    };
    DiskRuntime {
        fs: fs_out,
        unmeasurable,
        soft_ok,
        hard_ok,
    }
}

impl DiskMonitor {
    /// Sample the live filesystem for `dests` and evaluate against the stop-table floors and the exit reserve.
    /// `simulated`: HARNESS-ONLY overrides `(free_override, unknown_path)`; the caller logs them as
    /// SIMULATED_PRESSURE.
    pub fn sample(
        &mut self,
        dests: &[Dest],
        now_s: f64,
        simulated_free: Option<u64>,
        simulated_unknown: bool,
    ) -> DiskRuntime {
        let readings: Vec<DiskReading> = dests
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let size = std::fs::metadata(&d.path).map(|m| m.len()).unwrap_or(0);
                let mut fs = probe_fs(&d.path);
                if simulated_unknown && i == 0 {
                    fs = None;
                }
                if let (Some(f), Some((dev, _))) = (simulated_free, fs) {
                    fs = Some((dev, f));
                }
                (d.path.clone(), fs, size)
            })
            .collect();
        evaluate_runtime(
            self,
            &readings,
            now_s,
            pump_quant_app::stop_policy::DISK_SOFT_FLOOR_BYTES,
            pump_quant_app::stop_policy::DISK_HARD_FLOOR_BYTES,
            EXIT_RESERVE_BYTES,
            EXIT_RESERVE_SECS,
        )
    }
}
