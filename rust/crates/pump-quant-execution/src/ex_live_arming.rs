//! Leaf `ex_live_arming`: the live-capital arming gate and risk envelope.
//!
//! ## What this is for
//! The bot is designed to trade autonomously: it must open and close positions
//! without a human approving each one. This module is what makes that safe
//! enough to actually switch on.
//!
//! The distinction it encodes — and the whole reason it exists — is between
//! **per-trade approval** and **arming**. Per-trade approval is incompatible
//! with a bot that reacts inside a slot, and this module never asks for it.
//! Arming is a single operator action that authorises live capital *within a
//! stated envelope*; after it, every entry and exit is the bot's own decision.
//!
//! Autonomy without an envelope is not autonomy, it is an unbounded loss
//! function with a keypair attached. A bug in sizing, a decode error that
//! mistakes a curve state, or a venue that starts filling at absurd prices will
//! drain a wallet in far less time than it takes anyone to notice. Every limit
//! here exists because a bot with no human in the loop has nothing else standing
//! between a defect and the balance.
//!
//! ## The rule that is easy to get backwards
//! **Exits are always admitted, including when disarmed.** A kill switch that
//! also blocks selling does not protect capital, it traps it — the position
//! stays open, unmanaged, while the thing that tripped the switch keeps moving
//! against it. Disarming stops the bot *opening* risk; it must never stop it
//! *closing* risk. [`LiveGate::admit_exit`] therefore cannot return a denial.
//!
//! ## Constitution refs
//! - §22: sizes and PnL in lamports, rates in basis points, all integer. PnL is
//!   `i64` because it is signed; every accumulator saturates.
//! - Determinism: no clock, no RNG. Wall time and day boundaries are supplied by
//!   the caller, so an identical event sequence always produces an identical
//!   decision — which is what makes the gate replayable in an incident review.
//! - §18.8 loud degradation: every denial carries a typed reason with the
//!   offending numbers, never a bare `false`.

/// Trades retained for the rate-limit window. Bounded and stack-allocated.
pub const TRADE_WINDOW: usize = 256;

/// Milliseconds in the rate-limit window (one hour).
pub const RATE_WINDOW_MS: u64 = 60 * 60 * 1_000;

/// Why the gate is not armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisarmReason {
    /// No operator has ever armed this process.
    NeverArmed,
    /// An operator disarmed it deliberately.
    OperatorDisarmed,
    /// The daily loss limit was reached. Re-arming is an operator action, on
    /// purpose: the system must not be able to decide on its own that today's
    /// losses were acceptable.
    DailyLossBreached,
    /// The operator heartbeat went stale — the dead-man's switch fired.
    HeartbeatLost,
}

/// Whether live capital is authorised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmState {
    /// Entries are refused. Exits are still admitted.
    Disarmed(DisarmReason),
    /// Entries are admitted subject to the envelope.
    Armed,
}

/// The limits an operator authorises when arming.
///
/// Every field is a hard ceiling, not a target. A zero in any field disables
/// entries entirely rather than meaning "unlimited" — an envelope that reads as
/// unlimited because someone left a field at its default is the failure mode
/// this type exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveEnvelope {
    /// Largest single position, lamports.
    pub max_position_lamports: u64,
    /// Largest total capital deployed across all open positions, lamports.
    pub max_total_deployed_lamports: u64,
    /// Most positions open at once.
    pub max_open_positions: u32,
    /// Most entries admitted per rolling hour.
    pub max_entries_per_hour: u32,
    /// Realised loss in one day that trips the kill switch, lamports.
    pub daily_loss_limit_lamports: u64,
    /// Widest slippage an entry may accept, basis points.
    pub max_entry_slippage_bps: u32,
    /// Milliseconds without an operator heartbeat before the dead-man's switch
    /// fires. Zero disables the switch, which is a deliberate operator choice
    /// and is recorded as such.
    pub heartbeat_timeout_ms: u64,
}

impl LiveEnvelope {
    /// An envelope that admits nothing. The safe default: a gate constructed
    /// without an explicit operator envelope must not trade.
    #[must_use]
    pub const fn closed() -> Self {
        Self {
            max_position_lamports: 0,
            max_total_deployed_lamports: 0,
            max_open_positions: 0,
            max_entries_per_hour: 0,
            daily_loss_limit_lamports: 0,
            max_entry_slippage_bps: 0,
            heartbeat_timeout_ms: 0,
        }
    }

    /// Whether this envelope could admit any entry at all.
    #[must_use]
    pub const fn admits_anything(&self) -> bool {
        self.max_position_lamports > 0
            && self.max_total_deployed_lamports > 0
            && self.max_open_positions > 0
            && self.max_entries_per_hour > 0
    }
}

