//! Serving-contract guards that must keep failing loudly if someone "fixes" latency the wrong way.
//!
//! * No early-headline execution: the engine path acts only on a COMPLETE, validated completion
//!   (`complete_meta`). `complete_streaming*` returns a decision before the completion has finished and
//!   `complete_retrying*` resubmits; neither may be reachable from the app or junction execution path.
//!   "No retries implemented" is the explicit current policy; any later retry must preserve request identity
//!   and the original sampled action, which these helpers do not.

use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_execution_path_never_calls_the_streaming_headline_or_retry_helpers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rs_files(&root.join("pump-quant-app/src"), &mut files);
    rs_files(&root.join("pump-quant-junction/src"), &mut files);
    let banned = [
        "complete_streaming",
        "complete_retrying",
        "streaming_request_body",
        ".headline(",
    ];
    let mut hits = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for (i, l) in text.lines().enumerate() {
            let t = l.trim_start();
            if t.starts_with("//") {
                continue;
            }
            for b in banned {
                if l.contains(b) {
                    hits.push(format!("{}:{}: {}", f.display(), i + 1, l.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "early-headline/retry helpers reached the execution path:\n{}",
        hits.join("\n")
    );
}

#[test]
fn the_worker_acts_only_on_a_complete_completion() {
    let src =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/model_worker.rs"))
            .unwrap();
    assert!(
        src.contains("source.complete_meta("),
        "the worker must use the full-completion call"
    );
}

#[test]
fn the_engine_deadline_is_bounded_and_below_the_socket_timeout() {
    // Both bounds exist and are ordered. Neither value is VALIDATED against real full-completion latency:
    // that measurement is a Windows step (see the handoff), so these are provisional, not frozen.
    let deadline_ms = pump_quant_app::freshness::CHAMPION_MAX_DECISION_AGE_MS;
    let socket_ms = pump_quant_junction_client_timeout_ms();
    assert!(
        deadline_ms > 0 && socket_ms > deadline_ms,
        "{deadline_ms} vs {socket_ms}"
    );
}

fn pump_quant_junction_client_timeout_ms() -> u64 {
    // Parse the constant from source so this test does not need a dependency on the junction crate.
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../pump-quant-junction/src/model_lifecycle.rs"),
    )
    .unwrap();
    let line = src
        .lines()
        .find(|l| l.contains("pub const CLIENT_TIMEOUT"))
        .expect("constant");
    let secs: u64 = line
        .split("from_secs(")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    secs * 1_000
}
