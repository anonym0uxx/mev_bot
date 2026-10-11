//! Restart durability pin: the controlled-shutdown path persists the held ledger (new generation + decision clock);
//! pq_daemon must seal the capture to that ledger before the stop gate can break the loop. Otherwise the durable
//! ledger is ahead of the sealed capture and every restart from a handed-off stop refuses launch recovery
//! (`capture_seal_behind_restart`). Behavioural proof: the packaged handoff -> restore drill (pkg_drill.py).
#![forbid(unsafe_code)]

#[test]
fn the_stop_gate_seals_the_capture_before_it_can_exit_the_loop() {
    let src = include_str!("../src/bin/pq_daemon.rs");
    let call = src
        .find("let gate = handle_stop_request(")
        .expect("stop gate call site");
    let matched = src[call..].find("match gate {").expect("gate match") + call;
    let between = &src[call..matched];
    assert!(
        between.contains("capture_seal_if_new("),
        "capture must be sealed between the controlled shutdown and the stop-gate match"
    );
    // The gate's completion arms still exit the loop: the seal must precede every `break` they contain.
    assert!(!between.contains("break;"));
}
