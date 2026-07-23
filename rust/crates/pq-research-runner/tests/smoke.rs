//! Smoke test: feed a tiny sealed-experiment tape to the `pq-research-runner`
//! binary over stdin and assert it exits 0 and emits the ablation/baseline JSON.
use std::io::Write;
use std::process::{Command, Stdio};

const FIXTURE: &str = concat!(
    "{\"kind\":\"baseline_event\",\"index\":0,\"eligible\":true,\"launch\":true,\"score\":10,\"net_hold\":5000,\"net_tpsl\":3000}\n",
    "{\"kind\":\"ablation\",\"family\":0,\"variant\":\"combined\",\"net\":3000,\"tail\":30}\n",
    "{\"kind\":\"ablation\",\"family\":0,\"variant\":\"removed\",\"net\":2000,\"tail\":20}\n",
    "{\"kind\":\"ablation\",\"family\":0,\"variant\":\"alone\",\"net\":1000,\"tail\":10}\n",
    "{\"kind\":\"ablation\",\"family\":1,\"variant\":\"removed\",\"net\":2500,\"tail\":25}\n",
);

fn run_with_stdin(input: &str) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pq-research-runner"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pq-research-runner");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait pq-research-runner");
    (out.status.success(), String::from_utf8(out.stdout).unwrap())
}

#[test]
fn exits_zero_and_emits_expected_keys() {
    let (ok, stdout) = run_with_stdin(FIXTURE);
    assert!(ok, "binary should exit 0");
    for key in [
        "\"experiment_hash\"",
        "\"baseline\"",
        "\"ablation\"",
        "\"baselines\"",
        "\"incremental_net_lamports\"",
    ] {
        assert!(
            stdout.contains(key),
            "missing key {key} in output: {stdout}"
        );
    }
    // Baseline (combined) net recorded as 3000.
    assert!(stdout.contains("\"net_lamports\":3000"));
}

#[test]
fn deterministic_across_runs() {
    let (_, a) = run_with_stdin(FIXTURE);
    let (_, b) = run_with_stdin(FIXTURE);
    assert_eq!(
        a, b,
        "research-runner output must be byte-identical across runs"
    );
}

#[test]
fn bad_experiment_exits_nonzero() {
    let (ok, _) = run_with_stdin(
        "{\"kind\":\"ablation\",\"family\":0,\"variant\":\"???\",\"net\":0,\"tail\":0}\n",
    );
    assert!(
        !ok,
        "malformed experiment must fail closed with a non-zero exit"
    );
}
