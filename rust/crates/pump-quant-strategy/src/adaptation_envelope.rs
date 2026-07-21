//! # adaptation_envelope — fast-path adaptation envelope guard (criterion 57)
//!
//! Any online-adapted parameter must be validated against its **registered**
//! envelope `[min, max]` before it can take effect. An out-of-envelope proposal
//! is either clamped to the nearest bound or rejected outright, per the caller's
//! [`EnvelopeMode`]; an ill-formed envelope (`min > max`) is always rejected. This
//! ties online adaptation to the hardcoded-parameter law: adaptation may move a
//! parameter only inside bounds someone registered and reviewed.
//!
//! ## Constitution
//! §102 parameter law / §57 fast-path discipline. §22: integer/fixed-point only,
//! deterministic; no clock/RNG.

/// A registered adaptation envelope for one parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// Inclusive lower bound.
    pub min: i64,
    /// Inclusive upper bound.
    pub max: i64,
}

impl Envelope {
    /// A well-formed envelope has `min <= max`.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.min <= self.max
    }
}

/// Where a proposed value sits relative to its envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptVerdict {
    /// Within `[min, max]`.
    InRange,
    /// Below `min`.
    BelowMin,
    /// Above `max`.
    AboveMax,
    /// The envelope itself is ill-formed (`min > max`).
    InvalidEnvelope,
}

/// How out-of-envelope proposals are handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeMode {
    /// Clamp the proposal to the nearest bound.
    ClampToBound,
    /// Reject the proposal entirely (keep the current value).
    RejectOutside,
}

/// The outcome of an adaptation attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptOutcome {
    /// Accepted unchanged (was in range).
    Accepted(i64),
    /// Accepted after clamping to a bound.
    Clamped(i64),
    /// Rejected — the value keeps its prior setting.
    Rejected,
}

/// Classify a proposed value against its envelope (leaf helper).
#[inline]
pub fn envelope_verdict(proposed: i64, env: &Envelope) -> AdaptVerdict {
    if !env.is_valid() {
        return AdaptVerdict::InvalidEnvelope;
    }
    if proposed < env.min {
        AdaptVerdict::BelowMin
    } else if proposed > env.max {
        AdaptVerdict::AboveMax
    } else {
        AdaptVerdict::InRange
    }
}

/// Clamp `proposed` into a valid envelope (saturating at the bounds).
///
/// Panics-free and total; on an invalid envelope the lower bound is returned as a
/// deterministic fallback (callers should gate on [`Envelope::is_valid`] first —
/// [`apply_adaptation`] does).
#[inline]
pub fn clamp_to_envelope(proposed: i64, env: &Envelope) -> i64 {
    if !env.is_valid() {
        return env.min;
    }
    proposed.clamp(env.min, env.max)
}

/// The fast-path adaptation guard (leaf **ae_envelope**).
///
/// * An invalid envelope → [`AdaptOutcome::Rejected`].
/// * An in-range proposal → [`AdaptOutcome::Accepted`].
/// * An out-of-range proposal → [`AdaptOutcome::Clamped`] (under
///   [`EnvelopeMode::ClampToBound`]) or [`AdaptOutcome::Rejected`] (under
///   [`EnvelopeMode::RejectOutside`]).
///
/// `current` is accepted for signature symmetry with a live parameter store; the
/// returned value is what the parameter should become. Pure and deterministic.
pub fn apply_adaptation(
    _current: i64,
    proposed: i64,
    env: &Envelope,
    mode: EnvelopeMode,
) -> AdaptOutcome {
    match envelope_verdict(proposed, env) {
        AdaptVerdict::InvalidEnvelope => AdaptOutcome::Rejected,
        AdaptVerdict::InRange => AdaptOutcome::Accepted(proposed),
        AdaptVerdict::BelowMin | AdaptVerdict::AboveMax => match mode {
            EnvelopeMode::ClampToBound => AdaptOutcome::Clamped(clamp_to_envelope(proposed, env)),
            EnvelopeMode::RejectOutside => AdaptOutcome::Rejected,
        },
    }
}
