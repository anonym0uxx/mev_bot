//! Source-registry lifecycle finite-state machine (constitution §18.8).
//!
//! ## Responsibility
//! Enforce the legal lifecycle transitions of an observation source in the
//! provider-neutral source registry (§18.8). A source (a Helius LaserStream
//! adapter, the transitional Jito ShredStream adapter, a canonical-RPC repair
//! adapter, …) moves through:
//!
//! `ACTIVE_PRIMARY, ACTIVE_REDUNDANT, TRANSITIONAL, DEGRADED, SUNSET_PENDING,
//! DISABLED, RETIRED`
//!
//! and this module is the single authority on which moves are legal. It refuses
//! illegal transitions (e.g. resurrecting a `RETIRED` source, or reviving a
//! `SUNSET_PENDING` feed whose vendor announced shutdown — §18.3.1: "Do not
//! fabricate Jito continuity past the announced shutdown").
//!
//! ## "Replaced" is a field, not a state (§18.8 fidelity)
//! The constitution's authoritative lifecycle-state set is exactly the seven
//! above; `replacement status` is a *separate* registry field, not a lifecycle
//! state. So the "TRANSITIONAL → SUNSET → Replaced" story is modeled as a
//! source reaching [`SourceLifecycleStatus::Retired`] while carrying a
//! [`SourceEntry::replaced_by`] pointer to the source that took over its role.
//!
//! ## Canonical authority is immutable (§18.8)
//! "[Quality] measurements may influence source role designation; they may never
//! change canonical authority." The [`SourceAuthorityClass`] is fixed at
//! construction and has no setter — only the lifecycle status ever changes.
//!
//! ## §22 / §57 compliance
//! No floating point, no wall-clock. The transition audit trail is a bounded
//! ring buffer that evicts oldest entries (§57: no unbounded growth).

/// The canonical authority class of a source (§18.8). Fixed at construction and
/// never mutated by a lifecycle transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceAuthorityClass {
    /// Earliest available verified low-latency observations.
    EarliestSignal,
    /// Production structured transactions/accounts/slots/blocks.
    StructuredObservation,
    /// Canonical transaction/account repair and historical retrieval.
    CanonicalRepair,
    /// Finalized truth for the system's own submitted transactions.
    ReconciledExecution,
}

/// Source lifecycle states (§18.8), exactly the constitution's authoritative
/// seven.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceLifecycleStatus {
    /// The primary source for its role.
    ActivePrimary,
    /// A live redundant/comparison source for its role.
    ActiveRedundant,
    /// A sunset-bound source retained temporarily (e.g. Jito ShredStream before
    /// its announced shutdown — §18.3.1).
    Transitional,
    /// Live but health-degraded (§18.8 `DEGRADED`).
    Degraded,
    /// Sunset announced; awaiting disable/retire. One-way (§18.3.1).
    SunsetPending,
    /// Temporarily switched off; may be re-enabled.
    Disabled,
    /// Permanently retired. Terminal.
    Retired,
}

impl SourceLifecycleStatus {
    /// Terminal states admit no further transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(self, SourceLifecycleStatus::Retired)
    }

    /// Whether a source in this state may currently serve live observations.
    ///
    /// Advisory classification for callers; not itself a transition guard.
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            SourceLifecycleStatus::ActivePrimary
                | SourceLifecycleStatus::ActiveRedundant
                | SourceLifecycleStatus::Transitional
                | SourceLifecycleStatus::Degraded
        )
    }

    /// Whether a direct transition from `self` to `next` is legal.
    ///
    /// ## Transition law (§18.8, §18.3.1)
    /// * Live states (`ActivePrimary`, `ActiveRedundant`, `Transitional`,
    ///   `Degraded`) may re-rank among themselves, degrade, be `Disabled`, or
    ///   enter `SunsetPending`.
    /// * `SunsetPending` is one-way: only `Disabled` or `Retired` (no revival —
    ///   an announced shutdown is not fabricated away).
    /// * `Disabled` may re-enter live redundancy/primary, go `SunsetPending`, or
    ///   `Retired`.
    /// * `Retired` is terminal.
    /// * A no-op self-transition is never legal (a transition must change state).
    pub fn can_transition_to(&self, next: SourceLifecycleStatus) -> bool {
        use SourceLifecycleStatus::*;
        if *self == next {
            return false;
        }
        match self {
            ActivePrimary => matches!(next, ActiveRedundant | Degraded | SunsetPending | Disabled),
            ActiveRedundant => {
                matches!(next, ActivePrimary | Degraded | SunsetPending | Disabled)
            }
            Transitional => {
                // A transitional (sunset-bound) source may serve redundancy,
                // degrade, be disabled, or move toward sunset — but is never
                // "promoted" to permanent primary (§18.3.2: no permanent
                // dependency on a sunset source).
                matches!(next, ActiveRedundant | Degraded | SunsetPending | Disabled)
            }
            Degraded => matches!(
                next,
                ActivePrimary | ActiveRedundant | SunsetPending | Disabled
            ),
            SunsetPending => matches!(next, Disabled | Retired),
            Disabled => matches!(
                next,
                ActivePrimary | ActiveRedundant | SunsetPending | Retired
            ),
            Retired => false,
        }
    }
}

