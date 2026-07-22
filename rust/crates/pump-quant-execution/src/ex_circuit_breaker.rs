//! Leaf `ex_circuit_breaker`: RPC circuit-breaker backoff state machine.
//!
//! Ported from `momentum/rpc_sender.rs`. The legacy breaker counted consecutive
//! send failures; once they reached `circuit_breaker_threshold` it tripped
//! `Open { since }` and subsequent sends were skipped until
//! `circuit_breaker_cooldown_ms` had elapsed, at which point it reset to
//! `Closed` with the failure counter zeroed. A single success also reset it.
//! Critically (per the legacy comments), the breaker controls **backoff only**
//! — it never routes to Jito.
//!
//! ## Responsibility
//! Advance the breaker state given the outcome of a send (or a timer tick),
//! deterministically. Wall-clock time is supplied by the caller as `now_ms`
//! (constitution §22: no clock reads in the logic path).
//!
//! ## Constitution refs
//! - §22: integer counters and millisecond timestamps only.
//! - Overflow: the failure counter uses `saturating_add`; elapsed time uses
//!   `saturating_sub`.

/// Which half of the breaker cycle we are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerPhase {
    /// RPC working normally; sends are allowed.
    Closed,
    /// Tripped; sends are skipped until the cooldown elapses.
    Open,
}

/// Full breaker state. The threshold and cooldown are carried in the state so
/// the transition can remain a pure two-argument function (the fixed
/// `breaker_next(state, outcome)` signature) while staying self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerState {
    /// Current phase.
    pub phase: BreakerPhase,
    /// Consecutive failure count (only meaningful while `Closed`).
    pub consecutive_failures: u32,
    /// Epoch-ms at which the breaker last tripped `Open`.
    pub opened_at_ms: u64,
    /// Consecutive failures required to trip. Legacy default: 3.
    pub threshold: u32,
    /// Cooldown before an `Open` breaker resets to `Closed`, in ms.
    /// Legacy default: 15_000.
    pub cooldown_ms: u64,
}

impl BreakerState {
    /// Create a fresh `Closed` breaker with the given threshold and cooldown.
    pub fn new(threshold: u32, cooldown_ms: u64) -> Self {
        Self {
            phase: BreakerPhase::Closed,
            consecutive_failures: 0,
            opened_at_ms: 0,
            threshold,
            cooldown_ms,
        }
    }

    /// Whether a send is currently allowed (breaker not in active cooldown).
    ///
    /// `Closed` always allows; `Open` allows only once the cooldown has elapsed
    /// (`now_ms - opened_at_ms >= cooldown_ms`), mirroring the legacy pre-flight
    /// check that resets an expired breaker before sending.
    pub fn allows_send(&self, now_ms: u64) -> bool {
        match self.phase {
            BreakerPhase::Closed => true,
            BreakerPhase::Open => now_ms.saturating_sub(self.opened_at_ms) >= self.cooldown_ms,
        }
    }

    /// Milliseconds remaining in the current cooldown, or `0` if closed/expired.
    pub fn remaining_ms(&self, now_ms: u64) -> u64 {
        match self.phase {
            BreakerPhase::Closed => 0,
            BreakerPhase::Open => self
                .cooldown_ms
                .saturating_sub(now_ms.saturating_sub(self.opened_at_ms)),
        }
    }
}

/// Outcome fed to the breaker. Each variant carries the caller's current
/// timestamp so the transition never reads a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// A send confirmed on-chain.
    Success {
        /// Current epoch time in milliseconds.
        now_ms: u64,
    },
    /// A send failed, timed out, or confirmed with an instruction error.
    Failure {
        /// Current epoch time in milliseconds.
        now_ms: u64,
    },
    /// A no-op tick used purely to let an `Open` breaker's cooldown expire.
    Tick {
        /// Current epoch time in milliseconds.
        now_ms: u64,
    },
}

impl SendOutcome {
    /// The timestamp carried by this outcome.
    #[inline]
    fn now_ms(&self) -> u64 {
        match self {
            SendOutcome::Success { now_ms }
            | SendOutcome::Failure { now_ms }
            | SendOutcome::Tick { now_ms } => *now_ms,
        }
    }
}

/// Advance the breaker one step.
///
/// Faithful port of the legacy reset/trip logic:
/// 1. If `Open` and the cooldown has **not** elapsed, the send would have been
///    skipped — the state is returned unchanged (aside from the incoming
///    outcome, which does not apply during cooldown).
/// 2. If `Open` and the cooldown **has** elapsed, reset to `Closed` with the
///    failure counter zeroed, then apply the incoming outcome as if closed.
/// 3. While `Closed`:
///    - `Success` ⇒ reset the failure counter to `0`.
///    - `Failure` ⇒ increment; on reaching `threshold`, trip `Open` and stamp
///      `opened_at_ms`.
///    - `Tick` ⇒ no change.
pub fn breaker_next(state: BreakerState, outcome: SendOutcome) -> BreakerState {
    let now = outcome.now_ms();
    let mut next = state;

    // Handle an open breaker: either stay open (cooldown active) or reset.
    if next.phase == BreakerPhase::Open {
        if now.saturating_sub(next.opened_at_ms) < next.cooldown_ms {
            // Still cooling down — sends are skipped, outcome does not apply.
            return next;
        }
        // Cooldown complete — reset and fall through to apply the outcome.
        next.phase = BreakerPhase::Closed;
        next.consecutive_failures = 0;
    }

    match outcome {
        SendOutcome::Success { .. } => {
            next.consecutive_failures = 0;
            next.phase = BreakerPhase::Closed;
        }
        SendOutcome::Failure { .. } => {
            next.consecutive_failures = next.consecutive_failures.saturating_add(1);
            if next.consecutive_failures >= next.threshold {
                next.phase = BreakerPhase::Open;
                next.opened_at_ms = now;
            }
        }
        SendOutcome::Tick { .. } => {}
    }

    next
}
