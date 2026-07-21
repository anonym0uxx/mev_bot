//! Leaf pl_advance: probe-ladder capital-scaling state machine (criterion 27).

use pump_quant_strategy::probe_ladder::{advance_ladder, LadderConfig, ProbeLadder, RungOutcome};

#[test]
fn size_schedule_doubles_and_clamps() {
    let cfg = LadderConfig::test(); // base 1_000, max_rung 4, cap 12_000
                                    // 1_000 * 2^r, clamped at 12_000.
    assert_eq!(cfg.size_at_rung(0), 1_000);
    assert_eq!(cfg.size_at_rung(1), 2_000);
    assert_eq!(cfg.size_at_rung(2), 4_000);
    assert_eq!(cfg.size_at_rung(3), 8_000);
    assert_eq!(cfg.size_at_rung(4), 12_000); // 16_000 clamped to cap
                                             // Rung above max is clamped to max_rung first.
    assert_eq!(cfg.size_at_rung(9), cfg.size_at_rung(4));
}

#[test]
fn advances_only_on_reconciled_positive() {
    let cfg = LadderConfig::test();
    let s0 = ProbeLadder::new();
    assert_eq!(s0, ProbeLadder::Probing { rung: 0 });

    // Neutral holds the rung.
    let s = advance_ladder(s0, RungOutcome::Neutral, &cfg);
    assert_eq!(s, ProbeLadder::Probing { rung: 0 });

    // Positive advances to rung 1 and enters Scaled.
    let s = advance_ladder(s, RungOutcome::ReconciledPositive, &cfg);
    assert_eq!(s, ProbeLadder::Scaled { rung: 1 });
    assert_eq!(s.planned_size(&cfg), 2_000);

    // Two more positives reach rung 3.
    let s = advance_ladder(s, RungOutcome::ReconciledPositive, &cfg);
    let s = advance_ladder(s, RungOutcome::ReconciledPositive, &cfg);
    assert_eq!(s, ProbeLadder::Scaled { rung: 3 });
    assert_eq!(s.planned_size(&cfg), 8_000);
}

#[test]
fn caps_rung_at_max() {
    let cfg = LadderConfig::test();
    let mut s = ProbeLadder::new();
    for _ in 0..10 {
        s = advance_ladder(s, RungOutcome::ReconciledPositive, &cfg);
    }
    assert_eq!(s.rung(), cfg.max_rung);
    assert_eq!(s.planned_size(&cfg), 12_000);
}

#[test]
fn deterioration_halts_terminally() {
    let cfg = LadderConfig::test();
    let s = advance_ladder(ProbeLadder::new(), RungOutcome::ReconciledPositive, &cfg);
    assert_eq!(s, ProbeLadder::Scaled { rung: 1 });

    let halted = advance_ladder(s, RungOutcome::Deteriorated, &cfg);
    assert_eq!(halted, ProbeLadder::Halted { rung: 1 });

    // Halt is terminal: no outcome re-advances it.
    for out in [
        RungOutcome::ReconciledPositive,
        RungOutcome::Neutral,
        RungOutcome::Deteriorated,
    ] {
        assert_eq!(
            advance_ladder(halted, out, &cfg),
            ProbeLadder::Halted { rung: 1 }
        );
    }
}

#[test]
fn deterministic_replay() {
    let cfg = LadderConfig::test();
    let seq = [
        RungOutcome::Neutral,
        RungOutcome::ReconciledPositive,
        RungOutcome::ReconciledPositive,
        RungOutcome::Neutral,
    ];
    let run = |cfg: &LadderConfig| {
        let mut s = ProbeLadder::new();
        for o in seq {
            s = advance_ladder(s, o, cfg);
        }
        s
    };
    assert_eq!(run(&cfg), run(&cfg));
    assert_eq!(run(&cfg), ProbeLadder::Scaled { rung: 2 });
}
