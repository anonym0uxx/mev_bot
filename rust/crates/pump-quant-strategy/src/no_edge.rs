//! # no_edge — explicit No-Edge operating state + no-forced-entry guard (criterion 50)
//!
//! A small lane state machine with an explicit [`LaneState::NoEdge`] variant and a
//! guard ([`emit_entry`]) that can never produce an entry [`OrderIntent`] from
//! `NoEdge` (or from `Searching`). "Idle but searching" is a valid, non-failure
//! operating state — the null-hypothesis conclusion that a *tested thing* shows no
//! edge must never license a forced trade to "stay active" (constitution §2/§50,
//! anti-idle mandate).
//!
//! ## Constitution
//! §2 null hypothesis, §50 no-forced-entry. Pure, deterministic; no I/O.

/// Kind of order an intent represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentKind {
    /// Opening / adding entry order.
    Entry,
    /// Risk-reducing exit order.
    Exit,
}

/// A minimal order intent. Entries are only ever produced through [`emit_entry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderIntent {
    /// The candidate token mint.
    pub token_mint: u64,
    /// Intent kind.
    pub kind: IntentKind,
}

/// The lane's operating state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneState {
    /// Searching for a defensible edge — idle, not forced to trade.
    Searching,
    /// A validated edge is active; entries may be emitted.
    Active,
    /// Explicit no-edge verdict for the tested approach — never forces an entry.
    NoEdge,
    /// Lane retired (out of service).
    Retired,
}

/// Why the lane is idle instead of emitting an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleReason {
    /// Still searching — no defensible edge yet.
    Searching,
    /// Explicit no-edge verdict.
    NoEdge,
    /// Lane retired.
    Retired,
}

/// The result of an entry-emission attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryEmission {
    /// An entry intent was produced (only possible from [`LaneState::Active`]).
    Emitted(OrderIntent),
    /// The lane stayed idle for `IdleReason` — no forced trade.
    Idle(IdleReason),
}

/// Whether the lane may emit an entry at all.
///
/// Only [`LaneState::Active`] may; `NoEdge`, `Searching`, and `Retired` may not.
#[inline]
pub fn may_emit_entry(state: LaneState) -> bool {
    matches!(state, LaneState::Active)
}

/// The no-forced-entry guard (leaf **ne_guard**).
///
/// Emits an entry intent **only** from [`LaneState::Active`]. From `NoEdge`,
/// `Searching`, or `Retired` it returns [`EntryEmission::Idle`] with the matching
/// reason — there is no code path that constructs an entry `OrderIntent` in the
/// `NoEdge` state, so a forced trade is unrepresentable. Pure.
pub fn emit_entry(state: LaneState, token_mint: u64) -> EntryEmission {
    match state {
        LaneState::Active => EntryEmission::Emitted(OrderIntent {
            token_mint,
            kind: IntentKind::Entry,
        }),
        LaneState::Searching => EntryEmission::Idle(IdleReason::Searching),
        LaneState::NoEdge => EntryEmission::Idle(IdleReason::NoEdge),
        LaneState::Retired => EntryEmission::Idle(IdleReason::Retired),
    }
}

/// Advance the lane state on new edge evidence.
///
/// From `Searching` or `NoEdge`: finding a defensible edge → `Active`, otherwise
/// `NoEdge`. `Active` with a lost edge falls back to `NoEdge`. `Retired` is
/// terminal. Deterministic.
pub fn lane_on_evidence(state: LaneState, edge_found: bool) -> LaneState {
    match state {
        LaneState::Retired => LaneState::Retired,
        LaneState::Active => {
            if edge_found {
                LaneState::Active
            } else {
                LaneState::NoEdge
            }
        }
        LaneState::Searching | LaneState::NoEdge => {
            if edge_found {
                LaneState::Active
            } else {
                LaneState::NoEdge
            }
        }
    }
}
