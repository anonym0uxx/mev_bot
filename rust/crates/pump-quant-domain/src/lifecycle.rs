//! Candidate lifecycle state vocabulary.
//!
//! ## Responsibility
//! The stable enumeration of the states a discovery candidate passes through,
//! plus total, side-effect-free predicates over it (terminal? holds capital?
//! legal transition?). The candidate state machine itself lives downstream; this
//! crate only owns the shared *names* and their invariants so every crate agrees
//! on what "terminal" means.
//!
//! ## Constitution alignment
//! Section 23 — `CandidateLifecycleState` and the rule that every candidate,
//! including never-traded and rejected ones, remains queryable forever (so
//! terminal states are archival, not deletion).

use core::fmt;

/// The lifecycle state of a discovery candidate (Section 23).
///
/// Discriminants are explicit and stable for journal encoding. `Rejected` and
/// `PermanentlyInvalidated` are *resting* failure states that may only proceed to
/// `Archived`; `Archived` is the sole fully terminal state. No terminal candidate
/// is ever dropped — it stays queryable forever.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum CandidateLifecycleState {
    /// Just discovered (creation/initialization or active-market qualification).
    Discovered = 0,
    /// Under passive observation, not yet actively evaluated for entry.
    Observing = 1,
    /// Actively evaluated against entry policy each event.
    Evaluating = 2,
    /// Passed gates; eligible for slot arbitration / entry.
    EntryEligible = 3,
    /// A position has been entered.
    Entered = 4,
    /// Holding an open position, managed by the exit family.
    Managing = 5,
    /// Position fully closed (round-trip complete).
    Exited = 6,
    /// Rejected for entry (resting failure state; may only be archived next).
    Rejected = 7,
    /// Permanently invalidated (e.g. proven rug/unsellable); resting failure
    /// state, may only be archived next.
    PermanentlyInvalidated = 8,
    /// Archived after terminal handling; the sole fully terminal state, retained
    /// for replay/research.
    Archived = 9,
}

impl CandidateLifecycleState {
    /// All states in stable discriminant order.
    pub const ALL: [CandidateLifecycleState; 10] = [
        CandidateLifecycleState::Discovered,
        CandidateLifecycleState::Observing,
        CandidateLifecycleState::Evaluating,
        CandidateLifecycleState::EntryEligible,
        CandidateLifecycleState::Entered,
        CandidateLifecycleState::Managing,
        CandidateLifecycleState::Exited,
        CandidateLifecycleState::Rejected,
        CandidateLifecycleState::PermanentlyInvalidated,
        CandidateLifecycleState::Archived,
    ];

    /// `true` when no further lifecycle transition is legal. Only `Archived` is
    /// fully terminal; it remains queryable (Section 23) — terminal means "no more
    /// transitions", not "deleted".
    #[inline]
    pub const fn is_terminal(self) -> bool {
        matches!(self, CandidateLifecycleState::Archived)
    }

    /// `true` for a resting failure state (`Rejected` / `PermanentlyInvalidated`)
    /// whose only legal successor is `Archived`.
    #[inline]
    pub const fn is_failed(self) -> bool {
        matches!(
            self,
            CandidateLifecycleState::Rejected | CandidateLifecycleState::PermanentlyInvalidated
        )
    }

    /// `true` when the candidate currently has (or is entering) an open position
    /// and therefore holds capital at risk.
    #[inline]
    pub const fn holds_position(self) -> bool {
        matches!(
            self,
            CandidateLifecycleState::Entered | CandidateLifecycleState::Managing
        )
    }

    /// Whether a direct transition `self -> next` is legal under the state
    /// machine's rules.
    ///
    /// Encodes the forward flow plus a failure escape hatch. Rules:
    /// * `Archived` (terminal) has no legal successor.
    /// * A resting failure state ([`Self::is_failed`]) may only proceed to
    ///   `Archived` — not to another failure state, not backward.
    /// * From any *live* pre-exit state (`Discovered`..=`Managing`) the candidate
    ///   may fail out to `Rejected` or `PermanentlyInvalidated`.
    /// * The forward path is
    ///   `Discovered -> Observing -> Evaluating -> EntryEligible -> Entered ->
    ///   Managing -> Exited -> Archived`, with `EntryEligible`/`Evaluating` able to
    ///   fall back to earlier evaluation states (re-evaluation).
    #[inline]
    pub fn can_transition_to(self, next: CandidateLifecycleState) -> bool {
        use CandidateLifecycleState::*;
        // Terminal state never transitions.
        if self.is_terminal() {
            return false;
        }
        // Resting failure states may only be archived.
        if self.is_failed() {
            return matches!(next, Archived);
        }
        // Failure escape hatch: only from live pre-exit states.
        if matches!(next, Rejected | PermanentlyInvalidated) {
            return matches!(
                self,
                Discovered | Observing | Evaluating | EntryEligible | Entered | Managing
            );
        }
        match (self, next) {
            (Discovered, Observing) => true,
            (Observing, Evaluating) => true,
            (Evaluating, EntryEligible) => true,
            // Re-evaluation fallbacks.
            (EntryEligible, Evaluating) => true,
            (EntryEligible, Observing) => true,
            (Evaluating, Observing) => true,
            // Entry and management.
            (EntryEligible, Entered) => true,
            (Entered, Managing) => true,
            (Managing, Exited) => true,
            // Archival of completed round-trips.
            (Exited, Archived) => true,
            _ => false,
        }
    }
}

impl fmt::Display for CandidateLifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CandidateLifecycleState::Discovered => "Discovered",
            CandidateLifecycleState::Observing => "Observing",
            CandidateLifecycleState::Evaluating => "Evaluating",
            CandidateLifecycleState::EntryEligible => "EntryEligible",
            CandidateLifecycleState::Entered => "Entered",
            CandidateLifecycleState::Managing => "Managing",
            CandidateLifecycleState::Exited => "Exited",
            CandidateLifecycleState::Rejected => "Rejected",
            CandidateLifecycleState::PermanentlyInvalidated => "PermanentlyInvalidated",
            CandidateLifecycleState::Archived => "Archived",
        })
    }
}
