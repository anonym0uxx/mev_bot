//! Discord Gateway lane: fixture-driven classification + pure normalization of
//! the MESSAGE_CREATE payload, dedupe/allowlist gating, and the binary's
//! fail-closed arming (spawned with the Discord tokens scrubbed — refusal
//! happens BEFORE any socket could open; no test here touches the network).

use std::collections::HashSet;
use std::process::{Command, Output};

use pq_stream_capture::dedupe::DedupeRing;
use pq_stream_capture::discord_gateway::{
    classify, extract_cashtags, extract_mints, invalid_session_action, is_designated_caller,
    normalize_message, process_message, ready_of, Allowlist, Inbound, MsgOutcome, Reconnect,
};
use pq_stream_capture::json::{self, Value};

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
}

fn frame(name: &str) -> Value {
    json::parse(&fixture(name)).unwrap_or_else(|e| panic!("fixture {name} parse: {e}"))
}

/// Pull the `d` payload out of a DISPATCH fixture frame.
fn dispatch_d(name: &str) -> Value {
    let v = frame(name);
    match classify(&v) {
        Inbound::Dispatch { d, .. } => d.expect("dispatch has payload").clone(),
        other => panic!("{name} is not a dispatch: {other:?}"),
    }
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn alpha_allowlist() -> Allowlist {
    Allowlist {
        guilds: set(&["555000000000000001"]),
        channels: set(&["777000000000000002"]),
    }
}

fn alpha_callers() -> HashSet<String> {
    set(&["999000000000000003"])
}

// -------------------------------------------------------- classification

#[test]
fn hello_fixture_yields_heartbeat_interval() {
    assert_eq!(
        classify(&frame("discord_hello.json")),
        Inbound::Hello {
            heartbeat_interval_ms: 41250
        }
    );
}

#[test]
fn ready_fixture_extracts_session_and_resume_url() {
    let v = frame("discord_ready.json");
    let Inbound::Dispatch { t, seq, d } = classify(&v) else {
        panic!("READY is a dispatch");
    };
    assert_eq!(t, "READY");
    assert_eq!(seq, Some(1));
    let (session_id, resume_url) = ready_of(d.unwrap()).expect("READY carries session");
    assert_eq!(session_id, "a1b2c3d4e5f6072839405162738495a0");
    assert_eq!(resume_url, "wss://gateway-us-east1-b.discord.gg");
}

#[test]
fn control_op_fixtures_classify() {
    assert_eq!(
        classify(&frame("discord_reconnect.json")),
        Inbound::Reconnect
    );
    assert_eq!(
        classify(&frame("discord_invalid_session_resumable.json")),
        Inbound::InvalidSession { resumable: true }
    );
    assert_eq!(
        classify(&frame("discord_invalid_session_dead.json")),
        Inbound::InvalidSession { resumable: false }
    );
    assert_eq!(
        classify(&frame("discord_heartbeat_ack.json")),
        Inbound::HeartbeatAck
    );
}

#[test]
fn resume_vs_reidentify_decision_on_op7_vs_op9() {
    // op 7 RECONNECT → RESUME; op 9 resumable=true → RESUME; false → re-IDENTIFY.
    assert_eq!(
        classify(&frame("discord_reconnect.json")),
        Inbound::Reconnect
    );
    assert_eq!(invalid_session_action(true), Reconnect::Resume);
    assert_eq!(invalid_session_action(false), Reconnect::Reidentify);
}

// -------------------------------------------------- normalization + gating

#[test]
fn alpha_message_normalizes_with_caller_cashtag_and_mint() {
    let d = dispatch_d("discord_message_create_alpha.json");
    let line = normalize_message(&d, &alpha_callers());
    let v = json::parse(&line).unwrap();
    assert_eq!(v.get("lane").and_then(Value::as_str), Some("discord_alpha"));
    assert_eq!(v.get("platform").and_then(Value::as_str), Some("discord"));
    assert_eq!(
        v.get("guild_id").and_then(Value::as_str),
        Some("555000000000000001")
    );
    assert_eq!(
        v.get("channel_id").and_then(Value::as_str),
        Some("777000000000000002")
    );
    assert_eq!(
        v.get("author_id").and_then(Value::as_str),
        Some("999000000000000003")
    );
    assert_eq!(v.get("author").and_then(Value::as_str), Some("alphacaller"));
    assert_eq!(
        v.get("community").and_then(Value::as_str),
        Some("777000000000000002")
    );
    assert_eq!(v.get("is_designated_caller"), Some(&Value::Bool(true)));
    // cashtag + mint extracted from content.
    assert!(line.contains("\"cashtags\":[\"WIF\"]"));
    assert!(line.contains("\"mints\":[\"So11111111111111111111111111111111111111112\"]"));
}

#[test]
fn normalized_schema_has_exact_key_set() {
    let d = dispatch_d("discord_message_create_alpha.json");
    let v = json::parse(&normalize_message(&d, &alpha_callers())).unwrap();
    let Value::Object(pairs) = &v else {
        panic!("normalized line is not an object");
    };
    let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "lane",
            "platform",
            "guild_id",
            "channel_id",
            "author_id",
            "author",
            "community",
            "content",
            "is_designated_caller",
            "ts",
            "cashtags",
            "mints",
        ]
    );
}

#[test]
fn chat_message_is_not_a_designated_caller() {
    let d = dispatch_d("discord_message_create_chat.json");
    assert!(!is_designated_caller(
        &alpha_callers(),
        "222000000000000004"
    ));
    let line = normalize_message(&d, &alpha_callers());
    assert!(line.contains("\"is_designated_caller\":false"));
    assert!(line.contains("\"cashtags\":[]"));
    assert!(line.contains("\"mints\":[]"));
}

