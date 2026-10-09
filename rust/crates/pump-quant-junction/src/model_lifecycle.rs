//! Daemon-side lifecycle of the paper model lane: arm it with the PRODUCTION client, restore the durable
//! SAFETY_OFF latch, and decide whether a stop request may complete.
//!
//! Kept in the library (not inline in `pq_daemon`) so the exact code the daemon runs is what the tests
//! exercise. Paper only: nothing here submits anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use pump_quant_app::engine::model_safety::StopAssessment;
use pump_quant_app::engine::Engine;
use pump_quant_app::safety_off::SafetyLoad;
use pump_quant_inference::InferenceClient;

/// Per-request transport bound. Strictly above the engine's decision deadline
/// (`CHAMPION_MAX_DECISION_AGE_MS`, 3 s) so the engine's own deadline abandons a slow ask first and the
/// worker thread is still freed by a bounded socket timeout — no ask can wait forever.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(8);

/// Where the daemon publishes the CURRENT shutdown request (session + request id + exposure digest).
/// The recipient reads it; the daemon rewrites it whenever the exposure changes.
pub const HANDOFF_REQUEST_FILE: &str = "data/PROTECTIVE_HANDOFF_REQUEST.json";
/// Where an identified recipient writes its acceptance. A file that exists BEFORE the request is
/// published cannot match it (it cannot contain the request id), so a stale file authorizes nothing.
pub const PROTECTIVE_HANDOFF_ACK_FILE: &str = "data/PROTECTIVE_HANDOFF_ACK.json";

/// Where the durable SAFETY_OFF latch lives by default.
pub const DEFAULT_SAFETY_FILE: &str = "data/model_safety_off.json";

/// Result of arming.
#[derive(Debug)]
pub struct Armed {
    /// What the durable file said at load.
    pub load: SafetyLoad,
    /// Whether the engine started blocked (restored latch or unreadable file).
    pub blocked_at_start: bool,
    /// The safety file in use.
    pub safety_path: PathBuf,
}

/// Arm the paper model lane against an OpenAI-compatible llama-server endpoint with the production
/// [`InferenceClient`], and adopt the durable latch. A blocked latch stays blocked: a restart never
/// re-arms.
pub fn arm_paper_model(engine: &mut Engine, endpoint: &str, safety_path: &Path) -> Armed {
    let client = InferenceClient::new(endpoint, CLIENT_TIMEOUT);
    engine.enable_paper_model(client);
    let load = engine.model_safety_attach(safety_path);
    Armed {
        load,
        blocked_at_start: engine.model_safety_blocked(),
        safety_path: safety_path.to_path_buf(),
    }
}

/// Why an acknowledgement did not authorize termination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckRejection {
    /// No acknowledgement file.
    Absent,
    /// Present but unreadable or not the expected JSON.
    Unreadable,
    /// Written for another daemon session.
    SessionMismatch,
    /// Written for another (older/newer) shutdown request.
    RequestMismatch,
    /// Bound to a different position/pending-order snapshot than the one outstanding now.
    ExposureMismatch,
    /// No named recipient, or the recipient did not state it accepted protective responsibility.
    NoAcceptedRecipient,
}

/// What the daemon does with a stop request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopGate {
    /// Nothing held, nothing outstanding: safe to stop.
    CompleteFlat,
    /// Exposure remains and a VALID acknowledgement binds an identified recipient to this session,
    /// request and exposure snapshot: stop.
    CompleteHandedOff {
        /// The recipient that accepted protective responsibility.
        recipient: String,
    },
    /// Exposure remains and no valid acknowledgement exists: DO NOT stop. Remain blocked, alert, report
    /// incomplete shutdown. No liquidation is attempted - that policy is not agreed.
    Incomplete {
        /// What is outstanding.
        assessment: StopAssessment,
        /// Why no acknowledgement authorized termination.
        rejection: AckRejection,
        /// The request id the recipient must echo.
        request_id: String,
        /// The exposure digest the recipient must echo.
        exposure_digest: String,
    },
}

/// A per-process session id: unguessable-enough and unique per daemon start (pid + start nanos). A file
/// from an earlier run, or one written before the session started, cannot contain it.
#[must_use]
pub fn new_session_id() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("pq-{}-{n:x}", std::process::id())
}

/// Validate an acknowledgement file against the CURRENT session, request and exposure digest.
///
/// # Errors
/// [`AckRejection`] - the first reason it cannot authorize termination. Fail-closed on everything.
pub fn validate_ack(
    ack_file: &Path,
    session: &str,
    request_id: &str,
    exposure_digest: &str,
) -> Result<String, AckRejection> {
    let raw = match std::fs::read_to_string(ack_file) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(AckRejection::Absent),
        Err(_) => return Err(AckRejection::Unreadable),
    };
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|_| AckRejection::Unreadable)?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_str());
    if get("session_id") != Some(session) {
        return Err(AckRejection::SessionMismatch);
    }
    if get("request_id") != Some(request_id) {
        return Err(AckRejection::RequestMismatch);
    }
    if get("exposure_digest") != Some(exposure_digest) {
        return Err(AckRejection::ExposureMismatch);
    }
    let recipient = get("recipient").map(str::trim).unwrap_or("");
    let accepted = v
        .get("accepted_protective_responsibility")
        .and_then(|x| x.as_bool())
        == Some(true);
    if recipient.is_empty() || !accepted {
        return Err(AckRejection::NoAcceptedRecipient);
    }
    Ok(recipient.to_string())
}

/// Stop-request bookkeeping for one daemon session.
#[derive(Debug)]
pub struct StopSession {
    /// This daemon session's id.
    pub session_id: String,
    request_seq: u64,
    last_digest: String,
    request_id: String,
}

impl StopSession {
    /// A fresh session.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session_id: new_session_id(),
            request_seq: 0,
            last_digest: String::new(),
            request_id: String::new(),
        }
    }
}

