//! Leaf `ex_sell_ladder_escalate`: second-scale deterministic escalation trigger.
//!
//! Ported from the legacy `sell_engine.rs` `run()` loop, which decided whether a
//! pending sell was due for escalation by comparing the time elapsed since the
//! last attempt against the current level's `timeout`:
//! `now.saturating_sub(last_attempt_ms) >= escalation.timeout`.
//!
//! ## Responsibility
//! A pure predicate: given the current level, the time elapsed since the last
//! attempt, how much of the order is still unfilled, and the per-level
//! thresholds, decide whether to escalate to the next level.
//!
//! ## Constitution refs
//! - §22: integer-only. Elapsed time is `u64` ms; unfilled fraction is `bps`.
//! - Deterministic: a pure function of its inputs, no clock read.

/// Per-level thresholds controlling the escalation trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LadderThresholds {
    /// Confirmation timeout for each of the 5 levels, in milliseconds. Once the
    /// elapsed time at `level` reaches `level_timeout_ms[level]`, the attempt is
    /// considered timed out and escalation is due. Mirrors the legacy
    /// `ESCALATION_LADDER[i].timeout`.
    pub level_timeout_ms: [u64; 5],
    /// Minimum still-unfilled fraction (basis points) that justifies escalating.
    /// If the order is already essentially filled (`unfilled_bps` below this),
    /// there is nothing left worth escalating for.
    pub min_unfilled_bps: u32,
}

impl Default for LadderThresholds {
    /// Defaults taken from the legacy ladder timeouts (3s, 3s, 5s, 5s, 10s) with
    /// a 100 bps (1%) minimum unfilled floor.
    fn default() -> Self {
        Self {
            level_timeout_ms: [3_000, 3_000, 5_000, 5_000, 10_000],
            min_unfilled_bps: 100,
        }
    }
}

/// Last valid level index (0-indexed) in the 5-level ladder.
pub const MAX_LEVEL: u8 = 4;

/// Decide whether to escalate to the next ladder level.
///
/// Returns `true` iff all of:
/// 1. The current `level` is below [`MAX_LEVEL`] (there is a higher level to go
///    to — at the top level the engine retries with a cooldown rather than
///    escalating, per `ex_sell_ladder_state`).
/// 2. The elapsed time since the last attempt has reached the current level's
///    timeout (`elapsed_ms >= level_timeout_ms[level]`).
/// 3. The order is still meaningfully unfilled (`unfilled_bps >= min_unfilled_bps`).
///
/// The `level` argument is clamped so an out-of-range value cannot panic.
pub fn should_escalate(
    level: u8,
    elapsed_ms: u64,
    unfilled_bps: u32,
    thresholds: &LadderThresholds,
) -> bool {
    if level >= MAX_LEVEL {
        return false;
    }
    let idx = (level as usize).min(thresholds.level_timeout_ms.len() - 1);
    let timed_out = elapsed_ms >= thresholds.level_timeout_ms[idx];
    let still_unfilled = unfilled_bps >= thresholds.min_unfilled_bps;
    timed_out && still_unfilled
}
