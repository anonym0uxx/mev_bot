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
    let accepted = v.get("accepted_protective_responsibility").and_then(|x| x.as_bool()) == Some(true);
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
        st.request_seq += 1;
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
    let hex = |m: &[u8; 32]| m.iter().take(4).map(|b| format!("{b:02x}")).collect::<String>();
    let status = engine.model_held_data_status();
    let mut lines = Vec::new();
    let mut degraded = false;
    for s in &status {
        let age = |v: Option<i64>| v.map_or("none".to_string(), |a| format!("{a}ms"));
        match &s.management_ready {
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
