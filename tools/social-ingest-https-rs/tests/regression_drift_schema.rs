//! REGRESSION HARDENING (additive) for the HTTPS social/market-intel edge —
//! owned by the end-to-end regression layer. Pure and network-free (§22):
//!
//!  1. DRIFT SENTINEL fires on PERTURBED fixtures — for birdeye and coingecko,
//!     the FNV-1a shape-hash sentinel must flag a key-set change on the real
//!     fixture's fingerprinted object. A reorder is NOT drift; a key add/rename
//!     IS. If a refactor silences the sentinel, these fail.
//!  2. FAIL-CLOSED missing-key exit 3 — the REQUIRED birdeye source refuses to
//!     start without its key, with the distinct capability-loss exit code,
//!     before any file/socket is touched.
//!  3. RECORD-SCHEMA shape assertions — the exact record tags and key ORDER of
//!     `birdeye_ohlcv_1d_v1` / `birdeye_token_overview_v1` /
//!     `birdeye_token_security_v1` are pinned, so a schema regression (renamed,
//!     reordered, added, or dropped key) is caught mechanically.

use std::process::{Command, Output};

use pq_social_capture::json::{self, Value};
use pq_social_capture::pump::{shape_hash, Sentinel};
use pq_social_capture::{birdeye, coingecko};