impl Default for StopSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle a stop request for an armed engine: block entries, invalidate queued risk-increasing intents,
/// persist held/pending state, publish the request (session id, request id, exposure digest, snapshot) for a
/// recipient, then decide. The request id changes whenever the exposure changes, so an acknowledgement for an
/// earlier snapshot can never authorize a later one. The engine keeps protecting positions either way.
pub fn handle_stop_request(
    engine: &mut Engine,
    st: &mut StopSession,
    request_file: &Path,
    ack_file: &Path,
) -> StopGate {
    let report = engine.model_controlled_shutdown();
    let assessment = engine.model_stop_assessment();
    if assessment.is_flat_and_reconciled() && report.persisted {
        return StopGate::CompleteFlat;
    }
    let (digest, snapshot) = engine.model_exposure_digest();
    if digest != st.last_digest || st.request_id.is_empty() {
        #[allow(clippy::arithmetic_side_effects)]
        // LINT-ALLOW(hot_arith,hot_cast): u64 request seq counter
        {
            st.request_seq += 1;
        }
        st.request_id = format!("{}-req{}", st.session_id, st.request_seq);
        st.last_digest = digest.clone();
        let doc = serde_json::json!({
            "session_id": st.session_id,
            "request_id": st.request_id,
            "exposure_digest": digest,
            "exposure_snapshot": snapshot,
            "assessment": {
                "held": assessment.held,
                "pending_orders": assessment.pending_orders,
                "uncertain_orders": assessment.uncertain_orders,
            },
            "ack_file": ack_file.display().to_string(),
            "instructions": "To accept protective responsibility write ack_file as JSON with session_id, request_id, exposure_digest copied from this file, recipient (your identity) and accepted_protective_responsibility=true.",
        });
        // Best effort: if the request cannot be published nothing can be acknowledged -> stays incomplete.
        let _ = std::fs::write(request_file, doc.to_string());
    }
    if !report.persisted {
        // Cannot record the exposure durably: never claim completion on an unrecorded exposure.
        return StopGate::Incomplete {
            assessment,
            rejection: AckRejection::Unreadable,
            request_id: st.request_id.clone(),
            exposure_digest: st.last_digest.clone(),
        };
    }
    match validate_ack(ack_file, &st.session_id, &st.request_id, &st.last_digest) {
        Ok(recipient) => StopGate::CompleteHandedOff { recipient },
        Err(rejection) => StopGate::Incomplete {
            assessment,
            rejection,
            request_id: st.request_id.clone(),
            exposure_digest: st.last_digest.clone(),
        },
    }
}

/// One line per held position describing whether management data is actually fresh, plus a loud degraded
/// summary. Pure formatting over the engine's measured status so the daemon and tests print the same thing.
#[must_use]
pub fn held_data_report(engine: &Engine) -> (String, bool) {
    let hex = |m: &[u8; 32]| {
        m.iter()
            .take(4)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let status = engine.model_held_data_status();
    let mut lines = Vec::new();
    let mut degraded = false;
    for s in &status {
        let age = |v: Option<i64>| v.map_or("none".to_string(), |a| format!("{a}ms"));
        match &s.management_ready {
            Ok(())
                if pump_quant_app::engine::model_manage::amm_protection_gap(
                    s,
                    engine.model_clock_ms_now(),
                )
                .is_some() =>
            {
                degraded = true;
                lines.push(format!(
                    "held {} venue=amm DEGRADED: {} (last verified mark age={}); price-based protection (hard stop / rug precursor) is NOT observing this position",
                    hex(&s.mint),
                    pump_quant_app::engine::model_manage::amm_protection_gap(s, engine.model_clock_ms_now()).unwrap_or_default(),
                    s.protect_mark_ms.map_or("none".to_string(), |m| format!("{}ms", engine.model_clock_ms_now().saturating_sub(m)))
                ));
            }
            Ok(()) => lines.push(format!(
                "held {} venue={} reserve_age={} print_age={} READY",
                hex(&s.mint),
                if s.amm { "amm" } else { "curve" },
                age(s.reserve_age_ms),
                age(s.last_print_age_ms)
            )),
            Err(why) => {
                degraded = true;
                lines.push(format!(
                    "held {} venue={} reserve_age={} print_age={} DEGRADED: management data unavailable ({why}); \
                     protection that remains = print-driven rug-precursor/hard-stop ONLY while prints arrive",
                    hex(&s.mint),
                    if s.amm { "amm" } else { "curve" },
                    age(s.reserve_age_ms),
                    age(s.last_print_age_ms)
                ));
            }
        }
    }
    (lines.join("\n"), degraded)
}

/// Free bytes on the filesystem holding `path` (None when it cannot be measured).
#[must_use]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a properly sized, zeroed statvfs.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut s) };
    if rc != 0 {
        return None;
    }
    // `u64::from` is a no-op on 64-bit targets (where libc statvfs fields are u64)
    // but is required on 32-bit targets where they are u32; keep it for portability.
    #[allow(clippy::useless_conversion)]
    Some(u64::from(s.f_bavail).saturating_mul(u64::from(s.f_frsize)))
}

/// What the headroom check concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Headroom {
    /// Enough free space for durable writes.
    Ok { free: u64 },
    /// Below the floor: durable state/journals are at risk. The daemon must say so loudly and treat any
    /// failed persist as fail-closed (blocked), not keep trading blind.
    Low { free: u64, floor: u64 },
    /// Could not be measured: reported, never assumed fine.
    Unknown,
}

