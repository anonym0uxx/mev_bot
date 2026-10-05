//! Manifest writer — produces the `pumpfun_laserstream_manifest_v1_<SESSION>.json`
//! file with all capture metadata: repo SHA, schema versions, endpoint, counts,
//! file hashes, timing, and quality metrics.

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::normalizer::Normalizer;

#[derive(Serialize)]
pub struct ManifestV1 {
    pub schema_version: String,
    pub raw_schema_version: String,
    pub events_schema_version: String,
    pub repo_sha: String,
    pub session_id: String,
    pub endpoint_host: String,
    pub commitment: String,
    pub programs: Vec<String>,
    pub start_unix_ms: u64,
    pub end_unix_ms: u64,
    pub start_slot: Option<u64>,
    pub end_slot: Option<u64>,
    pub duration_minutes: u64,
    pub raw_files: Vec<RawFileInfo>,
    pub events_file: Option<EventFileInfo>,
    pub total_raw_records: u64,
    pub total_events: u64,
    pub counts: EventCounts,
    pub quality: QualityMetrics,
}

#[derive(Serialize)]
pub struct RawFileInfo {
    pub filename: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct EventFileInfo {
    pub filename: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct EventCounts {
    pub creates: u64,
    pub pump_buys: u64,
    pub pump_sells: u64,
    pub pump_completes: u64,
    pub migrations: u64,
    pub pumpswap_buys: u64,
    pub pumpswap_sells: u64,
    pub pumpswap_create_pools: u64,
    pub pumpswap_deposits: u64,
    pub pumpswap_withdraws: u64,
}

#[derive(Serialize)]
pub struct QualityMetrics {
    pub duplicates: u64,
    pub decode_failures: u64,
    pub unknown_events: u64,
    pub reconnects: u64,
    pub gaps: Vec<SlotGap>,
}

#[derive(Serialize, Clone)]
pub struct SlotGap {
    pub from_slot: u64,
    pub to_slot: u64,
}

/// Build + write the manifest file.
pub fn write_manifest(
    dir: &Path,
    session: &str,
    repo_sha: &str,
    endpoint_host: &str,
    commitment: &str,
    programs: &[String],
    start_unix_ms: u64,
    end_unix_ms: u64,
    start_slot: Option<u64>,
    end_slot: Option<u64>,
    duration_minutes: u64,
    normalizer: &Normalizer,
    reconnects: u64,
    gaps: Vec<SlotGap>,
    raw_files: Vec<RawFileInfo>,
    events_file: Option<EventFileInfo>,
    total_raw_records: u64,
    total_events: u64,
) -> std::io::Result<()> {
    let manifest = ManifestV1 {
        schema_version: "1".to_string(),
        raw_schema_version: "1".to_string(),
        events_schema_version: "1".to_string(),
        repo_sha: repo_sha.to_string(),
        session_id: session.to_string(),
        endpoint_host: endpoint_host.to_string(),
        commitment: commitment.to_string(),
        programs: programs.to_vec(),
        start_unix_ms,
        end_unix_ms,
        start_slot,
        end_slot,
        duration_minutes,
        raw_files,
        events_file,
        total_raw_records,
        total_events,
        counts: EventCounts {
            creates: normalizer.creates,
            pump_buys: normalizer.pump_buys,
            pump_sells: normalizer.pump_sells,
            pump_completes: normalizer.pump_completes,
            migrations: normalizer.migrations,
            pumpswap_buys: normalizer.pumpswap_buys,
            pumpswap_sells: normalizer.pumpswap_sells,
            pumpswap_create_pools: normalizer.pumpswap_create_pools,
            pumpswap_deposits: normalizer.pumpswap_deposits,
            pumpswap_withdraws: normalizer.pumpswap_withdraws,
        },
        quality: QualityMetrics {
            duplicates: normalizer.duplicates(),
            decode_failures: normalizer.decode_failures(),
            unknown_events: normalizer.unknown_events(),
            reconnects,
            gaps,
        },
    };

    let path = dir.join(format!("pumpfun_laserstream_manifest_v1_{session}.json"));
    let json = serde_json::to_string_pretty(&manifest)?;
    fs::write(&path, json)?;
    Ok(())
}

/// Compute SHA-256 hash of a file (for manifest integrity).
pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        use sha2::Digest;
        hasher.update(&buf[..n]);
    }
    use sha2::Digest;
    let hash = hasher.finalize();
    Ok(crate::encoding::hex_encode(&hash))
}

/// Get file size in bytes.
pub fn file_bytes(path: &Path) -> std::io::Result<u64> {
    let metadata = fs::metadata(path)?;
    Ok(metadata.len())
}
