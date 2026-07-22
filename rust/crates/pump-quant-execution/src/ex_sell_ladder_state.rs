//! Leaf `ex_sell_ladder_state`: the 5-level sell escalation state machine.
//!
//! Ported faithfully from the legacy `momentum/sell_engine.rs` `process_sell`
//! outcome-application logic and its `ESCALATION_LADDER` constant.
//!
//! ## Responsibility
//! Given the current ladder state and the outcome of the most recent sell
//! attempt, decide the *next* ladder state: which escalation level is now
//! active, whether the sell has completed, and whether all levels have been
//! exhausted (token stuck).
//!
//! ## Legacy fidelity
//! The original engine kept mutable `PendingSell` fields (`current_attempt`,
//! `first_attempt_ms`, `last_attempt_ms`, `queued_at_ms`) and mutated them in
//! `process_sell` based on the `SellAttemptResult`. That mutation is reproduced
//! here as a pure `LadderState -> LadderState` transition so it is deterministic
//! and testable in isolation (constitution §22: no wall-clock; time is an input).
//!
//! ## Constitution refs
//! - §22: integer-only; all durations are `u64` milliseconds.
//! - Overflow: attempt counters use `saturating_add`; time uses `saturating_*`.

/// Strategy selected for each escalation level. Controls the TX parameters the
/// caller would build. Mirrors the legacy `SellStrategy` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SellStrategy {
    /// Default RPC submit path with baseline parameters.
    RpcDefault,
    /// Same path but with an elevated compute-unit price (higher priority fee).
    RpcHighPriority,
    /// Rebuild the TX with a much wider slippage tolerance.
    RpcMaxSlippage,
    /// Nuclear option: `min_sol_out = 0`, max priority fee — exit at any cost.
    ForceMarketSell,
}

/// Immutable configuration for a single escalation level. Mirrors the legacy
/// `SellEscalation` struct, with the `timeout` `Duration` replaced by an integer
/// `timeout_ms` (constitution §22: no non-integer time in the logic path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SellEscalation {
    /// 0-indexed level number.
    pub attempt: u8,
    /// Strategy to use at this level.
    pub strategy: SellStrategy,
    /// Maximum acceptable slippage in basis points.
    pub max_slippage_bps: u16,
    /// Additional priority fee in lamports (on top of the base tip).
    pub extra_priority_lamports: u64,
    /// Compute-unit price override (micro-lamports per CU).
    pub compute_unit_price: u64,
    /// How long to wait for confirmation before escalating, in milliseconds.
    pub timeout_ms: u64,
}

/// The 5-level escalation ladder, ported verbatim from the legacy constant.
///
/// Each level is strictly more aggressive than the previous one (wider
/// slippage, higher priority fee), culminating in a forced market sell.
pub const ESCALATION_LADDER: [SellEscalation; 5] = [
    SellEscalation {
        attempt: 0,
        strategy: SellStrategy::RpcDefault,
        max_slippage_bps: 300,
        extra_priority_lamports: 0,
        compute_unit_price: 5_000,
        timeout_ms: 3_000,
    },
    SellEscalation {
        attempt: 1,
        strategy: SellStrategy::RpcDefault,
        max_slippage_bps: 800,
        extra_priority_lamports: 10_000,
        compute_unit_price: 8_000,
        timeout_ms: 3_000,
    },
    SellEscalation {
        attempt: 2,
        strategy: SellStrategy::RpcHighPriority,
        max_slippage_bps: 1_500,
        extra_priority_lamports: 50_000,
        compute_unit_price: 15_000,
        timeout_ms: 5_000,
    },
    SellEscalation {
        attempt: 3,
        strategy: SellStrategy::RpcMaxSlippage,
        max_slippage_bps: 5_000,
        extra_priority_lamports: 100_000,
        compute_unit_price: 30_000,
        timeout_ms: 5_000,
    },
    SellEscalation {
        attempt: 4,
        strategy: SellStrategy::ForceMarketSell,
        max_slippage_bps: 9_900,
        extra_priority_lamports: 200_000,
        compute_unit_price: 100_000,
        timeout_ms: 10_000,
    },
];

/// Effective cooldown (ms) added to `last_attempt_ms` once all levels are
/// exhausted, so the engine keeps retrying the max level slowly instead of
/// hammering RPC. Ported from the legacy `sell.last_attempt_ms = now + 25_000`.
pub const EXHAUSTED_COOLDOWN_MS: u64 = 25_000;

/// Number of levels in the ladder.
pub const LADDER_LEN: u8 = ESCALATION_LADDER.len() as u8;

/// Outcome of the most recent sell attempt, mirroring the legacy
/// `SellAttemptResult` plus the on-chain "balance is zero" short-circuit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SellOutcome {
    /// No attempt has been made yet (fresh queue entry).
    Pending,
    /// TX confirmed on-chain — the sell succeeded.
    Confirmed,
    /// TX submitted but not yet confirmed; it may still land. Do not escalate.
    MaybeConfirmed,
    /// TX definitively failed (build error, RPC error, instruction error).
    Failed,
    /// Circuit breaker was open — the sell was not attempted. Do not escalate.
    CircuitOpen {
        /// Milliseconds remaining before the breaker cooldown expires.
        remaining_ms: u64,
    },
    /// On-chain balance is zero — the token was already sold elsewhere.
    BalanceZero,
}

