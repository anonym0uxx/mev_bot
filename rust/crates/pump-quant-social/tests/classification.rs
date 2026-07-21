//! Leaf tests: eight-state fade-first classification (§29.8).

use pump_quant_social::classification::{classify, ClassificationConfig, DeterminantBundle};
use pump_quant_social::types::{DeterminantScore, SourceState};

fn score(v: i64) -> DeterminantScore {
    DeterminantScore {
        value_bps: v,
        sample_size: 30,
        confidence_bps: 5_000,
    }
}

/// A neutral, well-sampled bundle with no fade flags — the caller mutates fields to
/// steer classification.
fn neutral() -> DeterminantBundle {
    DeterminantBundle {
        d1: score(0),
        d2: score(0),
        d3: score(0),
        d4: score(0),
        d5: score(0),
        d6: score(0),
        d7: score(5_000),
        d8: score(5_000),
        d9: score(0),
        d10: score(0),
        shill_suspect: false,
        post_peak_persistent: false,
        bot_farm: false,
        echo_heavy: false,
        total_sample: 30,
    }
}

#[test]
fn insufficient_sample_is_the_fade_first_default() {
    let cfg = ClassificationConfig::fade_first_default();
    let mut b = neutral();
    b.total_sample = 5; // below min_sample (12)
    assert_eq!(classify(&b, &cfg).state, SourceState::InsufficientSample);
}

#[test]
fn shill_suspect_takes_priority() {
    let cfg = ClassificationConfig::fade_first_default();
    let mut b = neutral();
    // Even with strong pre-flow evidence, the shill flag fades first.
    b.d3 = score(9_000);
    b.d1 = score(9_000);
    b.d2 = score(8_000);
    b.shill_suspect = true;
    assert_eq!(classify(&b, &cfg).state, SourceState::PaidShillSuspect);
}

#[test]
fn engagement_farm_and_echo_and_late_exit() {
    let cfg = ClassificationConfig::fade_first_default();

    let mut farm = neutral();
    farm.bot_farm = true;
    assert_eq!(classify(&farm, &cfg).state, SourceState::EngagementFarm);

    let mut echo = neutral();
    echo.echo_heavy = true;
    assert_eq!(classify(&echo, &cfg).state, SourceState::CopyEchoAccount);

    let mut late = neutral();
    late.post_peak_persistent = true;
    assert_eq!(
        classify(&late, &cfg).state,
        SourceState::LateExitLiquidityPromoter
    );

    // D2 value at/below the late-exit threshold also triggers it.
    let mut late2 = neutral();
    late2.d2 = score(-3_000);
    assert_eq!(
        classify(&late2, &cfg).state,
        SourceState::LateExitLiquidityPromoter
    );
}

#[test]
fn pre_flow_alpha_requires_beating_the_control() {
    let cfg = ClassificationConfig::fade_first_default();

    // Strong raw markout but ZERO excess over control → NOT pre-flow alpha.
    let mut selection_only = neutral();
    selection_only.d1 = score(6_000);
    selection_only.d3 = score(0); // fails control
    selection_only.d2 = score(2_000);
    let st = classify(&selection_only, &cfg).state;
    assert_eq!(st, SourceState::FlowAmplifier); // positive markout, rides flow

    // Beats the control from a pre-flow posture → PreFlowAlpha.
    let mut alpha = neutral();
    alpha.d3 = score(2_000); // >= 1500
    alpha.d1 = score(2_000); // >= 1000
    alpha.d2 = score(5_000); // > 0
    assert_eq!(classify(&alpha, &cfg).state, SourceState::PreFlowAlpha);
}

#[test]
fn flow_amplifier_and_organic() {
    let cfg = ClassificationConfig::fade_first_default();

    let mut amp = neutral();
    amp.d1 = score(500); // >= amplifier threshold 300, but d3 excess 0
    assert_eq!(classify(&amp, &cfg).state, SourceState::FlowAmplifier);

    let mut organic = neutral();
    organic.d1 = score(100); // below amplifier threshold, no fade flags
    assert_eq!(
        classify(&organic, &cfg).state,
        SourceState::OrganicCommunityNode
    );
}

#[test]
fn confidence_is_min_of_driving_determinants() {
    let cfg = ClassificationConfig::fade_first_default();
    let mut alpha = neutral();
    alpha.d3 = DeterminantScore {
        value_bps: 2_000,
        sample_size: 30,
        confidence_bps: 2_000,
    };
    alpha.d1 = score(2_000); // conf 5000
    alpha.d2 = score(5_000); // conf 5000
    let c = classify(&alpha, &cfg);
    assert_eq!(c.state, SourceState::PreFlowAlpha);
    // min(2000, 5000, 5000) = 2000 — thin D3 evidence caps confidence.
    assert_eq!(c.confidence_bps, 2_000);
    assert_eq!(c.decay_half_life_ns, cfg.decay_half_life_ns);
}