/// Minimum free bytes the durable-state directory must keep. Generous relative to the files at stake (the
/// safety latch is KBs, journals grow) so the alert fires well before a write actually fails.
pub const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Check headroom on the filesystem that holds the durable safety file.
#[must_use]
pub fn check_headroom(path: &Path, floor: u64) -> Headroom {
    match free_bytes(path) {
        None => Headroom::Unknown,
        Some(free) if free < floor => Headroom::Low { free, floor },
        Some(free) => Headroom::Ok { free },
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Stop trigger -> action table: daemon-side measurement and actions (`pump_quant_app::stop_policy`).
// ---------------------------------------------------------------------------------------------------------------

/// Startup refusal: a live signing/submission capability is present. The paper model lane refuses to start.
///
/// # Errors
/// The capability found, by name (see `stop_policy::require_paper_only`).
pub fn startup_paper_check(
    engine: &Engine,
    live_flag: bool,
    keypair_loaded: bool,
) -> Result<(), &'static str> {
    pump_quant_app::stop_policy::require_paper_only(pump_quant_app::stop_policy::StartCapability {
        live_flag,
        keypair_loaded,
        submission_sink_installed: engine.outbound_sink_installed(),
        paper_mode: engine.mode() == pump_quant_app::engine::RunMode::Paper,
    })
}

/// Host RAM headroom from `/proc/meminfo` against the operator's 12% floor. `None` = unmeasurable.
#[must_use]
pub fn ram_headroom_ok() -> Option<bool> {
    let t = std::fs::read_to_string("/proc/meminfo").ok()?;
    let (total, avail) = pump_quant_app::stop_policy::parse_meminfo(&t);
    pump_quant_app::stop_policy::ram_ok(total, avail, pump_quant_app::stop_policy::RAM_FLOOR_BPS)
}

/// One cgroup v2 level of this process's ancestor chain, as read. `None` in `max`/`high` = unreadable or
/// unparseable; `Some(None)` = the literal "max" (no limit at this level); `Some(Some(n))` = a byte limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgLevel {
    /// The level's path relative to the cgroup root (e.g. "/system.slice/hermes-gateway.service").
    pub level: String,
    /// `memory.max`.
    pub max: Option<Option<u64>>,
    /// `memory.high`.
    pub high: Option<Option<u64>>,
    /// `memory.current` (`None` = unreadable).
    pub current: Option<u64>,
}

/// The cgroup term's state. Three DISTINCT states; "no cgroup cap" does NOT mean unlimited physical memory
/// (the host term still applies), and unmeasurable is UNKNOWN, never PASS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemState {
    /// Every level of the ancestor chain reads "max" for both memory.max and memory.high.
    NoCgroupCap,
    /// The tightest level: its ceiling (lower of memory.max / memory.high), its room (ceiling - memory.current)
    /// and its path.
    CgroupCap { cap: u64, room: u64, level: String },
    /// A file needed for the verdict could not be read or parsed (what, by name).
    Unmeasurable(String),
}

/// THE memory rule, shared by the startup budget and the per-tick RAM stop input:
/// `effective = min(host MemAvailable - floor_bps of MemTotal, min over the cgroup chain of (limit - current))`,
/// compared against `floor_bytes`. The host floor is subtracted ONCE, on the host term only.
/// The ~219 GB measured on this host is SHARED headroom (inference and other processes consume it), so callers
/// re-evaluate this on every check; it is never cached as a reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemVerdict {
    /// Cgroup term state.
    pub state: MemState,
    /// The host floor, bytes (`floor_bps` of MemTotal). `None` = meminfo unreadable.
    pub host_floor: Option<u64>,
    /// Host term: MemAvailable - host_floor (saturating). `None` = meminfo unreadable.
    pub host_term: Option<u64>,
    /// min(host_term, cgroup room). `None` = UNKNOWN.
    pub effective: Option<u64>,
    /// `effective >= floor_bytes`; `None` = UNKNOWN (restricts new exposure, never PASS).
    pub ok: Option<bool>,
}

/// Pure memory rule (see [`MemVerdict`]). `levels` is the WHOLE ancestor chain (any order); every level counts.
#[must_use]
pub fn mem_rule(
    mem_total_kb: Option<u64>,
    mem_available_kb: Option<u64>,
    levels: &[CgLevel],
    floor_bps: u64,
    floor_bytes: u64,
) -> MemVerdict {
    let host_floor = mem_total_kb.map(|t| {
        u64::try_from(u128::from(t.saturating_mul(1024)) * u128::from(floor_bps) / 10_000)
            .unwrap_or(u64::MAX)
    });
    let host_term = match (mem_available_kb, host_floor) {
        (Some(a), Some(f)) => Some(a.saturating_mul(1024).saturating_sub(f)),
        _ => None,
    };
    let mut state = MemState::NoCgroupCap;
    for l in levels {
        let (Some(max), Some(high)) = (l.max, l.high) else {
            state =
                MemState::Unmeasurable(format!("{}: memory.max/memory.high unreadable", l.level));
            break;
        };
        let Some(cap) = pump_quant_app::stop_policy::cgroup_mem_ceiling(max, high) else {
            continue; // "max" at this level: no cap here; ancestors still count
        };
        let Some(cur) = l.current else {
            state = MemState::Unmeasurable(format!("{}: memory.current unreadable", l.level));
            break;
        };
        let room = cap.saturating_sub(cur);
        let tighter = match &state {
            MemState::CgroupCap { room: r, .. } => room < *r,
            _ => true,
        };
        if tighter {
            state = MemState::CgroupCap {
                cap,
                room,
                level: l.level.clone(),
            };
        }
    }
    let effective = match (&state, host_term) {
        (MemState::Unmeasurable(_), _) | (_, None) => None,
        (MemState::NoCgroupCap, Some(h)) => Some(h),
        // The floor is NOT subtracted again here: the cgroup term is raw room.
        (MemState::CgroupCap { room, .. }, Some(h)) => Some(h.min(*room)),
    };
    MemVerdict {
        state,
        host_floor,
        host_term,
        effective,
        ok: pump_quant_app::stop_policy::bytes_ok(effective, floor_bytes),
    }
}

