//! Integration tests for the `sentiment-enrich` brain seam: the filter must
//! NEVER block, drop or reorder the stream — every input line comes out, in
//! order, either byte-identical or byte-prefix-identical with exactly the
//! three annotation fields spliced before the closing brace. `--replay` is a
//! pure function of stdin + fixture (§22, byte-stable); every failure mode
//! (null fixture entry, out-of-range values, unreachable server, oversize
//! line, non-JSON line) degrades to ABSENCE (§6.4) — never to a mutated or
//! missing line. No test starts a server; the "live" tests point at a closed
//! loopback port to prove fail-open.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use pq_social_capture::json;

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Run `sentiment-enrich` with the given flags and stdin, model-id env
/// scrubbed (tests pin it explicitly where it matters).
fn run(args: &[&str], stdin: &str, envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pq-social-capture"));
    cmd.arg("sentiment-enrich")
        .args(args)
        .env_remove("LLAMA_SERVER_URL")
        .env_remove("LLAMA_MODEL_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("binary runs");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("stdin written");
    child.wait_with_output().expect("binary exits")
}

fn stdout_lines(out: &Output) -> Vec<String> {
    String::from_utf8(out.stdout.clone())
        .expect("stdout is UTF-8")
        .lines()
        .map(String::from)
        .collect()
}

/// Four normalized capture lines (the `normalize.py` contract) driving the
/// four-entry `sentiment_replay.json` fixture: enrich, simulated failure,
/// out-of-range rejection, enrich.
const INPUT: &str = concat!(
    "{\"platform\":\"x\",\"author\":\"degen\",\"community\":\"\",\"text\":\"send it $WIF\",\"likes\":420,\"reposts\":69,\"replies\":12,\"echo\":false,\"observed_at_ns\":42}\n",
    "{\"platform\":\"tiktok\",\"author\":\"memelord\",\"community\":\"\",\"text\":\"$BONK insane\",\"likes\":5,\"reposts\":1,\"replies\":2,\"echo\":false,\"observed_at_ns\":43}\n",
    "{\"platform\":\"web\",\"author\":\"site\",\"community\":\"site\",\"text\":\"trending $PONKE\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\"observed_at_ns\":44}\n",
    "{\"platform\":\"x\",\"author\":\"fudder\",\"community\":\"\",\"text\":\"$WIF is a rug, dev dumped\",\"likes\":1,\"reposts\":0,\"replies\":0,\"echo\":true,\"observed_at_ns\":45}\n",
);

// -------------------------------------------------------------------- replay

#[test]
fn replay_enriches_and_fails_open_per_entry() {
    let out = run(&["--replay", &fixture("sentiment_replay.json")], INPUT, &[]);
    assert!(out.status.success(), "{out:?}");
    let lines = stdout_lines(&out);
    let input: Vec<&str> = INPUT.lines().collect();
    assert_eq!(lines.len(), 4, "never drops");
    // Line 1: enriched from fixture entry 1.
    assert_eq!(
        lines[0],
        format!(
            "{},\"sentiment_bp\":9100,\"sentiment_conf_bp\":7000,\"sentiment_model\":\"local-llm-v0\"}}",
            input[0].strip_suffix('}').unwrap()
        )
    );
    // Line 2: null fixture entry = simulated failure -> UNCHANGED (§6.4).
    assert_eq!(lines[1], input[1]);
    // Line 3: out-of-range fixture entry (12000) rejected, NOT clamped ->
    // unchanged.
    assert_eq!(lines[2], input[2]);
    // Line 4: bearish enrichment from fixture entry 4 (content_hash ignored).
    assert_eq!(
        lines[3],
        format!(
            "{},\"sentiment_bp\":300,\"sentiment_conf_bp\":8000,\"sentiment_model\":\"local-llm-v0\"}}",
            input[3].strip_suffix('}').unwrap()
        )
    );
}

#[test]
fn replay_is_byte_exact_deterministic_across_runs() {
    let a = run(&["--replay", &fixture("sentiment_replay.json")], INPUT, &[]);
    let b = run(&["--replay", &fixture("sentiment_replay.json")], INPUT, &[]);
    assert!(a.status.success() && b.status.success());
    assert_eq!(a.stdout, b.stdout, "byte-identical replays (§22)");
}

#[test]
fn every_output_line_is_valid_json_with_originals_untouched() {
    let out = run(&["--replay", &fixture("sentiment_replay.json")], INPUT, &[]);
    for (i, (inp, got)) in INPUT.lines().zip(stdout_lines(&out)).enumerate() {
        // Order + prefix preservation: the output line is the input line up
        // to its closing brace, byte-for-byte.
        assert!(
            got.starts_with(inp.strip_suffix('}').unwrap()),
            "line {i} reordered or mutated"
        );
        let v = json::parse(&got).expect("valid JSON out");
        assert_eq!(
            v.get("observed_at_ns"),
            json::parse(inp).unwrap().get("observed_at_ns"),
            "line {i} original field mutated"
        );
    }
}

#[test]
fn replay_exhaustion_is_absence_not_an_error() {
    // 4 enrichable lines + a 5th that finds the fixture empty of entries.
    let five = format!(
        "{INPUT}{}\n",
        "{\"platform\":\"x\",\"author\":\"late\",\"community\":\"\",\"text\":\"gm $WIF\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false,\"observed_at_ns\":46}"
    );
    let out = run(&["--replay", &fixture("sentiment_replay.json")], &five, &[]);
    assert!(out.status.success(), "exhaustion fails open: {out:?}");
    let lines = stdout_lines(&out);
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[4], five.lines().nth(4).unwrap(), "unchanged");
}

