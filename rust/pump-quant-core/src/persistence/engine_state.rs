//! Engine state file writer.
//!
//! Writes `engine-state.json` so monitoring scripts (pnl-summary.js)
//! can determine session boundaries and engine version.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

/// Write the engine state JSON file to `{data_dir}/engine-state.json`.
///
/// Called on startup and periodically (every 60s) to keep the file fresh.
pub fn write_engine_state(data_dir: &str, started_at_ms: u64) -> Result<()> {
    let state = json!({
        "daemonStartedAt": started_at_ms,
        "engineVersion": "v5-rust",
        "configVersion": "rust",
    });

    let state_path = Path::new(data_dir).join("engine-state.json");

    // Ensure directory exists
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data dir: {}", parent.display()))?;
    }

    // Write atomically via temp file + rename to avoid partial reads
    let tmp_path = state_path.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_string_pretty(&state)?)
        .with_context(|| format!("failed to write engine state to {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &state_path)
        .with_context(|| format!("failed to rename engine state to {}", state_path.display()))?;

    Ok(())
}