/// Read the WHOLE cgroup v2 ancestor chain of `rel` (the `0::` path of `/proc/self/cgroup`) under `root`
/// (normally `/sys/fs/cgroup`), leaf first, up to but excluding the root itself (the root carries no limits).
#[must_use]
pub fn read_cgroup_chain(root: &Path, rel: &str) -> Vec<CgLevel> {
    let lim = |d: &Path, f: &str| {
        std::fs::read_to_string(d.join(f))
            .ok()
            .and_then(|s| pump_quant_app::stop_policy::parse_cgroup_limit(&s).ok())
    };
    let mut out = Vec::new();
    let mut rel_p = PathBuf::from(rel.trim());
    loop {
        let r = rel_p.to_string_lossy().trim_start_matches('/').to_string();
        if r.is_empty() {
            break;
        }
        let d = root.join(&r);
        out.push(CgLevel {
            level: format!("/{r}"),
            max: lim(&d, "memory.max"),
            high: lim(&d, "memory.high"),
            current: std::fs::read_to_string(d.join("memory.current"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok()),
        });
        if !rel_p.pop() {
            break;
        }
    }
    out
}

/// HARNESS-ONLY simulated pressure (offline replay harness; `PQ_OFFLINE_PAPER_REPLAY` set, never live). The file
/// [`SIMULATED_PRESSURE_FILE`] names the resource(s) to report as short: `ram`, `disk`, `disk_unknown`. Every use
/// is logged as SIMULATED_PRESSURE and kept apart from the measured readings.
pub const SIMULATED_PRESSURE_FILE: &str = "data/HARNESS_SIMULATED_PRESSURE";

/// Which simulated pressures are on (`harness` false -> none, whatever the file says).
#[must_use]
pub fn simulated_pressure(harness: bool) -> (bool, bool, bool) {
    if !harness {
        return (false, false, false);
    }
    let t = std::fs::read_to_string(SIMULATED_PRESSURE_FILE).unwrap_or_default();
    let has = |k: &str| t.split_whitespace().any(|w| w == k);
    (has("ram"), has("disk"), has("disk_unknown"))
}

/// The shared memory rule over this process's LIVE readings (`/proc/meminfo`, `/proc/self/cgroup`, the whole
/// `/sys/fs/cgroup` ancestor chain). Called at startup AND on every runtime check.
#[must_use]
pub fn mem_verdict_now() -> MemVerdict {
    let (total_kb, avail_kb) = std::fs::read_to_string("/proc/meminfo")
        .map(|t| pump_quant_app::stop_policy::parse_meminfo(&t))
        .unwrap_or((None, None));
    let rel = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|cg| {
            cg.lines()
                .find_map(|l| l.strip_prefix("0::"))
                .map(str::to_string)
        });
    let levels = match rel {
        Some(r) => read_cgroup_chain(Path::new("/sys/fs/cgroup"), &r),
        None => vec![CgLevel {
            level: "/proc/self/cgroup".into(),
            max: None,
            high: None,
            current: None,
        }],
    };
    mem_rule(
        total_kb,
        avail_kb,
        &levels,
        pump_quant_app::stop_policy::RAM_FLOOR_BPS,
        pump_quant_app::stop_policy::RAM_FLOOR_BYTES,
    )
}

/// Effective memory available to this process under the shared rule (`None` = UNKNOWN).
#[must_use]
pub fn mem_available_for_self() -> Option<u64> {
    mem_verdict_now().effective
}

/// RAM stop input: the shared rule's verdict against [`pump_quant_app::stop_policy::RAM_FLOOR_BYTES`].
#[must_use]
pub fn ram_bytes_ok() -> Option<bool> {
    mem_verdict_now().ok
}

/// Human label for the three memory states (startup log, report).
#[must_use]
pub fn mem_state_label(s: &MemState) -> String {
    match s {
        MemState::NoCgroupCap => {
            "no cgroup cap (every ancestor level 'max'; host term still applies)".to_string()
        }
        MemState::CgroupCap { cap, room, level } => {
            format!("cgroup cap {cap} at level {level} (room {room})")
        }
        MemState::Unmeasurable(w) => format!("unmeasurable ({w}) -> UNKNOWN, never PASS"),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 6 h paper-run resource budget from REAL-TIME rates and the process's ACTUAL limits
// (basis: /training/mh_build/proc/RESOURCE_BUDGET.md section 6).
// ---------------------------------------------------------------------------------------------------------------

/// Event-stream growth, bytes per REAL-TIME second. Keyed on the capture `recv_unix_ms`, never on accelerated replay
/// wall time. A range, derived in RESOURCE_BUDGET.md section 6:
/// * `lo`: s2 mean 755.7 tx/s x 596.5 B/tx (s1's measured stream bytes per captured tx).
/// * `mid`: s1 measured mean, 405,446,105 B over 674 capture-s.
/// * `hi`: s1 busiest measured 1-min stream window, 41,115,483 B/min.
/// * `stress`: s1 busiest 1-min tx window (75,997 tx) x the cont_wire05/bM3a mix (1.7405 ev/tx x 436.6 B/ev). A
///   heavier-schema tape at the peak rate. The start check uses this bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRate {
    /// Quiet segment mean.
    pub lo_bps: u64,
    /// Measured mean.
    pub mid_bps: u64,
    /// Measured peak 1-min window.
    pub hi_bps: u64,
    /// Cross-tape peak (upper bound).
    pub stress_bps: u64,
}

/// The measured real-time event-stream rate range (bytes/s).
pub const STREAM_RATE: StreamRate = StreamRate {
    lo_bps: 450_750,
    mid_bps: 601_552,
    hi_bps: 685_258,
    stress_bps: 962_505,
};

/// Bytes the run writes besides the event stream, upper bound for 6.5 h: flow.ckpt x2 (atomic replace) with x10
/// growth margin (0.34 GB), held journal (~0.04 GB at the limP1 rate of 125 kB / 150 s), err.log + barrier/model
/// logs (~0.03 GB). Rounded up to 0.5 GB.
pub const NON_STREAM_BYTES_6H: u64 = 500_000_000;

/// The run's disk need `(lo, mid, hi, stress)` in bytes for `run_s` real-time seconds (6 h + 30 min drain = 23_400).
#[must_use]
pub fn run_disk_need(rate: StreamRate, run_s: u64) -> (u64, u64, u64, u64) {
    let f = |bps: u64| {
        bps.saturating_mul(run_s)
            .saturating_add(NON_STREAM_BYTES_6H)
    };
    (
        f(rate.lo_bps),
        f(rate.mid_bps),
        f(rate.hi_bps),
        f(rate.stress_bps),
    )
}

/// Open files the daemon needs: measured 8 fds in steady state (limP1), plus a 64-fd margin for transient
/// sockets (RPC, model HTTP workers, reconnects) and atomic-replace temp files.
pub const FD_NEED: u64 = 8 + 64;

/// The run's budget against the actual limits read at start. Every `None` input is UNMEASURED and makes the
/// corresponding verdict `None` (never assumed fine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBudget {
    /// Least free bytes over the written filesystems.
    pub disk_free: Option<u64>,
    /// `(lo, mid, hi, stress)` disk need, bytes.
    pub disk_need: (u64, u64, u64, u64),
    /// Free at start >= STRESS need + the soft floor (even the upper bound ends above the soft floor).
    pub disk_ok: Option<bool>,
    /// Soft RLIMIT_NOFILE.
    pub nofile_soft: Option<u64>,
    /// `nofile_soft >= FD_NEED`.
    pub nofile_ok: Option<bool>,
    /// The shared memory rule's verdict ([`mem_rule`]): state, host floor, effective available.
    pub mem: MemVerdict,
    /// `mem.effective >= RAM_FLOOR_BYTES` (12 GiB = 2x the projected 6.5 h daemon VmHWM of 5.95 GB).
    pub mem_ok: Option<bool>,
}

