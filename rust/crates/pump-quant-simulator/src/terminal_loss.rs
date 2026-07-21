//! Predeclared terminal-loss accounting.
//!
//! Responsibility: value a position that has become **terminally unexitable**
//! (constitution §38: "An unexitable position may never be valued at displayed
//! price; use predeclared terminal-loss rules"). The policy is declared *before*
//! the outcome is known and is applied to the position's cost basis — never to the
//! appreciated displayed mark — so the simulator cannot manufacture phantom value
//! from a market it can no longer sell into.

use crate::fixed::{mul_bps, BPS_ONE};

/// A predeclared rule for valuing an unexitable position.
///
/// The rule is chosen at experiment pre-registration time (§53) and frozen; it is
/// applied to the position's *basis* (SOL actually committed / effective entry
/// value), guaranteeing the recovered value is always `<= basis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalLossPolicy {
    /// The most conservative rule: an unexitable position is written to zero.
    WriteToZero,
    /// A predeclared recoverable residual, as a fraction of basis in basis points
    /// (clamped to `BPS_ONE`). Models a small salvage value where one is genuinely
    /// justified and pre-registered.
    ResidualBps(u32),
    /// A predeclared fixed lamport residual, capped at the basis so it can never
    /// exceed what was committed.
    FixedResidualLamports(u64),
}

impl TerminalLossPolicy {
    /// Terminal (recovered) value in lamports for a position with the given
    /// `basis_lamports`.
    ///
    /// Invariant guaranteed for every variant: the returned value is in
    /// `[0, basis_lamports]`. This is the property that forbids valuing an
    /// unexitable position at its displayed (appreciated) mark.
    #[must_use]
    pub fn terminal_value(&self, basis_lamports: u64) -> u64 {
        match *self {
            TerminalLossPolicy::WriteToZero => 0,
            TerminalLossPolicy::ResidualBps(bps) => mul_bps(basis_lamports, bps.min(BPS_ONE)),
            TerminalLossPolicy::FixedResidualLamports(fixed) => fixed.min(basis_lamports),
        }
    }
}
