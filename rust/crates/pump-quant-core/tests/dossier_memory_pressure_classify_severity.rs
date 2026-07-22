// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'memory_pressure' component (leaf 'classify_severity').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::memory_pressure::*;

#[test]
fn classify_severity_prop() {
    // Exact-percentage budget so RSS band edges are integer-clean; park the
    // available dimension at 0 floors so only RSS scores in this fixture.
    let t = PressureThresholds {
        budget_bytes: 1_000_000,
        avail_soft_bytes: 0,
        avail_hard_bytes: 0,
        avail_critical_bytes: 0,
        ..PressureThresholds::default()
    };

    // Concrete band edges, inclusive lower edge.
    assert_eq!(
        classify(&MemorySample::rss(699_999), &t),
        PressureLevel::Nominal
    );
    assert_eq!(
        classify(&MemorySample::rss(700_000), &t),
        PressureLevel::Soft
    );
    assert_eq!(
        classify(&MemorySample::rss(850_000), &t),
        PressureLevel::Hard
    );
    assert_eq!(
        classify(&MemorySample::rss(950_000), &t),
        PressureLevel::Critical
    );

    // Monotone non-decreasing in RSS across the whole range.
    let mut prev = PressureLevel::Nominal;
    let mut r: u64 = 0;
    while r <= 1_200_000 {
        let cur = classify(&MemorySample::rss(r), &t);
        assert!(
            cur >= prev,
            "classify must be monotone non-decreasing in RSS"
        );
        prev = cur;
        r += 5_000;
    }

    // More-severe dimension wins: RSS Soft (72%) but available Hard -> Hard.
    let d = PressureThresholds::default();
    let s = MemorySample::new(72 * (1u64 << 30) / 100, 256 * (1u64 << 20));
    assert_eq!(classify(&s, &d), PressureLevel::Hard);

    // Edge: unobserved available never fabricates pressure.
    assert_eq!(
        classify(&MemorySample::rss(10 * (1u64 << 20)), &d),
        PressureLevel::Nominal
    );
}