/// Pure budget verdict (tested); [`run_budget_now`] feeds it the live readings.
#[must_use]
pub fn run_budget(
    disk_free: Option<u64>,
    nofile_soft: Option<u64>,
    mem: MemVerdict,
    run_s: u64,
) -> RunBudget {
    let need = run_disk_need(STREAM_RATE, run_s);
    RunBudget {
        disk_free,
        disk_need: need,
        disk_ok: disk_free.map(|f| {
            f >= need
                .3
                .saturating_add(pump_quant_app::stop_policy::DISK_SOFT_FLOOR_BYTES)
        }),
        nofile_soft,
        nofile_ok: nofile_soft.map(|n| n >= FD_NEED),
        mem_ok: mem.ok,
        mem,
    }
}

/// Soft `RLIMIT_NOFILE` of this process from `/proc/self/limits` (`None` = unreadable; "unlimited" = u64::MAX).
#[must_use]
pub fn nofile_soft_limit() -> Option<u64> {
    let t = std::fs::read_to_string("/proc/self/limits").ok()?;
    parse_nofile_soft(&t)
}

/// Parse the soft "Max open files" value from `/proc/<pid>/limits` text.
#[must_use]
pub fn parse_nofile_soft(limits: &str) -> Option<u64> {
    let l = limits.lines().find(|l| l.starts_with("Max open files"))?;
    let v = l
        .trim_start_matches("Max open files")
        .split_whitespace()
        .next()?;
    if v == "unlimited" {
        Some(u64::MAX)
    } else {
        v.parse().ok()
    }
}

/// The run budget against this process's live limits, over the paths it writes (least free decides).
#[must_use]
pub fn run_budget_now(write_paths: &[&Path], run_s: u64) -> RunBudget {
    let mut disk: Option<u64> = None;
    let mut unmeasured = false;
    for p in write_paths {
        match free_bytes(p) {
            Some(f) => disk = Some(disk.map_or(f, |m| m.min(f))),
            None => unmeasured = true,
        }
    }
    run_budget(
        if unmeasured { None } else { disk },
        nofile_soft_limit(),
        mem_verdict_now(),
        run_s,
    )
}

/// Disk stop inputs `(soft_ok, hard_ok)` over EVERY durable-output path (event stream, journals, checkpoints): the
/// least free filesystem decides. Any unmeasurable path -> `(None, None)` (soft restricts; hard does not latch).
#[must_use]
pub fn disk_floors_ok(paths: &[&Path], soft: u64, hard: u64) -> (Option<bool>, Option<bool>) {
    let mut min_free: Option<u64> = None;
    for p in paths {
        match free_bytes(p) {
            None => return (None, None),
            Some(f) => min_free = Some(min_free.map_or(f, |m| m.min(f))),
        }
    }
    match min_free {
        None => (None, None),
        Some(f) => (Some(f >= soft), Some(f >= hard)),
    }
}

/// Disk headroom as a stop-table input: `Some(true)` only when measured at or above the floor.
#[must_use]
pub fn disk_headroom_ok(path: &Path, floor: u64) -> Option<bool> {
    match check_headroom(path, floor) {
        Headroom::Ok { .. } => Some(true),
        Headroom::Low { .. } => Some(false),
        Headroom::Unknown => None,
    }
}

/// Run deadline and drain bound. The default is the operator's 6 h + 30 min drain; an override
/// (`PQ_RUN_DEADLINE_MS` / `PQ_DRAIN_BOUND_MS`) may only SHORTEN them (a longer run is not this switch's call).
#[must_use]
pub fn run_deadline_from(deadline: Option<&str>, drain: Option<&str>) -> (i64, i64) {
    use pump_quant_app::stop_policy::{DRAIN_BOUND_MS, RUN_DEADLINE_MS};
    let pick = |v: Option<&str>, dflt: i64| {
        v.and_then(|x| x.trim().parse::<i64>().ok())
            .filter(|x| *x > 0)
            .map_or(dflt, |x| x.min(dflt))
    };
    (pick(deadline, RUN_DEADLINE_MS), pick(drain, DRAIN_BOUND_MS))
}

