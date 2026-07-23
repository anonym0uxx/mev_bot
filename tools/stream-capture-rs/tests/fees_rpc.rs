//! Fee-sampler + RPC failover integration: fixture-driven response parsing
//! composed into the exact `fee_calibration_v1` record, and PumpPortal
//! payload fixtures through the raw-line contract. No network.

use pq_stream_capture::emit;
use pq_stream_capture::fees::{
    calibration_record, parse_fee_levels, parse_recent_fees, percentile_nearest_rank,
};
use pq_stream_capture::json::{self, Value};
use pq_stream_capture::pumpportal_ws;
use pq_stream_capture::rpc::redact_url;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
}

#[test]
fn fee_fixtures_compose_into_exact_calibration_record() {
    let levels = parse_fee_levels(&fixture("fee_estimate_response.json")).unwrap();
    let fees = parse_recent_fees(&fixture("recent_fees_response.json")).unwrap();
    assert_eq!(fees, vec![0, 120_000, 3000, 45_000, 45_000, 800_000]);
    let p50 = percentile_nearest_rank(&fees, 50);
    let p90 = percentile_nearest_rank(&fees, 90);
    // sorted: [0, 3000, 45000, 45000, 120000, 800000]; n=6.
    // p50 rank ceil(3.0)=3 -> 45000; p90 rank ceil(5.4)=6 -> 800000.
    assert_eq!(p50, Some(45_000));
    assert_eq!(p90, Some(800_000));
    let record = calibration_record(
        1_753_142_500_000,
        &redact_url("https://mainnet.helius-rpc.com/?api-key=SECRET"),
        Some(&levels),
        p50,
        p90,
    );
    assert_eq!(
        record,
        "{\"record\":\"fee_calibration_v1\",\"unix_ms\":1753142500000,\
         \"provider\":\"https://mainnet.helius-rpc.com\",\
         \"levels\":{\"min\":0,\"low\":1000,\"medium\":42007,\"high\":250000,\
         \"veryHigh\":1500000,\"unsafeMax\":2000000000},\
         \"recent_fees_p50\":45000,\"recent_fees_p90\":800000}"
    );
    assert!(!record.contains("SECRET"), "credentials never in records");
}

#[test]
fn method_not_found_response_degrades_to_absence_not_zero() {
    // A non-Helius provider answering getPriorityFeeEstimate.
    let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
    assert!(parse_fee_levels(body).is_err());
    let record = calibration_record(1, "https://generic-rpc.example", None, Some(5), Some(9));
    assert!(
        record.contains("\"levels\":null"),
        "loss must read as null, never 0"
    );
}

// ------------------------------------------------------------ pumpportal

#[test]
fn pumpportal_create_fixture_rides_raw_line_untouched() {
    let payload = fixture("pumpportal_create.json");
    let payload = payload.trim_end();
    // The lane validates then embeds VERBATIM.
    assert!(json::parse(payload).is_ok());
    let line = emit::raw_line("pumpportal", 42, None, payload);
    let v = json::parse(&line).unwrap();
    let raw = v.get("raw").unwrap();
    assert_eq!(raw.get("txType").and_then(Value::as_str), Some("create"));
    assert_eq!(
        raw.get("mint").and_then(Value::as_str),
        Some("NewMintPumpXXXXXXXXXXXXXXXXXXXXXXXXXXXXpump")
    );
    // Float precision text preserved verbatim inside the line (§6.3).
    assert!(line.contains("\"vSolInBondingCurve\":31.759999999999998"));
}

#[test]
fn pumpportal_migration_fixture_rides_raw_line_untouched() {
    let payload = fixture("pumpportal_migration.json");
    let payload = payload.trim_end();
    let line = emit::raw_line("pumpportal", 43, None, payload);
    let v = json::parse(&line).unwrap();
    assert_eq!(
        v.get("raw").unwrap().get("txType").and_then(Value::as_str),
        Some("migrate")
    );
    assert_eq!(
        v.get("raw").unwrap().get("pool").and_then(Value::as_str),
        Some("pump-amm")
    );
}

#[test]
fn pumpportal_subscribe_batch_matches_wire_contract() {
    let batch =
        pumpportal_ws::subscription_batch(&["NewMintPumpXXXXXXXXXXXXXXXXXXXXXXXXXXXXpump".into()]);
    assert_eq!(batch[0], "{\"method\":\"subscribeNewToken\"}");
    assert_eq!(batch[1], "{\"method\":\"subscribeMigration\"}");
    assert_eq!(
        batch[2],
        "{\"method\":\"subscribeTokenTrade\",\"keys\":[\"NewMintPumpXXXXXXXXXXXXXXXXXXXXXXXXXXXXpump\"]}"
    );
}
