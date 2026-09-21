//! Decision freshness — binding a verdict to the clock it was made against.
//!
//! THE GAP THIS CLOSES. A decision is rendered from a snapshot taken at `t_dec_ms` and resolved
//! some time later. Nothing carried that gap: the request had no clock at all, so a verdict
//! computed against a two-second-old flow state was indistinguishable from one computed
//! against a ten-second-old one, and the engine had no basis to refuse the late one. On a
//! memecoin tape where the prompt's own windows are seconds-fresh, that is a real loss channel —
//! and it is the channel that gets WIDER once the trading brain sits behind a network hop,
//! which is exactly why this is built before the brain is wired in rather than after.
//!
//! AUTHORITY. This is a VETO, not a judgement: it never substitutes a decision, it refuses to
//! apply one that arrived too late (and the caller then has no trade rather than a stale one).
//! Refusing is a capital-safety act; inventing a replacement verdict would be inference control,
//! which this work retires.
//!
//! THE CLOCK IS SUPPLIED, NEVER SAMPLED. Both timestamps arrive as arguments. A clock read inside
//! the decision path would make paper and replay sessions non-deterministic and move the golden
//! digests; supplying them keeps the live path honest AND the offline paths byte-stable.

#![forbid(unsafe_code)]

/// The two ends of a decision's life: when its snapshot was taken, and when it is being applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionClock {
    /// The clock the prompt's snapshot was rendered at (the corpus's `t_dec_ms`).
    pub decided_at_ms: i64,
    /// The time the verdict is being resolved — a real clock on the live path, a replayed or
    /// scenario value offline.
    pub resolved_at_ms: i64,
}

impl DecisionClock {
    /// The age of a decision, in milliseconds, when the clock is coherent.
    #[must_use]
    pub const fn age_ms(self) -> Option<u64> {
        let gap = self.resolved_at_ms.saturating_sub(self.decided_at_ms);
        if gap < 0 {
            None
        } else {
            Some(gap as u64)
        }
    }
}

/// Why a decision could not be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalenessVeto {
    /// The clock ran BACKWARDS: the verdict claims to have been resolved before it was decided.
    /// That is an inconsistent clock, not a fast decision, and it fails closed — a negative age
    /// silently coerced to zero would make the freshest possible decision out of the most
    /// broken input.
    Backwards {
        /// The clock the snapshot was rendered at.
        decided_at_ms: i64,
        /// The inconsistent resolution time.
        resolved_at_ms: i64,
    },
    /// The decision arrived later than the limit allows.
    Stale {
        /// The measured age, milliseconds.
        age_ms: u64,
        /// The limit in force.
        max_ms: u64,
    },
}

impl StalenessVeto {
    /// Stable label for journals and dashboards — never reword.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StalenessVeto::Backwards { .. } => "decision_clock_backwards",
            StalenessVeto::Stale { .. } => "decision_stale",
        }
    }
}

/// The freshness limit the champion runs with, milliseconds.
///
/// **Derived, not taste.** The corpus's own `INVALIDATION` lines — the model's own statement of
/// when a position's premise is dead — are written as `exit if … last_trade_age_s > 3.0`. Three
/// seconds is the horizon the model itself treats as "this state is no longer the state I
/// reasoned about", so a verdict applied later than that is acting on evidence the brain would
/// already have called stale.
///
/// Published as a constant for the same reason the impact limit is: `Config` enters the
/// decision-journal seed through `strategy_identity`'s `Debug` hash, so a config field would move
/// the sealed golden digest as a side effect.
pub const CHAMPION_MAX_DECISION_AGE_MS: u64 = 3_000;

/// Check that a decision is fresh enough to apply, returning its age when it is.
pub fn check_freshness(clock: DecisionClock, max_age_ms: u64) -> Result<u64, StalenessVeto> {
    let Some(age_ms) = clock.age_ms() else {
        return Err(StalenessVeto::Backwards {
            decided_at_ms: clock.decided_at_ms,
            resolved_at_ms: clock.resolved_at_ms,
        });
    };
    if age_ms > max_age_ms {
        return Err(StalenessVeto::Stale {
            age_ms,
            max_ms: max_age_ms,
        });
    }
    Ok(age_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(decided: i64, resolved: i64) -> DecisionClock {
        DecisionClock {
            decided_at_ms: decided,
            resolved_at_ms: resolved,
        }
    }

    #[test]
    fn a_decision_inside_the_limit_reports_its_age() {
        assert_eq!(check_freshness(clock(1_000, 1_000), 3_000), Ok(0));
        assert_eq!(check_freshness(clock(1_000, 2_500), 3_000), Ok(1_500));
        // Exactly at the limit is inside it: the bound is inclusive, like every other veto here.
        assert_eq!(check_freshness(clock(1_000, 4_000), 3_000), Ok(3_000));
    }

    #[test]
    fn a_late_decision_is_vetoed_with_both_numbers() {
        let veto = check_freshness(clock(1_000, 4_001), 3_000).unwrap_err();
        assert_eq!(
            veto,
            StalenessVeto::Stale {
                age_ms: 3_001,
                max_ms: 3_000
            }
        );
        assert_eq!(veto.as_str(), "decision_stale");
    }

    #[test]
    fn a_backwards_clock_fails_closed_rather_than_reading_as_fresh() {
        let veto = check_freshness(clock(2_000, 1_000), 3_000).unwrap_err();
        assert_eq!(
            veto,
            StalenessVeto::Backwards {
                decided_at_ms: 2_000,
                resolved_at_ms: 1_000
            }
        );
        assert_eq!(veto.as_str(), "decision_clock_backwards");
        // And the age accessor must not saturate a negative gap into a fresh-looking zero.
        assert_eq!(clock(2_000, 1_000).age_ms(), None);
    }

    #[test]
    fn the_champion_limit_is_the_horizon_the_model_itself_uses() {
        // The corpus's invalidation lines read `last_trade_age_s > 3.0`; the limit is that same
        // three seconds, and a limit that did not bite inside a handful of slots would be
        // decoration on a tape whose flow windows are seconds wide.
        assert_eq!(CHAMPION_MAX_DECISION_AGE_MS, 3_000);
        assert!(check_freshness(
            clock(0, CHAMPION_MAX_DECISION_AGE_MS as i64 + 1),
            CHAMPION_MAX_DECISION_AGE_MS
        )
        .is_err());
    }
}