/// The drain bound passed: hand off through the EXISTING stop protocol by raising the stop sentinel. The stop
/// gate then completes only when flat+reconciled or after a session/request/exposure-bound acknowledgement;
/// otherwise the daemon stays up, blocked, protecting. Nothing is force-closed. Returns whether it was raised now.
pub fn request_deadline_handoff(stop_file: &Path) -> bool {
    if stop_file.exists() {
        return false;
    }
    if let Some(d) = stop_file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(stop_file, "run_deadline_drain_elapsed\n").is_ok()
}

/// RPC/data budget for launch bootstrap (the only paper-mode RPC consumer), in history PAGES per hour, with a
/// reservation for held-position support. Basis (measured, docs/HELIUS_BUDGET_2026-07-29.md): plan ceiling
/// 200 req/s, measured standing load 0.133 req/s. Capacity 3_600 pages/h (1 page/s average = 0.5% of the
/// ceiling); each held position reserves 120 pages/h (6 full 20-page walks) that discovery may not spend.
#[must_use]
pub fn bootstrap_budget() -> pump_quant_app::stop_policy::WindowBudget {
    pump_quant_app::stop_policy::WindowBudget::new(
        pump_quant_app::stop_policy::HeldReserve {
            capacity: 3_600,
            per_held: 120,
        },
        3_600_000,
    )
}

/// Default held-state ledger path.
pub const DEFAULT_HELD_FILE: &str = "data/model_held_state.json";

/// What startup restore decided.
#[derive(Debug)]
pub enum StartupRestore {
    /// No ledger: clean start.
    Clean,
    /// Positions/orders rebuilt (pending orders are UNCERTAIN).
    Restored(pump_quant_app::held_state::RestoreReport),
    /// The ledger exists but cannot be applied. The daemon MUST NOT start trading: starting would orphan
    /// real exposure. The file is untouched; an operator reconciles it.
    Refused(String),
}

/// Attach the held-state ledger and restore it into a FRESH engine (before the first tick).
pub fn restore_held_state(engine: &mut Engine, path: &Path) -> StartupRestore {
    engine.model_held_attach(path);
    match engine.model_held_restore() {
        Ok(None) => StartupRestore::Clean,
        Ok(Some(r)) => StartupRestore::Restored(r),
        Err(e) => StartupRestore::Refused(format!("{e:?}")),
    }
}

/// Default location of the durable missing-history ledger.
pub const DEFAULT_MISSING_HISTORY_FILE: &str = "data/model_missing_history.json";

/// What startup did with the missing-history ledger.
#[derive(Debug, PartialEq, Eq)]
pub enum MissingHistoryStartup {
    /// No ledger and no restored exposure: a genuinely clean first start.
    Clean,
    /// Trusted records restored (count). Their gaps keep refusing.
    Restored(usize),
    /// The ledger is unreadable/incompatible, OR it is ABSENT while held exposure was restored
    /// (a deleted record must not read as "no gap"). Every prompt refuses
    /// `join_history_continuity_unknown`; the evidence on disk is left untouched.
    ContinuityUnknown(String),
}

/// Attach + restore the missing-history ledger BEFORE the first tick (call after
/// [`restore_held_state`] so `held_restored` is known).
pub fn attach_missing_history(
    engine: &mut Engine,
    path: &Path,
    held_restored: bool,
) -> MissingHistoryStartup {
    use pump_quant_app::missing_history_store::StoreLoad;
    match engine.model_missing_attach(path) {
        StoreLoad::NeverWritten if held_restored => {
            let _ = engine.model_restore_missing_history(&[], false);
            MissingHistoryStartup::ContinuityUnknown("absent_with_restored_exposure".into())
        }
        StoreLoad::NeverWritten => {
            // Clean start: make "trusted, no unresolved gaps" durable so a later restart with exposure is not
            // forced to read an absent file as unknown.
            engine.model_missing_publish_baseline();
            MissingHistoryStartup::Clean
        }
        StoreLoad::Records(r) => MissingHistoryStartup::Restored(r.len()),
        StoreLoad::Untrusted(why) => MissingHistoryStartup::ContinuityUnknown(why.to_string()),
    }
}

/// Mints whose reserve/print feeds the daemon must (re)subscribe after a restore, independently of
/// new-opportunity discovery.
#[must_use]
pub fn mints_needing_feeds(engine: &Engine) -> Vec<[u8; 32]> {
    engine.model_held_mints()
}

/// Edge-triggered callout for held positions whose management data is stale or missing.
///
/// Emits: ONSET (first time a position degrades, naming the component), REMINDER (every `remind_ms` while
/// it stays degraded, with how long), RECOVERED (when management is ready again, with the outage length),
/// and UNPROTECTED (degraded AND prints silent: neither management nor the print-driven rug precursor /
/// hard stop can act). It never loosens the 60 s bound and never trades; it only tells the operator.
#[derive(Debug, Default)]
pub struct StaleCallout {
    state: std::collections::BTreeMap<[u8; 32], (i64, i64, bool)>, // (since_ms, last_alert_ms, unprotected_said)
    /// Held AMM positions whose price-based protection has no valid mark: (since_ms, last_alert_ms).
    protect: std::collections::BTreeMap<[u8; 32], (i64, i64)>,
    /// Held positions with no executable protection because every token is reserved by an unresolved sell.
    reserved: std::collections::BTreeMap<[u8; 32], (i64, i64)>,
}

/// One line to print, with whether it is an alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalloutLine {
    /// Text.
    pub text: String,
    /// ALERT-level (operator action may be needed).
    pub alert: bool,
}

/// Print silence beyond which the print-driven safeguards are treated as unable to act: the existing
/// pricing bound, not a new threshold.
const PRINT_SILENT_MS: i64 = pump_quant_app::curve_annotation::PRICING_BUDGET_MS;

