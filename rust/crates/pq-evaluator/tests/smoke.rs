//! Smoke test: feed a tiny fixture tape to the `pq-evaluator` binary over stdin
//! and assert it exits 0 and emits the graded-report JSON keys.
use std::io::Write;
use std::process::{Command, Stdio};

const FIXTURE: &str = concat!(
    "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":100000,\"fees\":1000,\"tips\":500,\"failed\":0}\n",
    "{\"kind\":\"trade\",\"lane\":\"early\",\"gross\":30000,\"fees\":300,\"tips\":100,\"failed\":0}\n",
    "{\"kind\":\"baseline_event\",\"index\":0,\"eligible\":true,\"launch\":true,\"score\":10,\"net_hold\":5000,\"net_tpsl\":3000}\n",
    "{\"kind\":\"pvalue\",\"id\":1,\"p_ppm\":5000}\n",
    "{\"kind\":\"pvalue\",\"id\":2,\"p_ppm\":500000}\n",
    "{\"kind\":\"perf\",\"row\":[100,100,100,100]}\n",
    "{\"kind\":\"perf\",\"row\":[10,10,10,10]}\n",
    "{\"kind\":\"perf\",\"row\":[20,20,20,20]}\n",
    "{\"kind\":\"perf\",\"row\":[30,30,30,30]}\n",
    "{\"kind\":\"candidate\",\"id\":1}\n",
);

fn run_with_stdin(input: &str) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pq-evaluator"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pq-evaluator");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait pq-evaluator");
    (out.status.success(), String::from_utf8(out.stdout).unwrap())
}

#[test]
fn exits_zero_and_emits_expected_keys() {
    let (ok, stdout) = run_with_stdin(FIXTURE);
    assert!(ok, "binary should exit 0");
    for key in [
        "\"evaluator_hash\"",
        "\"strategy_hash\"",
        "\"net_sol_lamports\"",
        "\"baselines_defeated\"",
        "\"baselines\"",
        "\"fdr_blocks\"",
        "\"pbo_blocks\"",
        "\"promotion_reason\"",
        "\"grade\"",
    ] {
        assert!(
            stdout.contains(key),
            "missing key {key} in output: {stdout}"
        );
    }
    // Sanity: total net = (100000-1500) + (30000-400) = 128100.
    assert!(stdout.contains("\"net_sol_lamports\":128100"));
}

#[test]
fn deterministic_across_runs() {
    let (_, a) = run_with_stdin(FIXTURE);
    let (_, b) = run_with_stdin(FIXTURE);
    assert_eq!(a, b, "evaluator output must be byte-identical across runs");
}

#[test]
fn bad_tape_exits_nonzero() {
    let (ok, _) = run_with_stdin("{\"kind\":\"trade\",\"lane\":\"bogus\"}\n");
    assert!(!ok, "malformed tape must fail closed with a non-zero exit");
}