#[test]
fn non_enrichable_lines_pass_through_without_consuming_fixture_entries() {
    // A non-JSON line and a no-text line interleave; they must NOT consume
    // fixture entries, so the following real line still gets entry 1.
    let input = concat!(
        "this is not json\n",
        "{\"platform\":\"x\",\"author\":\"quiet\",\"community\":\"\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false}\n",
        "{\"platform\":\"x\",\"author\":\"degen\",\"community\":\"\",\"text\":\"send it $WIF\",\"likes\":1,\"reposts\":0,\"replies\":0,\"echo\":false}\n",
    );
    let out = run(&["--replay", &fixture("sentiment_replay.json")], input, &[]);
    assert!(out.status.success());
    let lines = stdout_lines(&out);
    assert_eq!(lines[0], "this is not json", "passed through verbatim");
    assert_eq!(lines[1], input.lines().nth(1).unwrap(), "no text -> skip");
    assert!(
        lines[2].contains("\"sentiment_bp\":9100"),
        "first fixture entry landed on the first enrichable line: {}",
        lines[2]
    );
}

#[test]
fn model_id_env_is_recorded_as_provenance() {
    let out = run(
        &["--replay", &fixture("sentiment_replay.json")],
        INPUT,
        &[("LLAMA_MODEL_ID", "glm-5.2-q3kxl")],
    );
    assert!(stdout_lines(&out)[0].ends_with("\"sentiment_model\":\"glm-5.2-q3kxl\"}"));
}

// ----------------------------------------------------------------- --require

#[test]
fn require_flag_makes_failure_loud() {
    let out = run(
        &["--replay", &fixture("sentiment_replay.json"), "--require"],
        INPUT,
        &[],
    );
    assert_eq!(out.status.code(), Some(1), "loud exit");
    let lines = stdout_lines(&out);
    // The failing line itself was still emitted unchanged before exiting —
    // never dropped — and nothing after it was consumed.
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1], INPUT.lines().nth(1).unwrap());
    assert!(String::from_utf8_lossy(&out.stderr).contains("FATAL (--require)"));
}

// --------------------------------------------------------------- passthrough

#[test]
fn passthrough_is_byte_identical() {
    let out = run(&["--passthrough"], INPUT, &[]);
    assert!(out.status.success());
    assert_eq!(out.stdout, INPUT.as_bytes(), "identity filter");
}

#[test]
fn conflicting_flags_refuse_to_start() {
    let out = run(
        &[
            "--replay",
            &fixture("sentiment_replay.json"),
            "--passthrough",
        ],
        "",
        &[],
    );
    assert_eq!(out.status.code(), Some(2));
}

// ------------------------------------------------------------- line-size cap

#[test]
fn oversize_line_streams_through_unchanged_and_unenriched() {
    let big = format!(
        "{{\"platform\":\"x\",\"text\":\"{}\"}}",
        "a".repeat(80 * 1024)
    );
    let input = format!(
        "{big}\n{}\n",
        "{\"platform\":\"x\",\"author\":\"degen\",\"community\":\"\",\"text\":\"send it $WIF\",\"likes\":1,\"reposts\":0,\"replies\":0,\"echo\":false}"
    );
    let out = run(
        &["--replay", &fixture("sentiment_replay.json")],
        &input,
        &[],
    );
    assert!(out.status.success());
    let lines = stdout_lines(&out);
    assert_eq!(lines[0], big, "oversize passes through byte-identical");
    assert!(!lines[0].contains("sentiment_bp"), "never enriched");
    // The oversize line consumed NO fixture entry: entry 1 lands next.
    assert!(lines[1].contains("\"sentiment_bp\":9100"), "{}", lines[1]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("byte cap"));
}

// ------------------------------------------------------- live-mode fail-open

#[test]
fn unreachable_server_fails_open_as_absence() {
    // Port 9 on loopback: connection refused immediately — no server is ever
    // started by tests. Every line must come out unchanged, exit 0.
    let out = run(&[], INPUT, &[("LLAMA_SERVER_URL", "http://127.0.0.1:9")]);
    assert!(out.status.success(), "fail-open: {out:?}");
    assert_eq!(out.stdout, INPUT.as_bytes(), "all lines unchanged");
    assert!(String::from_utf8_lossy(&out.stderr).contains("enrichment failure"));
}

#[test]
fn unreachable_server_with_require_exits_nonzero() {
    let out = run(
        &["--require"],
        INPUT,
        &[("LLAMA_SERVER_URL", "http://127.0.0.1:9")],
    );
    assert_eq!(out.status.code(), Some(1));
    // The line that failed was still emitted unchanged before the loud exit.
    assert_eq!(stdout_lines(&out)[0], INPUT.lines().next().unwrap());
}