impl StaleCallout {
    /// Evaluate the engine's measured status at wire clock `now_ms`.
    pub fn evaluate(&mut self, engine: &Engine, now_ms: i64, remind_ms: i64) -> Vec<CalloutLine> {
        let hex = |m: &[u8; 32]| {
            m.iter()
                .take(4)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let mut out = Vec::new();
        let status = engine.model_held_data_status();
        let live: std::collections::BTreeSet<[u8; 32]> = status.iter().map(|s| s.mint).collect();
        for s in &status {
            let silent = s.last_print_age_ms.is_none_or(|a| a > PRINT_SILENT_MS);
            match &s.management_ready {
                Err(why) => {
                    let e = self
                        .state
                        .entry(s.mint)
                        .or_insert((now_ms, i64::MIN / 2, false));
                    let first = e.1 == i64::MIN / 2;
                    // `now_ms` and `e.1`/`e.0` are readings of the same i64 millisecond wire
                    // clock; the i64::MIN/2 sentinel stored at insert is never subtracted
                    // (guarded by `first`). `saturating_sub` keeps the age type-safe at the
                    // i64 extremes: identical to `-` for any same-clock pair a live process
                    // produces, and it errs toward "very stale" (fail-safe for a degradation
                    // callout) otherwise. This is a practical clock-domain bound, not a type
                    // proof that the raw subtraction fits.
                    if first || now_ms.saturating_sub(e.1) >= remind_ms {
                        e.1 = now_ms;
                        out.push(CalloutLine {
                            text: format!(
                                "{} held {} venue={} MANAGEMENT UNAVAILABLE: {why} (reserve_age={} print_age={}) degraded_for={}s",
                                if first { "ONSET" } else { "REMINDER" },
                                hex(&s.mint),
                                if s.amm { "amm" } else { "curve" },
                                s.reserve_age_ms.map_or("none".into(), |a| format!("{a}ms")),
                                s.last_print_age_ms.map_or("none".into(), |a| format!("{a}ms")),
                                now_ms.saturating_sub(e.0).max(0) / 1000
                            ),
                            alert: true,
                        });
                    }
                    if silent && !e.2 {
                        e.2 = true;
                        out.push(CalloutLine {
                            text: format!(
                                "UNPROTECTED held {}: management data unavailable AND no print within {}ms - the rug precursor / hard stop \
                                 are print-driven and cannot act until prints resume; the connection being up does not change this",
                                hex(&s.mint),
                                PRINT_SILENT_MS
                            ),
                            alert: true,
                        });
                    } else if !silent {
                        e.2 = false;
                    }
                }
                Ok(()) => {
                    if let Some((since, _, _)) = self.state.remove(&s.mint) {
                        // `since` is the same-clock reading stored at ONSET; see the note above.
                        out.push(CalloutLine {
                            text: format!(
                                "RECOVERED held {}: management data fresh again after {}s degraded",
                                hex(&s.mint),
                                now_ms.saturating_sub(since).max(0) / 1000
                            ),
                            alert: false,
                        });
                    }
                }
            }
        }
        // PROTECTION GAP (held AMM): an independent state from management readiness. The age is the last VERIFIED
        // mark; a hint, a rejected swap or an advancing clock never refreshes it, and it fires with no further swap.
        for s in &status {
            match pump_quant_app::engine::model_manage::amm_protection_gap(s, now_ms) {
                Some(why) => {
                    let e = self.protect.entry(s.mint).or_insert((now_ms, i64::MIN / 2));
                    let first = e.1 == i64::MIN / 2;
                    if first || now_ms.saturating_sub(e.1) >= remind_ms {
                        e.1 = now_ms;
                        out.push(CalloutLine {
                            text: format!(
                                "{} held {} venue=amm UNPROTECTED (price-based): {why} (last_verified_mark_age={}) degraded_for={}s - hard stop / rug precursor are not observing this position; no liquidation rule is applied",
                                if first { "ONSET" } else { "REMINDER" },
                                hex(&s.mint),
                                s.protect_mark_ms.map_or("none".into(), |m| format!("{}ms", now_ms.saturating_sub(m))),
                                now_ms.saturating_sub(e.0).max(0) / 1000
                            ),
                            alert: true,
                        });
                    }
                }
                None => {
                    if let Some((since, _)) = self.protect.remove(&s.mint) {
                        out.push(CalloutLine {
                            text: format!(
                                "RECOVERED held {}: a verified protection mark arrived after {}s without one",
                                hex(&s.mint),
                                now_ms.saturating_sub(since).max(0) / 1000
                            ),
                            alert: false,
                        });
                    }
                }
            }
        }
        // RESERVATION GAP: an unresolved sell holds every remaining token, so a protective trigger cannot sell
        // anything (it defers). The position stays monitored and every trigger stays armed; this only tells the operator.
        for s in &status {
            match pump_quant_app::engine::model_manage::sell_reservation_gap(s) {
                Some(why) => {
                    let e = self
                        .reserved
                        .entry(s.mint)
                        .or_insert((now_ms, i64::MIN / 2));
                    let first = e.1 == i64::MIN / 2;
                    if first || now_ms.saturating_sub(e.1) >= remind_ms {
                        e.1 = now_ms;
                        out.push(CalloutLine {
                            text: format!(
                                "{} held {} PROTECTION DEFERRED (sell unresolved): {why}; protective_deferrals={} degraded_for={}s - monitoring and triggers stay armed, nothing is sold or resubmitted until the sell is reconciled",
                                if first { "ONSET" } else { "REMINDER" },
                                hex(&s.mint),
                                s.protect_deferred,
                                now_ms.saturating_sub(e.0).max(0) / 1000
                            ),
                            alert: true,
                        });
                    }
                }
                None => {
                    if let Some((since, _)) = self.reserved.remove(&s.mint) {
                        out.push(CalloutLine {
                            text: format!(
                                "RECOVERED held {}: the unresolved sell was reconciled after {}s; protection is evaluated against free inventory again",
                                hex(&s.mint),
                                now_ms.saturating_sub(since).max(0) / 1000
                            ),
                            alert: false,
                        });
                    }
                }
            }
        }
        // A position that closed while degraded is forgotten (no stale RECOVERED later).
        self.reserved.retain(|m, _| live.contains(m));
        self.state.retain(|m, _| live.contains(m));
        self.protect.retain(|m, _| live.contains(m));
        out
    }

    /// Positions currently degraded.
    #[must_use]
    pub fn degraded_count(&self) -> usize {
        self.state
            .len()
            .saturating_add(self.protect.len())
            .saturating_add(self.reserved.len())
    }
}

/// Why a replay resume clock was refused. Every variant is fatal at startup: a daemon never runs on a clock it cannot trust.
#[derive(Debug, PartialEq, Eq)]
pub enum ResumeClockRefusal {
    /// `PQ_FLOW_RESUME_MS` set without the offline-paper-replay declaration.
    NotInOfflineReplayMode,
    /// The offline-paper-replay declaration combined with `--live`.
    LiveMode,
    /// The offline-paper-replay declaration without the paper model lane armed (no inference endpoint).
    ModelLaneNotArmed,
    /// The value is not a plain positive integer of milliseconds (sign, whitespace, hex, empty, overflow, zero).
    InvalidValue,
    /// The value lies in the future of the wall clock: a replay clock describes a PAST instant.
    InTheFuture,
}

/// The ONLY consumer of `PQ_FLOW_RESUME_MS`: the instant at which the durable flow history declares the feed resumes
/// (so a hole between the checkpoint's newest cursor and that instant becomes a NAMED gap). It is honoured only when the
/// process is explicitly declared an offline paper replay (`PQ_OFFLINE_PAPER_REPLAY=1`), is not `--live`, and has the paper
/// model lane armed. It does NOT touch the wire clock, any freshness/staleness bound, the inference deadlines (those run on
/// `Instant`), the missing-history ledger or the held-state ledger. Returns `(resume_ms, declared)`.
pub fn resolve_flow_resume_clock(
    live_mode: bool,
    model_armed: bool,
    offline_replay_flag: Option<&str>,
    resume_env: Option<&str>,
    wall_ms: i64,
) -> Result<(i64, bool), ResumeClockRefusal> {
    let replay_declared = offline_replay_flag == Some("1");
    let Some(v) = resume_env else {
        // Not asking for a replay clock: the wall clock, exactly as before. A bare replay declaration changes nothing.
        return Ok((wall_ms, false));
    };
    if live_mode {
        return Err(ResumeClockRefusal::LiveMode);
    }
    if !replay_declared {
        return Err(ResumeClockRefusal::NotInOfflineReplayMode);
    }
    if !model_armed {
        return Err(ResumeClockRefusal::ModelLaneNotArmed);
    }
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ResumeClockRefusal::InvalidValue);
    }
    let ms: i64 = v.parse().map_err(|_| ResumeClockRefusal::InvalidValue)?;
    if ms <= 0 {
        return Err(ResumeClockRefusal::InvalidValue);
    }
    if ms > wall_ms {
        return Err(ResumeClockRefusal::InTheFuture);
    }
    Ok((ms, true))
}

