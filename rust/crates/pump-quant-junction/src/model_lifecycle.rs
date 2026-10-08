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
        self.state.len() + self.protect.len() + self.reserved.len()
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
