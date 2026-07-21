#![allow(unused_imports)]
use pump_quant_execution::ex_sell_ladder_escalate::*;

fn reference(level: u8, elapsed_ms: u64, unfilled_bps: u32, t: &LadderThresholds) -> bool {
    if level >= 4 {
        return false;
    }
    let idx = (level as usize).min(4);
    elapsed_ms >= t.level_timeout_ms[idx] && unfilled_bps >= t.min_unfilled_bps
}

#[test]
fn defaults_match_legacy_ladder_timeouts() {
    let t = LadderThresholds::default();
    assert_eq!(t.level_timeout_ms, [3_000, 3_000, 5_000, 5_000, 10_000]);
    assert_eq!(t.min_unfilled_bps, 100);
}

#[test]
fn escalates_after_timeout_when_unfilled() {
    let t = LadderThresholds::default();
    // level 0, 3s timeout: at exactly 3000ms with 5000 bps unfilled -> escalate.
    assert!(should_escalate(0, 3_000, 5_000, &t));
    assert_eq!(
        should_escalate(0, 3_000, 5_000, &t),
        reference(0, 3_000, 5_000, &t)
    );
}

#[test]
fn no_escalation_before_timeout() {
    let t = LadderThresholds::default();
    assert!(!should_escalate(2, 4_999, 10_000, &t)); // level 2 timeout is 5000
    assert_eq!(
        should_escalate(2, 4_999, 10_000, &t),
        reference(2, 4_999, 10_000, &t)
    );
}

#[test]
fn no_escalation_when_essentially_filled() {
    let t = LadderThresholds::default();
    // Timed out but unfilled below the 100 bps floor -> nothing to escalate for.
    assert!(!should_escalate(1, 9_000, 50, &t));
    assert_eq!(
        should_escalate(1, 9_000, 50, &t),
        reference(1, 9_000, 50, &t)
    );
}

#[test]
fn top_level_never_escalates() {
    let t = LadderThresholds::default();
    assert!(!should_escalate(4, 1_000_000, 10_000, &t));
    assert!(!should_escalate(9, 1_000_000, 10_000, &t)); // clamped, still false
}

#[test]
fn sweep_matches_reference() {
    let t = LadderThresholds {
        level_timeout_ms: [1_000, 2_000, 3_000, 4_000, 5_000],
        min_unfilled_bps: 250,
    };
    for level in 0u8..6 {
        for &elapsed in &[0u64, 999, 1_000, 2_000, 3_500, 100_000] {
            for &unfilled in &[0u32, 249, 250, 251, 9_999] {
                assert_eq!(
                    should_escalate(level, elapsed, unfilled, &t),
                    reference(level, elapsed, unfilled, &t),
                    "level={level} elapsed={elapsed} unfilled={unfilled}"
                );
            }
        }
    }
}