#[cfg(test)]
mod resume_clock_tests {
    use super::*;
    const WALL: i64 = 1_790_000_000_000;
    const T: i64 = 1_788_965_347_168;
    fn r(
        live: bool,
        armed: bool,
        flag: Option<&str>,
        v: Option<&str>,
    ) -> Result<(i64, bool), ResumeClockRefusal> {
        resolve_flow_resume_clock(live, armed, flag, v, WALL)
    }
    #[test]
    fn no_resume_env_is_the_wall_clock_whatever_the_flag() {
        assert_eq!(r(false, true, None, None), Ok((WALL, false)));
        assert_eq!(r(false, true, Some("1"), None), Ok((WALL, false)));
        assert_eq!(r(true, false, None, None), Ok((WALL, false)));
    }
    #[test]
    fn honoured_only_in_declared_offline_paper_replay_with_the_model_lane_armed() {
        assert_eq!(
            r(false, true, Some("1"), Some("1788965347168")),
            Ok((T, true))
        );
        assert_eq!(
            r(false, true, None, Some("1788965347168")),
            Err(ResumeClockRefusal::NotInOfflineReplayMode)
        );
        assert_eq!(
            r(false, true, Some("0"), Some("1788965347168")),
            Err(ResumeClockRefusal::NotInOfflineReplayMode)
        );
        assert_eq!(
            r(false, true, Some("true"), Some("1788965347168")),
            Err(ResumeClockRefusal::NotInOfflineReplayMode)
        );
        assert_eq!(
            r(true, true, Some("1"), Some("1788965347168")),
            Err(ResumeClockRefusal::LiveMode)
        );
        assert_eq!(
            r(true, false, None, Some("1788965347168")),
            Err(ResumeClockRefusal::LiveMode)
        );
        assert_eq!(
            r(false, false, Some("1"), Some("1788965347168")),
            Err(ResumeClockRefusal::ModelLaneNotArmed)
        );
    }
    #[test]
    fn invalid_values_are_refused_never_defaulted() {
        for bad in [
            "",
            " ",
            "0",
            "-5",
            "+5",
            "12 ",
            " 12",
            "0x10",
            "1e9",
            "1.5",
            "abc",
            "99999999999999999999999",
            "1788965347168\n",
        ] {
            assert_eq!(
                r(false, true, Some("1"), Some(bad)),
                Err(ResumeClockRefusal::InvalidValue),
                "{bad:?}"
            );
        }
        assert_eq!(
            r(false, true, Some("1"), Some("1790000000001")),
            Err(ResumeClockRefusal::InTheFuture)
        );
        assert_eq!(
            r(false, true, Some("1"), Some("1790000000000")),
            Ok((WALL, true))
        );
    }
}