#[test]
fn process_emits_raw_and_normalized_for_allowlisted_alpha() {
    let d = dispatch_d("discord_message_create_alpha.json");
    let mut ring = DedupeRing::new(64);
    let mut out = Vec::new();
    let outcome = process_message(
        &d,
        &alpha_allowlist(),
        &alpha_callers(),
        &mut ring,
        1_700,
        &mut out,
    )
    .unwrap();
    assert_eq!(outcome, MsgOutcome::Emitted);
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "raw + normalized");
    // Raw line: §6.3 untouched d payload under lane "discord".
    assert!(lines[0].starts_with("{\"lane\":\"discord\",\"recv_unix_ms\":1700,\"raw\":{"));
    assert!(lines[0].contains("\"timestamp\":\"2024-06-01T12:00:00.000000+00:00\""));
    assert!(lines[1].starts_with("{\"lane\":\"discord_alpha\","));
    for line in &lines {
        assert!(json::parse(line).is_ok(), "invalid NDJSON: {line}");
    }
}

#[test]
fn non_allowlisted_channel_is_dropped_before_emit() {
    let d = dispatch_d("discord_message_create_other_channel.json");
    let mut ring = DedupeRing::new(64);
    let mut out = Vec::new();
    let outcome = process_message(
        &d,
        &alpha_allowlist(),
        &alpha_callers(),
        &mut ring,
        1,
        &mut out,
    )
    .unwrap();
    assert_eq!(outcome, MsgOutcome::Dropped);
    assert!(out.is_empty(), "dropped message must emit nothing");
}

#[test]
fn resumed_redelivery_dedupes_by_message_id() {
    let d = dispatch_d("discord_message_create_alpha.json");
    let mut ring = DedupeRing::new(64);
    let mut first = Vec::new();
    assert_eq!(
        process_message(
            &d,
            &alpha_allowlist(),
            &alpha_callers(),
            &mut ring,
            1,
            &mut first
        )
        .unwrap(),
        MsgOutcome::Emitted
    );
    let mut second = Vec::new();
    assert_eq!(
        process_message(
            &d,
            &alpha_allowlist(),
            &alpha_callers(),
            &mut ring,
            2,
            &mut second
        )
        .unwrap(),
        MsgOutcome::Deduped
    );
    assert!(second.is_empty(), "redelivered id emits nothing");
}

#[test]
fn cashtag_and_mint_extraction_mirror_content() {
    let d = dispatch_d("discord_message_create_alpha.json");
    let content = d.get("content").and_then(Value::as_str).unwrap();
    assert_eq!(extract_cashtags(content), vec!["WIF".to_string()]);
    assert_eq!(
        extract_mints(content),
        vec!["So11111111111111111111111111111111111111112".to_string()]
    );
}

#[test]
fn classify_never_panics_on_truncated_or_garbage_frames() {
    // Whatever the transport hands us that still parses as JSON must classify
    // without panicking; malformed JSON is rejected upstream by json::parse.
    let good = fixture("discord_message_create_alpha.json");
    for cut in 0..good.len() {
        // Truncations are almost all invalid JSON (Err) — the point is no panic.
        if let Ok(v) = json::parse(&good[..cut]) {
            let _ = classify(&v);
        }
    }
    for garbage in [
        "null",
        "true",
        "0",
        "[]",
        "{}",
        "\"x\"",
        r#"{"op":[]}"#,
        r#"{"op":0,"t":null}"#,
        r#"{"op":9}"#,
        r#"{"op":10}"#,
        r#"{"op":10,"d":"nope"}"#,
    ] {
        let v = json::parse(garbage).unwrap();
        let _ = classify(&v);
    }
}

// ------------------------------------------------------ binary behavior

fn run_scrubbed(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pq-stream-capture"))
        .args(args)
        .env_remove("DISCORD_USER_TOKEN")
        .env_remove("DISCORD_BOT_TOKEN")
        .env_remove("DISCORD_GATEWAY_URL")
        .output()
        .expect("binary runs")
}

#[test]
fn discord_missing_user_token_is_fail_closed_exit_3() {
    let out = run_scrubbed(&["discord-gateway", "--channels", "777000000000000002"]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ARMING_FAILED"), "{err}");
    assert!(err.contains("DISCORD_USER_TOKEN"), "{err}");
    assert!(out.stdout.is_empty(), "no data lines on refusal");
}

#[test]
fn discord_bot_kind_missing_bot_token_is_fail_closed_exit_3() {
    let out = run_scrubbed(&["discord-gateway", "--token-kind", "bot"]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("DISCORD_BOT_TOKEN"), "{err}");
}

#[test]
fn discord_bad_flag_is_usage_error_exit_2() {
    let out = run_scrubbed(&["discord-gateway", "--token-kind", "admin"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("token-kind"));
}

#[test]
fn selfcheck_reports_discord_token_status_without_leaking_value() {
    let out = Command::new(env!("CARGO_BIN_EXE_pq-stream-capture"))
        .arg("selfcheck")
        .env("DISCORD_USER_TOKEN", "sup3r-secret-discord-token")
        .env_remove("DISCORD_BOT_TOKEN")
        .output()
        .expect("binary runs");
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("env DISCORD_USER_TOKEN: set"), "{err}");
    assert!(err.contains("env DISCORD_BOT_TOKEN: MISSING"), "{err}");
    assert!(
        !err.contains("sup3r-secret-discord-token"),
        "token value leaked to stderr"
    );
}