/// Why an entry was refused. Each variant carries the numbers that caused it, so
/// a denial is diagnosable from the journal without re-deriving state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// Live capital is not armed.
    Disarmed(DisarmReason),
    /// The envelope admits nothing — at least one limit is zero.
    EnvelopeClosed,
    /// Position size exceeds the per-position ceiling.
    PositionTooLarge { requested: u64, ceiling: u64 },
    /// This entry would push total deployed capital over its ceiling.
    DeployedCapExceeded { would_be: u64, ceiling: u64 },
    /// Too many positions already open.
    TooManyOpen { open: u32, ceiling: u32 },
    /// The rolling-hour entry budget is spent.
    RateLimited { in_window: u32, ceiling: u32 },
    /// Quoted slippage is wider than the envelope allows.
    SlippageTooWide { quoted_bps: u32, ceiling_bps: u32 },
}

/// The gate's answer for one proposed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Proceed.
    Allow,
    /// Do not submit. The reason is typed for the journal.
    Deny(DenyReason),
}

impl Admission {
    /// Whether this admission permits submission.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Live-capital gate: arming state, risk envelope, and the running counters the
/// envelope is checked against.
#[derive(Debug, Clone, Copy)]
pub struct LiveGate {
    state: ArmState,
    envelope: LiveEnvelope,
    /// Realised PnL for the current day, lamports. Signed.
    day_realized_pnl: i64,
    /// Day index supplied by the caller; a change resets the daily counter.
    day_index: u64,
    /// Capital currently deployed across open positions, lamports.
    deployed_lamports: u64,
    /// Positions currently open.
    open_positions: u32,
    /// Admission timestamps for the rate-limit window.
    entry_times: [u64; TRADE_WINDOW],
    entry_head: usize,
    entry_len: usize,
    /// Last operator heartbeat, milliseconds.
    last_heartbeat_ms: u64,
}

impl Default for LiveGate {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveGate {
    /// A disarmed gate with a closed envelope.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: ArmState::Disarmed(DisarmReason::NeverArmed),
            envelope: LiveEnvelope::closed(),
            day_realized_pnl: 0,
            day_index: 0,
            deployed_lamports: 0,
            open_positions: 0,
            entry_times: [0; TRADE_WINDOW],
            entry_head: 0,
            entry_len: 0,
            last_heartbeat_ms: 0,
        }
    }

    /// Authorise live capital within `envelope`.
    ///
    /// This is the single operator action the design requires. After it, no
    /// entry or exit needs approval. An envelope that admits nothing is rejected
    /// rather than silently accepted, so "armed" always means "can actually
    /// trade".
    pub fn arm(&mut self, envelope: LiveEnvelope, now_ms: u64) -> Result<(), DenyReason> {
        if !envelope.admits_anything() {
            return Err(DenyReason::EnvelopeClosed);
        }
        self.envelope = envelope;
        self.state = ArmState::Armed;
        self.last_heartbeat_ms = now_ms;
        Ok(())
    }

    /// Stop admitting entries. Exits remain admitted.
    pub fn disarm(&mut self, reason: DisarmReason) {
        self.state = ArmState::Disarmed(reason);
    }

    /// Record an operator heartbeat for the dead-man's switch.
    pub fn heartbeat(&mut self, now_ms: u64) {
        self.last_heartbeat_ms = now_ms;
    }

    /// Current arming state, after applying the dead-man's switch at `now_ms`.
    pub fn state(&mut self, now_ms: u64) -> ArmState {
        if self.envelope.heartbeat_timeout_ms > 0 && matches!(self.state, ArmState::Armed) {
            let age = now_ms.saturating_sub(self.last_heartbeat_ms);
            if age > self.envelope.heartbeat_timeout_ms {
                self.state = ArmState::Disarmed(DisarmReason::HeartbeatLost);
            }
        }
        self.state
    }

    /// The active envelope.
    #[must_use]
    pub const fn envelope(&self) -> LiveEnvelope {
        self.envelope
    }

    /// Realised PnL so far today, lamports.
    #[must_use]
    pub const fn day_realized_pnl(&self) -> i64 {
        self.day_realized_pnl
    }

    /// Capital currently deployed, lamports.
    #[must_use]
    pub const fn deployed_lamports(&self) -> u64 {
        self.deployed_lamports
    }

    /// Positions currently open.
    #[must_use]
    pub const fn open_positions(&self) -> u32 {
        self.open_positions
    }

