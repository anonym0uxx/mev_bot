//! Leaf ae_envelope: fast-path adaptation envelope guard (criterion 57).

use pump_quant_strategy::adaptation_envelope::{
    apply_adaptation, clamp_to_envelope, envelope_verdict, AdaptOutcome, AdaptVerdict, Envelope,
    EnvelopeMode,
};

fn env(min: i64, max: i64) -> Envelope {
    Envelope { min, max }
}

#[test]
fn verdict_classifies_position() {
    let e = env(10, 100);
    assert_eq!(envelope_verdict(9, &e), AdaptVerdict::BelowMin);
    assert_eq!(envelope_verdict(10, &e), AdaptVerdict::InRange);
    assert_eq!(envelope_verdict(55, &e), AdaptVerdict::InRange);
    assert_eq!(envelope_verdict(100, &e), AdaptVerdict::InRange);
    assert_eq!(envelope_verdict(101, &e), AdaptVerdict::AboveMax);
    assert_eq!(
        envelope_verdict(50, &env(100, 10)),
        AdaptVerdict::InvalidEnvelope
    );
}

#[test]
fn clamp_matches_manual() {
    let e = env(10, 100);
    assert_eq!(clamp_to_envelope(5, &e), 10);
    assert_eq!(clamp_to_envelope(150, &e), 100);
    assert_eq!(clamp_to_envelope(42, &e), 42);
}

#[test]
fn in_range_accepted_both_modes() {
    let e = env(10, 100);
    for mode in [EnvelopeMode::ClampToBound, EnvelopeMode::RejectOutside] {
        assert_eq!(
            apply_adaptation(50, 42, &e, mode),
            AdaptOutcome::Accepted(42)
        );
    }
}

#[test]
fn out_of_range_clamp_vs_reject() {
    let e = env(10, 100);
    assert_eq!(
        apply_adaptation(50, 5, &e, EnvelopeMode::ClampToBound),
        AdaptOutcome::Clamped(10)
    );
    assert_eq!(
        apply_adaptation(50, 500, &e, EnvelopeMode::ClampToBound),
        AdaptOutcome::Clamped(100)
    );
    assert_eq!(
        apply_adaptation(50, 5, &e, EnvelopeMode::RejectOutside),
        AdaptOutcome::Rejected
    );
    assert_eq!(
        apply_adaptation(50, 500, &e, EnvelopeMode::RejectOutside),
        AdaptOutcome::Rejected
    );
}

#[test]
fn invalid_envelope_always_rejected() {
    let bad = env(100, 10);
    for mode in [EnvelopeMode::ClampToBound, EnvelopeMode::RejectOutside] {
        assert_eq!(apply_adaptation(50, 50, &bad, mode), AdaptOutcome::Rejected);
    }
}

#[test]
fn negative_bounds_supported() {
    let e = env(-50, -10);
    assert_eq!(
        apply_adaptation(-20, -70, &e, EnvelopeMode::ClampToBound),
        AdaptOutcome::Clamped(-50)
    );
    assert_eq!(
        apply_adaptation(-20, -30, &e, EnvelopeMode::ClampToBound),
        AdaptOutcome::Accepted(-30)
    );
}