fn fixture_path(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn fixture_text(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

/// Run the binary keyless (all adapter creds scrubbed) — proof replay/refusal
/// never depends on a credential and never opens a socket.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pq-social-capture"))
        .args(args)
        .env_remove("TWITTERAPI_IO_KEY")
        .env_remove("TIKTOK_API_KEY")
        .env_remove("FIRECRAWL_API_KEY")
        .env_remove("CG_API_KEY")
        .env_remove("BIRDEYE_API_KEY")
        .env_remove("BIRDEYE_BUDGET_PER_MIN")
        .output()
        .expect("binary runs")
}

fn stdout_lines(out: &Output) -> Vec<String> {
    String::from_utf8(out.stdout.clone())
        .expect("stdout utf8")
        .lines()
        .map(String::from)
        .collect()
}

/// Ordered top-level key list of a JSON object line (insertion order — the
/// json codec round-trips verbatim, so this is the on-wire key order).
fn object_keys(line: &str) -> Vec<String> {
    match json::parse(line).unwrap_or_else(|e| panic!("bad json {line:?}: {e}")) {
        Value::Object(pairs) => pairs.into_iter().map(|(k, _)| k).collect(),
        other => panic!("expected object, got {other:?}"),
    }
}

// ------------------------------------------- 1. drift sentinel fires

#[test]
fn birdeye_drift_sentinel_fires_on_perturbed_ohlcv_fixture() {
    // Baseline fingerprint from the REAL fixture's first OHLCV item …
    let base = json::parse(&fixture_text("birdeye_ohlcv.json")).unwrap();
    let kind = birdeye::classify(&base).expect("ohlcv classifies");
    let base_target = birdeye::shape_target(kind, &base).expect("shape target");
    let base_hash = shape_hash(base_target).expect("hashable object");

    // … perturb the fixture by RENAMING a key in the item's object (a genuine
    // key-set change, not a reorder). Re-fingerprint the same lane path.
    let perturbed_text =
        fixture_text("birdeye_ohlcv.json").replace("\"unixTime\"", "\"unix_time\"");
    let perturbed = json::parse(&perturbed_text).unwrap();
    let p_kind = birdeye::classify(&perturbed).expect("still classifies");
    let p_target = birdeye::shape_target(p_kind, &perturbed).expect("shape target");
    let p_hash = shape_hash(p_target).expect("hashable object");

    assert_ne!(
        base_hash, p_hash,
        "renaming a key MUST change the shape hash"
    );

    // The sentinel: first observation is baseline (no drift), the perturbed
    // one fires with the OLD hash returned — exactly the SCHEMA_DRIFT trigger.
    let mut sentinel = Sentinel::default();
    assert_eq!(
        sentinel.observe_shape(base_hash),
        None,
        "first obs is baseline"
    );
    assert_eq!(
        sentinel.observe_shape(p_hash),
        Some(base_hash),
        "drift must fire, returning the prior hash"
    );
    // And a re-observation of the SAME perturbed shape is NOT drift again.
    assert_eq!(
        sentinel.observe_shape(p_hash),
        None,
        "stable shape is not re-drift"
    );
}

#[test]
fn coingecko_drift_sentinel_fires_on_perturbed_markets_fixture() {
    // The markets fixture holds one array per poll (multi-document, what the CLI
    // reads via parse_stream); fingerprint the first page's first roster entry.
    let pages = json::parse_stream(&fixture_text("coingecko_markets.json")).unwrap();
    let base = pages.into_iter().next().expect("at least one page");
    let kind = coingecko::classify(&base).expect("markets classifies");
    let base_hash = shape_hash(coingecko::shape_target(kind, &base).expect("target")).unwrap();

    let perturbed_text =
        fixture_text("coingecko_markets.json").replace("\"current_price\"", "\"price_now\"");
    let perturbed = json::parse_stream(&perturbed_text)
        .unwrap()
        .into_iter()
        .next()
        .expect("at least one page");
    let p_kind = coingecko::classify(&perturbed).expect("still classifies");
    let p_hash = shape_hash(coingecko::shape_target(p_kind, &perturbed).expect("target")).unwrap();

    assert_ne!(base_hash, p_hash, "renamed key MUST change the shape hash");

    let mut sentinel = Sentinel::default();
    assert_eq!(sentinel.observe_shape(base_hash), None);
    assert_eq!(
        sentinel.observe_shape(p_hash),
        Some(base_hash),
        "drift fires"
    );
}

#[test]
fn reordered_keys_are_not_drift_shape_hash_is_order_independent() {
    // Guard the OTHER direction: a vendor reordering keys must NOT be flagged as
    // drift (else the sentinel would cry wolf on every poll). Same key SET,
    // different order → identical hash.
    let a = json::parse(r#"{"a":1,"b":2,"c":3}"#).unwrap();
    let b = json::parse(r#"{"c":3,"a":1,"b":2}"#).unwrap();
    assert_eq!(shape_hash(&a), shape_hash(&b), "reorder is not drift");
}

#[test]
fn drift_replay_is_loud_on_stderr_and_keeps_emitting() {
    // End-to-end through the CLI: the shipped *_drift.json fixtures must trip the
    // SCHEMA_DRIFT sentinel loudly WITHOUT killing the lane (tolerant parser
    // continues). Belt-and-suspenders over the pure test above.
    for (lane, fixture) in [
        ("birdeye", "birdeye_drift.json"),
        ("coingecko", "coingecko_drift.json"),
    ] {
        let out = run(&[lane, "--replay", &fixture_path(fixture)]);
        assert!(
            out.status.success(),
            "{lane} drift must not kill the lane: {out:?}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("SCHEMA_DRIFT"),
            "{lane} drift must be loud: {stderr}"
        );
        assert!(
            !stdout_lines(&out).is_empty(),
            "{lane} keeps emitting through drift"
        );
    }
}

// ------------------------------------------- 2. fail-closed exit 3

#[test]
fn birdeye_required_source_fails_closed_exit_3_without_key() {
    // §6.7 REQUIRED source: no BIRDEYE_API_KEY = refuse, exit 3, before any file
    // or socket (the named mints file does not exist and is never opened).
    let out = run(&["birdeye", "--ohlcv-watch", "does-not-exist.txt"]);
    assert_eq!(out.status.code(), Some(3), "fail-closed exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("BIRDEYE_API_KEY"),
        "names the missing key: {stderr}"
    );
    assert!(
        stderr.contains("REQUIRED") && stderr.contains("6.7"),
        "cites §6.7: {stderr}"
    );
    assert!(out.stdout.is_empty(), "no data lines on refusal");
}

// ------------------------------------------- 3. record-schema shape

#[test]
fn birdeye_ohlcv_record_schema_is_pinned() {
    // The `birdeye_ohlcv_1d_v1` line's record tag and EXACT key order.
    let out = run(&["birdeye", "--replay", &fixture_path("birdeye_ohlcv.json")]);
    assert!(out.status.success(), "{out:?}");
    let lines = stdout_lines(&out);
    assert!(!lines.is_empty(), "ohlcv replay emits a line");
    for line in &lines {
        assert_eq!(
            object_keys(line),
            [
                "record",
                "mint",
                "observed_unix_ms",
                "bars",
                "provider",
                "interval",
                "quote"
            ],
            "ohlcv key order drifted: {line}"
        );
        let v = json::parse(line).unwrap();
        assert_eq!(
            v.get("record").and_then(Value::as_str),
            Some("birdeye_ohlcv_1d_v1"),
            "record tag drifted: {line}"
        );
    }
}

#[test]
fn birdeye_token_data_record_schemas_are_pinned() {
    // token_overview and token_security both use the raw-passthrough envelope:
    // [record, mint, observed_unix_ms, raw] with their distinct record tags.
    for (fixture, tag) in [
        ("birdeye_overview.json", "birdeye_token_overview_v1"),
        ("birdeye_security.json", "birdeye_token_security_v1"),
    ] {
        let out = run(&["birdeye", "--replay", &fixture_path(fixture)]);
        assert!(out.status.success(), "{fixture}: {out:?}");
        let lines = stdout_lines(&out);
        assert!(!lines.is_empty(), "{fixture} emits a line");
        for line in &lines {
            assert_eq!(
                object_keys(line),
                ["record", "mint", "observed_unix_ms", "raw"],
                "{fixture} key order drifted: {line}"
            );
            let v = json::parse(line).unwrap();
            assert_eq!(
                v.get("record").and_then(Value::as_str),
                Some(tag),
                "{fixture} record tag drifted: {line}"
            );
        }
    }
}

#[test]
fn birdeye_record_tag_helper_matches_wire_tags() {
    // The pure record_tag() ↔ classify() mapping is the single source of the
    // three wire tags; pin it so a code-level rename is caught even if no
    // fixture exercises that page kind.
    let ohlcv = json::parse(&fixture_text("birdeye_ohlcv.json")).unwrap();
    let overview = json::parse(&fixture_text("birdeye_overview.json")).unwrap();
    let security = json::parse(&fixture_text("birdeye_security.json")).unwrap();
    assert_eq!(
        birdeye::record_tag(birdeye::classify(&ohlcv).unwrap()),
        "birdeye_ohlcv_1d_v1"
    );
    assert_eq!(
        birdeye::record_tag(birdeye::classify(&overview).unwrap()),
        "birdeye_token_overview_v1"
    );
    assert_eq!(
        birdeye::record_tag(birdeye::classify(&security).unwrap()),
        "birdeye_token_security_v1"
    );
}
