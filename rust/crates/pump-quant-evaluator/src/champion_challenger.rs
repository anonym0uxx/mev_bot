//! `champion_challenger` — champion/challenger comparison verdict primitive
//! (constitution §35, §51).
//!
//! Responsibility: the frozen, hash-pinned evaluator must be able to render the
//! pass/fail promotion verdict itself (constitution §51 — the evaluator verifies
//! before any result is accepted), rather than trusting an orchestrator's claim.
//! This is the deterministic net-SOL margin comparison: a challenger policy
//! defeats the reigning champion only if its reconciled net SOL exceeds the
//! champion's by at least a required margin. Promotion *orchestration* stays in
//! the supervisor; the verdict is an evaluator leaf.
//!
//! Integer-only (constitution §22): reuses [`NetSol`] lamport aggregates from
//! `evaluator_stats`; no floats.

use crate::evaluator_stats::NetSol;

/// Verdict of a challenger-vs-champion comparison (constitution §35).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChampionVerdict {
    /// Challenger beat the champion by at least the required margin.
    Defeats,
    /// Challenger did not clear the margin.
    Fails {
        /// Challenger net minus champion net (lamports); may be negative.
        margin_lamports: i128,
        /// The margin that was required.
        required_lamports: i128,
    },
    /// Challenger has no reconciled evidence — nothing can be promoted on it.
    NoEvidence,
}

impl ChampionVerdict {
    /// True iff the challenger defeats the champion.
    pub fn defeats(&self) -> bool {
        matches!(self, ChampionVerdict::Defeats)
    }
}

/// Does the challenger defeat the champion by the required net-SOL margin?
///
/// Responsibility (constitution §35, §51): a deterministic verdict the
/// hash-pinned evaluator can run. Rules:
///
/// * A challenger with no reconciled trades ([`NetSol::is_missing`]) yields
///   [`ChampionVerdict::NoEvidence`] — apparent activity is never evidence.
/// * A missing champion is treated as a zero-net incumbent (the empty book), so
///   a challenger must still clear `required_margin` above break-even.
/// * Otherwise [`ChampionVerdict::Defeats`] iff
///   `challenger.net − champion.net ≥ required_margin`, else
///   [`ChampionVerdict::Fails`] carrying the shortfall.
///
/// `required_margin` is a non-negative lamport figure by contract. The
/// subtraction is checked because these are reconciled lamport books whose
/// `i128` headroom is astronomically larger than any real balance; an overflow
/// signals corrupt input, not normal operation.
pub fn challenger_defeats_champion(
    champion: &NetSol,
    challenger: &NetSol,
    required_margin: i128,
) -> ChampionVerdict {
    if challenger.is_missing() {
        return ChampionVerdict::NoEvidence;
    }
    let champ_net = if champion.is_missing() {
        0
    } else {
        champion.net_lamports
    };
    let margin = challenger
        .net_lamports
        .checked_sub(champ_net)
        .expect("challenger_defeats_champion: margin i128 overflow");
    if margin >= required_margin {
        ChampionVerdict::Defeats
    } else {
        ChampionVerdict::Fails {
            margin_lamports: margin,
            required_lamports: required_margin,
        }
    }
}
