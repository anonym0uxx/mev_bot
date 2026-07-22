// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'memory_pressure' component (leaf 'shed_plan_cumulative').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_core::memory_pressure::*;

#[test]
fn shed_plan_cumulative_prop() {
    let levels = [
        PressureLevel::Nominal,
        PressureLevel::Soft,
        PressureLevel::Hard,
        PressureLevel::Critical,
    ];
    let count = |p: ShedPlan| {
        p.shed_research as u32
            + p.compact_caches as u32
            + p.narrow_best_of_n as u32
            + p.flush_and_release as u32
    };

    // Concrete plans at each level.
    assert_eq!(
        ShedPlan::for_level(PressureLevel::Nominal),
        ShedPlan::none()
    );
    assert!(!ShedPlan::for_level(PressureLevel::Nominal).is_shedding());
    let soft = ShedPlan::for_level(PressureLevel::Soft);
    assert!(
        soft.shed_research
            && soft.compact_caches
            && !soft.narrow_best_of_n
            && !soft.flush_and_release
    );
    let hard = ShedPlan::for_level(PressureLevel::Hard);
    assert!(
        hard.shed_research
            && hard.compact_caches
            && hard.narrow_best_of_n
            && !hard.flush_and_release
    );
    let crit = ShedPlan::for_level(PressureLevel::Critical);
    assert!(
        crit.shed_research
            && crit.compact_caches
            && crit.narrow_best_of_n
            && crit.flush_and_release
    );

    // Concrete flag counts (edge cases at both extremes).
    assert_eq!(count(ShedPlan::for_level(PressureLevel::Nominal)), 0);
    assert_eq!(count(ShedPlan::for_level(PressureLevel::Critical)), 4);

    // Each escalation is a superset (cumulative + monotone) and is_shedding
    // agrees with "any flag set".
    for w in levels.windows(2) {
        let l = ShedPlan::for_level(w[0]);
        let h = ShedPlan::for_level(w[1]);
        assert!(count(h) >= count(l));
        assert!(!l.shed_research || h.shed_research);
        assert!(!l.compact_caches || h.compact_caches);
        assert!(!l.narrow_best_of_n || h.narrow_best_of_n);
        assert!(!l.flush_and_release || h.flush_and_release);
        assert_eq!(h.is_shedding(), count(h) > 0);
    }
}
