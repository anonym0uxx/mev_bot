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

/// Operator file that acknowledges a protective handoff (another protector now owns the exposure).
/// Its mere existence is the acknowledgement; the daemon never creates it.
pub const PROTECTIVE_HANDOFF_ACK_FILE: &str = "data/PROTECTIVE_HANDOFF_ACK";

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

/// What the daemon does with a stop request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopGate {
    /// Nothing held, nothing outstanding: safe to stop.
    CompleteFlat,
    /// Exposure remains but the operator acknowledged a protective handoff: stop.
    CompleteHandedOff,
    /// Exposure remains and nobody else protects it: DO NOT stop. Remain blocked, alert, report
    /// incomplete shutdown. No liquidation is attempted — that policy is not agreed.
    Incomplete(StopAssessment),
}

/// Pure decision from an assessment and the acknowledgement. Fail-closed: any exposure without an ack
/// is incomplete.
#[must_use]
pub fn decide_stop(a: StopAssessment, handoff_acked: bool) -> StopGate {
    if a.is_flat_and_reconciled() {
        StopGate::CompleteFlat
    } else if handoff_acked {
        StopGate::CompleteHandedOff
    } else {
        StopGate::Incomplete(a)
    }
}

/// Handle a stop request for an armed engine: block entries, invalidate queued risk-increasing intents,
/// persist held/pending state, then decide. The engine keeps protecting positions either way.
pub fn handle_stop_request(engine: &mut Engine, ack_file: &Path) -> StopGate {
    let report = engine.model_controlled_shutdown();
    if !report.persisted {
        // Cannot write the record: never claim completion on an unrecorded exposure.
        let a = engine.model_stop_assessment();
        return if a.is_flat_and_reconciled() {
            StopGate::CompleteFlat
        } else {
            StopGate::Incomplete(a)
        };
    }
    decide_stop(engine.model_stop_assessment(), ack_file.exists())
}
