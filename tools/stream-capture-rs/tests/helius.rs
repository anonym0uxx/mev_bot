//! Helius WS lane: fixture-driven classification + §6.3 raw preservation,
//! and the binary's fail-closed arming / usage behavior (spawned with
//! credentials scrubbed — refusal happens BEFORE any socket could open; no
//! test here touches the network).

use std::process::{Command, Output};

use pq_stream_capture::emit;
use pq_stream_capture::helius_ws::{classify, slot_of, Inbound};
use pq_stream_capture::json;

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
}

// -------------------------------------------------------- classification

#[test]
fn transaction_notification_fixture_classifies_and_preserves_raw() {
    let text = fixture("helius_tx_notification.json");
    let v = json::parse(&text).unwrap();
    let Inbound::Notification { sub, result } = classify(&v) else {
        panic!("misclassified");
    };
    assert_eq!(sub, "transaction");
    let raw = json::serialize(result);
    // §6.3: base64 tx bytes, big balances and signature survive untouched.
    assert!(raw.contains("\"AvQ7CGvHhcyzMr9AZDzvSt2FN2WNHfStYaEbMV2tAFPuFCB27hCPZ0UGG2xwO3F6PW2AZ84pTx9dGRnsQvKSCwIB\""));
    assert!(raw.contains("\"preBalances\":[28279852264,158122684,1]"));
    assert!(raw.contains("\"slot\":347650001"));
    // The emitted lane line is itself valid JSON with the untouched raw.
    let line = emit::raw_line("helius_ws", 5, Some(("sub", sub)), &raw);
    let parsed = json::parse(&line).unwrap();
    assert_eq!(
        parsed.get("sub").and_then(json::Value::as_str),
        Some("transaction")
    );
    assert_eq!(
        parsed
            .get("raw")
            .and_then(|r| r.get("slot"))
            .and_then(json::Value::as_u64),
        Some(347_650_001)
    );
}

#[test]
fn slot_notification_fixture_is_the_heartbeat() {
    let v = json::parse(&fixture("helius_slot_notification.json")).unwrap();
    let Inbound::Notification { sub, result } = classify(&v) else {
        panic!("misclassified");
    };
    assert_eq!(sub, "slot");
    assert_eq!(slot_of(result), Some(347_649_965));
}

#[test]
fn account_notification_fixture_preserves_u64_max_rent_epoch() {
    let v = json::parse(&fixture("helius_account_notification.json")).unwrap();
    let Inbound::Notification { sub, result } = classify(&v) else {
        panic!("misclassified");
    };
    assert_eq!(sub, "account");
    // rentEpoch is u64::MAX — the classic f64-corruption canary (§6.3).
    assert!(json::serialize(result).contains("\"rentEpoch\":18446744073709551615"));
}

// ------------------------------------------------------ binary behavior

fn run_scrubbed(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pq-stream-capture"))
        .args(args)
        .env_remove("HELIUS_API_KEY")
        .env_remove("HELIUS_WS_URL")
        .env_remove("WEBHOOK_AUTH_SECRET")
        .env_remove("RPC_URLS")
        .output()
        .expect("binary runs")
}

#[test]
fn helius_ws_missing_key_is_fail_closed_exit_3() {
    let out = run_scrubbed(&[
        "helius-ws",
        "--programs",
        "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
    ]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ARMING_FAILED"), "{err}");
    assert!(err.contains("HELIUS_API_KEY"), "{err}");
    assert!(out.stdout.is_empty(), "no data lines on refusal");
}

#[test]
fn helius_ws_without_subscriptions_is_usage_error() {
    let out = run_scrubbed(&["helius-ws"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing to subscribe"));
}

#[test]
fn helius_ws_rejects_bad_commitment() {
    let out = run_scrubbed(&["helius-ws", "--programs", "x", "--commitment", "hopeful"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn webhook_listener_missing_secret_is_fail_closed_exit_3() {
    let out = run_scrubbed(&["webhook-listener"]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("WEBHOOK_AUTH_SECRET"), "{err}");
}

#[test]
fn fee_sampler_missing_all_providers_is_fail_closed_exit_3() {
    let out = run_scrubbed(&["fee-sampler", "--once"]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ARMING_FAILED"), "{err}");
}

#[test]
fn unknown_subcommand_and_no_subcommand_are_usage_errors() {
    assert_eq!(run_scrubbed(&["warp-drive"]).status.code(), Some(2));
    assert_eq!(run_scrubbed(&[]).status.code(), Some(2));
}

#[test]
fn selfcheck_passes_and_never_prints_secret_values() {
    let out = Command::new(env!("CARGO_BIN_EXE_pq-stream-capture"))
        .arg("selfcheck")
        .env("HELIUS_API_KEY", "sup3r-secret-value")
        .env("WEBHOOK_AUTH_SECRET", "hush-hush-value")
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("PASS: sha1 RFC 3174 vector"), "{err}");
    assert!(err.contains("env HELIUS_API_KEY: set"), "{err}");
    assert!(err.contains("env RPC_URLS: MISSING") || err.contains("env RPC_URLS: set"));
    assert!(
        !err.contains("sup3r-secret-value"),
        "secret leaked to stderr"
    );
    assert!(!err.contains("hush-hush-value"), "secret leaked to stderr");
}