/// Lifecycle phase of a pending sell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderPhase {
    /// Actively escalating; the caller should attempt at `LadderState::level`.
    Active,
    /// Completed successfully (confirmed sell or zero on-chain balance).
    Completed,
    /// All escalation levels exhausted; retrying the max level with a cooldown.
    Exhausted,
}

/// Full state of a pending sell as it moves through the ladder. This is the
/// deterministic distillation of the legacy mutable `PendingSell` fields that
/// `process_sell` updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LadderState {
    /// Current escalation level (0-indexed, clamped to `LADDER_LEN - 1`).
    pub level: u8,
    /// Lifecycle phase.
    pub phase: LadderPhase,
    /// Epoch-ms of the most recent attempt (or the next-eligible time when a
    /// cooldown has been applied). `0` means "never attempted".
    pub last_attempt_ms: u64,
    /// Epoch-ms of the first attempt (`0` until the first failure escalates).
    pub first_attempt_ms: u64,
    /// Epoch-ms when the sell was first queued.
    pub queued_at_ms: u64,
}

impl LadderState {
    /// Construct a fresh state for a newly queued sell at level 0.
    pub fn new(queued_at_ms: u64) -> Self {
        Self {
            level: 0,
            phase: LadderPhase::Active,
            last_attempt_ms: 0,
            first_attempt_ms: 0,
            queued_at_ms,
        }
    }

    /// The escalation parameters for the current (clamped) level.
    pub fn escalation(&self) -> &'static SellEscalation {
        &ESCALATION_LADDER[current_index(self.level)]
    }
}

/// Context for a single transition: the current wall-clock (supplied by the
/// caller for determinism) and the outcome being applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LadderCtx {
    /// Current epoch time in milliseconds (caller-supplied; §22 determinism).
    pub now_ms: u64,
    /// Outcome of the most recent attempt.
    pub outcome: SellOutcome,
}

/// Clamp a (possibly over-incremented) level to a valid ladder index.
#[inline]
fn current_index(level: u8) -> usize {
    (level as usize).min(ESCALATION_LADDER.len() - 1)
}

/// Apply one attempt outcome to the ladder state and return the next state.
///
/// Faithful port of the legacy `process_sell` match arms:
/// - `Confirmed` / `BalanceZero` → `Completed` (level preserved).
/// - `MaybeConfirmed` → stay at the same level, refresh `last_attempt_ms`
///   (the TX might still land; do not burn an escalation level).
/// - `Failed` → advance one level, set `first_attempt_ms` on the first
///   escalation, and refresh `last_attempt_ms`. If advancing runs past the last
///   level, mark `Exhausted`, clamp the level to the maximum, and push
///   `last_attempt_ms` forward by [`EXHAUSTED_COOLDOWN_MS`].
/// - `CircuitOpen { remaining_ms }` → do not escalate; delay the next attempt
///   until the breaker cooldown elapses (`last_attempt_ms = now + remaining_ms`).
/// - `Pending` → treat as "attempt now": keep level 0, stamp `last_attempt_ms`.
pub fn sell_ladder_next(cur: LadderState, ctx: LadderCtx) -> LadderState {
    let now = ctx.now_ms;
    let mut next = cur;

    match ctx.outcome {
        SellOutcome::Confirmed | SellOutcome::BalanceZero => {
            next.phase = LadderPhase::Completed;
        }
        SellOutcome::Pending => {
            // First attempt is being made now; record the timestamp.
            next.phase = LadderPhase::Active;
            next.last_attempt_ms = now;
        }
        SellOutcome::MaybeConfirmed => {
            // Do not escalate — the TX may still land. Re-check next tick.
            next.phase = LadderPhase::Active;
            next.last_attempt_ms = now;
        }
        SellOutcome::CircuitOpen { remaining_ms } => {
            // RPC is overloaded; retry the same level after the cooldown.
            next.phase = LadderPhase::Active;
            next.last_attempt_ms = now.saturating_add(remaining_ms);
        }
        SellOutcome::Failed => {
            let advanced = cur.level.saturating_add(1);
            next.last_attempt_ms = now;
            if next.first_attempt_ms == 0 {
                next.first_attempt_ms = now;
            }

            if advanced >= LADDER_LEN {
                // Exhausted: clamp to the last level and apply the cooldown so
                // the engine keeps retrying the forced market sell slowly.
                next.level = LADDER_LEN - 1;
                next.phase = LadderPhase::Exhausted;
                next.last_attempt_ms = now.saturating_add(EXHAUSTED_COOLDOWN_MS);
            } else {
                next.level = advanced;
                next.phase = LadderPhase::Active;
            }
        }
    }

    next
}