/// A stable identifier for a registered source.
///
/// A plain `u32` keeps the registry deterministic and float-free (§22).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u32);

/// Why a transition was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionError {
    /// The `from → to` move is not in the legal transition set.
    IllegalTransition {
        /// The state the source was in.
        from: SourceLifecycleStatus,
        /// The state that was requested.
        to: SourceLifecycleStatus,
    },
    /// The source is already terminal (`Retired`).
    AlreadyTerminal,
}

/// One recorded lifecycle transition (audit trail element).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionRecord {
    /// State before the transition.
    pub from: SourceLifecycleStatus,
    /// State after the transition.
    pub to: SourceLifecycleStatus,
    /// A caller-supplied monotone sequence number (e.g. an event index from the
    /// injected clock/replay ordering — never a wall-clock read, §22).
    pub sequence: u64,
}

/// A registered source with its immutable authority, current lifecycle status,
/// optional replacement pointer, and a bounded transition audit trail.
///
/// ## Constitution §18.8 / §57
/// Models one source-registry row. `authority` is immutable. `replaced_by`
/// records the §18.8 "replacement status" as a field. The audit `log` is a
/// fixed-capacity ring buffer (§57 memory bound): once full, the oldest record
/// is overwritten.
#[derive(Clone, Debug)]
pub struct SourceEntry {
    id: SourceId,
    authority: SourceAuthorityClass,
    status: SourceLifecycleStatus,
    replaced_by: Option<SourceId>,
    log: Vec<TransitionRecord>,
    log_capacity: usize,
    /// Index of the oldest record in the ring (0 while not yet wrapped).
    log_head: usize,
}

impl SourceEntry {
    /// Register a new source in its initial lifecycle state.
    ///
    /// `log_capacity` bounds the transition audit trail (§57); it is raised to a
    /// minimum of 1 so at least the most recent transition is always retained.
    pub fn new(
        id: SourceId,
        authority: SourceAuthorityClass,
        initial: SourceLifecycleStatus,
        log_capacity: usize,
    ) -> Self {
        let cap = log_capacity.max(1);
        Self {
            id,
            authority,
            status: initial,
            replaced_by: None,
            log: Vec::with_capacity(cap),
            log_capacity: cap,
            log_head: 0,
        }
    }

    /// The source's stable id.
    pub fn id(&self) -> SourceId {
        self.id
    }

    /// The immutable canonical authority class (§18.8: never changes).
    pub fn authority(&self) -> SourceAuthorityClass {
        self.authority
    }

    /// The current lifecycle status.
    pub fn status(&self) -> SourceLifecycleStatus {
        self.status
    }

    /// The replacement source, if one has been recorded.
    pub fn replaced_by(&self) -> Option<SourceId> {
        self.replaced_by
    }

    /// Attempt a lifecycle transition to `next`, recording it on success.
    ///
    /// `sequence` is a caller-supplied monotone ordering value (from replay /
    /// injected clock ordering, never a wall-clock read). On an illegal or
    /// terminal transition the status is left unchanged and an error returned.
    pub fn transition(
        &mut self,
        next: SourceLifecycleStatus,
        sequence: u64,
    ) -> Result<(), TransitionError> {
        if self.status.is_terminal() {
            return Err(TransitionError::AlreadyTerminal);
        }
        if !self.status.can_transition_to(next) {
            return Err(TransitionError::IllegalTransition {
                from: self.status,
                to: next,
            });
        }
        let record = TransitionRecord {
            from: self.status,
            to: next,
            sequence,
        };
        self.push_record(record);
        self.status = next;
        Ok(())
    }

    /// Retire this source and record which source replaced it (§18.8 replacement
    /// status). Convenience over [`SourceEntry::transition`] to
    /// [`SourceLifecycleStatus::Retired`] that also sets `replaced_by`.
    ///
    /// Only legal from a state that may transition to `Retired`
    /// (`SunsetPending` or `Disabled`).
    pub fn retire_replaced_by(
        &mut self,
        replacement: SourceId,
        sequence: u64,
    ) -> Result<(), TransitionError> {
        self.transition(SourceLifecycleStatus::Retired, sequence)?;
        self.replaced_by = Some(replacement);
        Ok(())
    }

    /// Number of transitions currently retained in the bounded audit trail.
    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    /// The retained transition history in chronological (oldest-first) order.
    ///
    /// Bounded to `log_capacity` entries (§57); older transitions beyond the
    /// bound have been evicted.
    pub fn history(&self) -> Vec<TransitionRecord> {
        // Reassemble the ring in chronological order starting at `log_head`.
        let n = self.log.len();
        let mut out = Vec::with_capacity(n);
        for offset in 0..n {
            out.push(self.log[(self.log_head + offset) % n]);
        }
        out
    }

    /// Push a record into the bounded ring buffer, evicting the oldest when
    /// full (§57 no-unbounded-growth).
    fn push_record(&mut self, record: TransitionRecord) {
        if self.log.len() < self.log_capacity {
            // Still filling: append. `log_head` stays at 0 until we wrap.
            self.log.push(record);
        } else {
            // Full: overwrite the oldest slot and advance the head.
            self.log[self.log_head] = record;
            self.log_head = (self.log_head + 1) % self.log_capacity;
        }
    }
}