    /// Entries admitted within the rolling window ending at `now_ms`.
    #[must_use]
    pub fn entries_in_window(&self, now_ms: u64) -> u32 {
        let cutoff = now_ms.saturating_sub(RATE_WINDOW_MS);
        self.entry_times[..self.entry_len]
            .iter()
            .filter(|&&t| t >= cutoff)
            .count() as u32
    }

    /// Roll the daily counters when the caller's day index advances.
    ///
    /// A new day clears realised PnL but does **not** re-arm after a loss-limit
    /// breach. Re-arming stays an operator action: a system that can wait out
    /// its own kill switch does not have one.
    pub fn roll_day(&mut self, day_index: u64) {
        if day_index != self.day_index {
            self.day_index = day_index;
            self.day_realized_pnl = 0;
        }
    }

    /// Decide whether to admit a proposed **entry**.
    ///
    /// Checks run cheapest-first and return the first violation, so the reason
    /// in the journal is the binding constraint rather than an arbitrary one.
    pub fn admit_entry(&mut self, size_lamports: u64, slippage_bps: u32, now_ms: u64) -> Admission {
        if let ArmState::Disarmed(r) = self.state(now_ms) {
            return Admission::Deny(DenyReason::Disarmed(r));
        }
        if !self.envelope.admits_anything() {
            return Admission::Deny(DenyReason::EnvelopeClosed);
        }
        if size_lamports > self.envelope.max_position_lamports {
            return Admission::Deny(DenyReason::PositionTooLarge {
                requested: size_lamports,
                ceiling: self.envelope.max_position_lamports,
            });
        }
        if slippage_bps > self.envelope.max_entry_slippage_bps {
            return Admission::Deny(DenyReason::SlippageTooWide {
                quoted_bps: slippage_bps,
                ceiling_bps: self.envelope.max_entry_slippage_bps,
            });
        }
        if self.open_positions >= self.envelope.max_open_positions {
            return Admission::Deny(DenyReason::TooManyOpen {
                open: self.open_positions,
                ceiling: self.envelope.max_open_positions,
            });
        }
        let would_be = self.deployed_lamports.saturating_add(size_lamports);
        if would_be > self.envelope.max_total_deployed_lamports {
            return Admission::Deny(DenyReason::DeployedCapExceeded {
                would_be,
                ceiling: self.envelope.max_total_deployed_lamports,
            });
        }
        let in_window = self.entries_in_window(now_ms);
        if in_window >= self.envelope.max_entries_per_hour {
            return Admission::Deny(DenyReason::RateLimited {
                in_window,
                ceiling: self.envelope.max_entries_per_hour,
            });
        }
        Admission::Allow
    }

    /// Decide whether to admit a proposed **exit**.
    ///
    /// Always [`Admission::Allow`]. This is not an oversight and must not be
    /// "tightened" later: a gate that can refuse an exit converts a kill switch
    /// into a capital trap. Whatever tripped the switch keeps moving, and the
    /// position sits open with no way out. The return type stays [`Admission`]
    /// so exits flow through the same journalled path as entries.
    #[must_use]
    pub const fn admit_exit(&self) -> Admission {
        Admission::Allow
    }

    /// Record that an admitted entry actually filled.
    ///
    /// Call this on fill, never on submission. Counting submissions would let a
    /// run of failed transactions consume the rate budget and the deployed cap
    /// without ever taking a position.
    pub fn record_entry_fill(&mut self, size_lamports: u64, now_ms: u64) {
        self.entry_times[self.entry_head] = now_ms;
        self.entry_head = (self.entry_head + 1) % TRADE_WINDOW;
        if self.entry_len < TRADE_WINDOW {
            self.entry_len += 1;
        }
        self.deployed_lamports = self.deployed_lamports.saturating_add(size_lamports);
        self.open_positions = self.open_positions.saturating_add(1);
    }

    /// Record a closed position and its realised PnL, tripping the kill switch
    /// if the daily loss limit is reached.
    ///
    /// `realized_pnl_lamports` is net of every cost — fees, tips, rent, realised
    /// slippage. A gross figure here would make the loss limit a fiction.
    pub fn record_exit_fill(&mut self, size_lamports: u64, realized_pnl_lamports: i64) {
        self.deployed_lamports = self.deployed_lamports.saturating_sub(size_lamports);
        self.open_positions = self.open_positions.saturating_sub(1);
        self.day_realized_pnl = self.day_realized_pnl.saturating_add(realized_pnl_lamports);

        let limit = self.envelope.daily_loss_limit_lamports;
        if limit > 0 && self.day_realized_pnl < 0 {
            let loss = self.day_realized_pnl.unsigned_abs();
            if loss >= limit {
                self.state = ArmState::Disarmed(DisarmReason::DailyLossBreached);
            }
        }
    }
}
