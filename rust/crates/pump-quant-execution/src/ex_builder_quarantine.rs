//! Leaf `ex_builder_quarantine`: the builder-quarantine circuit breaker
//! (constitution **criterion 78 / §36**).
//!
//! ## Why this exists
//! The §36 6-class failure taxonomy ([`pump_quant_protocol::errors::FailureClass6`],
//! produced by [`pump_quant_protocol::errors::classify_failure6`]) and the coarse
//! 3-class `pump_quant_strategy::safety_integrity::FailureClass::triggers_quarantine`
//! predicate both *classify* a construction defect, but nothing previously
//! **consumed** that classification to actually stop using a broken builder.
//!
//! A *construction* failure — a route/targeting defect ([`FailureClass6::RouteError`]),
//! a program/layout version mismatch ([`FailureClass6::VersionDrift`]), or an
//! unrecognised program error that fails closed ([`FailureClass6::Fatal`]) — is
//! **not** a market condition. Re-submitting an identically-built instruction
//! from the same builder at the same registry version will fail the same way and
//! burn capital / priority fees on a defect. This leaf folds classified
//! outcomes and, after [`QUARANTINE_STRIKE_THRESHOLD`] such failures from one
//! builder at one registry version, trips that builder to `Quarantined`. A
//! would-be submitter MUST consult [`BuilderQuarantineState::check`] /
//! [`BuilderQuarantineState::is_quarantined`] **before** any build or live use.
//!
//! ## Stickiness (criterion 78)
//! Construction failures are **sticky**: a subsequent *successful* trade does
//! NOT clear the quarantine ([`BuilderQuarantineState::record_success`] is a
//! deliberate no-op on the strike counter), because a builder that produced a
//! malformed instruction is not proven fixed by an unrelated success. The only
//! thing that clears a quarantine is a **registry-version bump** for that
//! builder (a new pinned program layout / discriminator set), which resets the
//! slot — modelled by recording against a higher `registry_version`.
//!
//! Non-construction classes ([`FailureClass6::GuardOrSlippage`],
//! [`FailureClass6::Transient`], [`FailureClass6::StateDrift`]) are market /
//! state conditions and never contribute to quarantine — they leave the strike
//! counter untouched.
//!
//! ## Determinism & bounds
//! - No clock, RNG, float, or I/O. `record_failure` folds a caller-supplied
//!   classified outcome; identical call sequences yield identical state.
//! - Bounded state: at most [`MAX_TRACKED_BUILDERS`] slots. One slot per
//!   `builder_id` retaining its *current* registry version (older versions are
//!   dead once bumped, so retaining only the current version is exact for the
//!   `(builder_id, registry_version)` key). When full, a new builder evicts the
//!   least-recently-updated **non-quarantined** slot; quarantined slots are
//!   never evicted (stickiness dominates capacity pressure).
//!
//! ## Constitution refs
//! - criterion 78 — builder-quarantine circuit breaker.
//! - §36 — 6-class failure taxonomy (source of the classified input).
//! - §18.2 — fail closed: an unknown-code `Fatal` counts toward quarantine.
//! - §22 — integer-only, deterministic, no clock / RNG / float / I/O.

use pump_quant_protocol::errors::FailureClass6;

/// Consecutive construction/unknown-code failures from one builder at one
/// registry version required to trip that builder to `Quarantined` (§36).
///
/// Three strikes: enough to distinguish a genuine construction defect from a
/// single fluke, small enough that capital is not repeatedly spent on a broken
/// builder.
pub const QUARANTINE_STRIKE_THRESHOLD: u32 = 3;

/// Maximum number of distinct builders tracked at once (bounded state, §22).
pub const MAX_TRACKED_BUILDERS: usize = 64;

/// Whether a §36 [`FailureClass6`] is a *construction-class* or *unknown-code*
/// failure that contributes to builder quarantine.
///
/// Quarantine-triggering:
/// - [`FailureClass6::RouteError`] — wrong-account / mint-curve targeting defect
///   (the 6-class analogue of the 3-class `Construction`).
/// - [`FailureClass6::VersionDrift`] — compiled layout/discriminator did not
///   match the pinned registry entry.
/// - [`FailureClass6::Fatal`] — unrecognised program error that fails closed
///   (the "unknown-code" case, §18.2), or an authorization failure; either way
///   re-submitting the same build is futile.
///
/// NOT triggering (market / recoverable state, never a construction defect):
/// - [`FailureClass6::GuardOrSlippage`] — the market moved between build & land.
/// - [`FailureClass6::Transient`] — transport transient.
/// - [`FailureClass6::StateDrift`] — on-chain state changed; re-plan, don't quarantine.
#[must_use]
#[inline]
pub fn class_triggers_quarantine(class: FailureClass6) -> bool {
    matches!(
        class,
        FailureClass6::RouteError | FailureClass6::VersionDrift | FailureClass6::Fatal
    )
}

/// Quarantine status of a single builder slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantinePhase {
    /// Below the strike threshold; the builder may be used.
    Clear,
    /// Threshold reached; the builder is quarantined until a registry-version bump.
    Quarantined,
}

/// The admission decision a submitter receives from the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderAdmission {
    /// The builder is clear — the caller may build / submit.
    Admitted,
    /// The builder is quarantined at this registry version — the caller MUST NOT
    /// build or submit; wait for a registry-version bump.
    Quarantined,
}

/// One tracked builder slot: its current registry version, its accumulated
/// construction-strike count at that version, and its phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    builder_id: u32,
    registry_version: u32,
    /// Accumulated construction/unknown-code strikes at `registry_version`.
    strikes: u32,
    phase: QuarantinePhase,
    /// Monotonic update stamp for deterministic LRU eviction (never a clock).
    last_seq: u64,
}

/// Bounded, deterministic builder-quarantine circuit breaker.
///
/// Keyed conceptually by `(builder_id, registry_version)`; because strikes reset
/// on a version bump, only the current version per builder is retained.
#[derive(Debug, Clone)]
pub struct BuilderQuarantineState {
    slots: Vec<Slot>,
    /// Monotonic counter driving deterministic eviction order.
    seq: u64,
}

impl Default for BuilderQuarantineState {
    fn default() -> Self {
        Self::new()
    }
}

impl BuilderQuarantineState {
    /// A fresh tracker with no builders quarantined.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            seq: 0,
        }
    }

    /// Index of the slot for `builder_id`, if tracked.
    #[inline]
    fn find(&self, builder_id: u32) -> Option<usize> {
        self.slots.iter().position(|s| s.builder_id == builder_id)
    }

    /// Next monotonic sequence stamp (saturating; deterministic).
    #[inline]
    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    /// Fold one classified failure outcome into the tracker and return the
    /// builder's resulting phase.
    ///
    /// Behaviour:
    /// - A non-triggering class ([`class_triggers_quarantine`] is `false`) is a
    ///   market/state condition: the strike counter is left **unchanged** and no
    ///   quarantine can result from it.
    /// - A triggering class recorded at a **new** `registry_version` for a
    ///   tracked builder first **resets** that builder's slot (version-bump
    ///   reset), then applies this strike to the fresh version.
    /// - A triggering class increments the strike counter; on reaching
    ///   [`QUARANTINE_STRIKE_THRESHOLD`] the builder trips to
    ///   [`QuarantinePhase::Quarantined`] (sticky — see module docs).
    ///
    /// Bounded: if the tracker is full and this is a new builder, the
    /// least-recently-updated non-quarantined slot is evicted to make room. If
    /// every slot is full **and** quarantined, an untracked new builder cannot
    /// be admitted to the table; its failure is dropped and it reports `Clear`
    /// (it has not itself accumulated any strikes).
    pub fn record_failure(
        &mut self,
        builder_id: u32,
        registry_version: u32,
        class: FailureClass6,
    ) -> QuarantinePhase {
        if !class_triggers_quarantine(class) {
            // Market / recoverable condition: does not touch quarantine state,
            // and — crucially — does NOT reset an existing quarantine either.
            return self
                .find(builder_id)
                .filter(|&i| self.slots[i].registry_version == registry_version)
                .map_or(QuarantinePhase::Clear, |i| self.slots[i].phase);
        }

        let stamp = self.next_seq();

        if let Some(i) = self.find(builder_id) {
            if self.slots[i].registry_version != registry_version {
                // Registry-version bump: reset this builder's slot entirely.
                self.slots[i].registry_version = registry_version;
                self.slots[i].strikes = 0;
                self.slots[i].phase = QuarantinePhase::Clear;
            }
            self.slots[i].strikes = self.slots[i].strikes.saturating_add(1);
            if self.slots[i].strikes >= QUARANTINE_STRIKE_THRESHOLD {
                self.slots[i].phase = QuarantinePhase::Quarantined;
            }
            self.slots[i].last_seq = stamp;
            return self.slots[i].phase;
        }

        // New builder — insert, evicting if necessary.
        let new_slot = Slot {
            builder_id,
            registry_version,
            strikes: 1,
            phase: if QUARANTINE_STRIKE_THRESHOLD <= 1 {
                QuarantinePhase::Quarantined
            } else {
                QuarantinePhase::Clear
            },
            last_seq: stamp,
        };

        if self.slots.len() < MAX_TRACKED_BUILDERS {
            self.slots.push(new_slot);
            return new_slot.phase;
        }

        // Full: evict the least-recently-updated NON-quarantined slot.
        let victim = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.phase != QuarantinePhase::Quarantined)
            .min_by_key(|(_, s)| s.last_seq)
            .map(|(i, _)| i);

        match victim {
            Some(i) => {
                self.slots[i] = new_slot;
                new_slot.phase
            }
            // Every slot quarantined: cannot admit — drop, report Clear.
            None => QuarantinePhase::Clear,
        }
    }

    /// Record a *successful* trade for a builder. Deliberately a **no-op** on the
    /// strike counter and phase: construction failures are sticky (criterion 78),
    /// so an unrelated success must not clear a quarantine. Present as an explicit
    /// contract point so callers cannot accidentally reset quarantine on success.
    #[inline]
    pub fn record_success(&mut self, _builder_id: u32, _registry_version: u32) {
        // Intentionally empty: success never clears construction quarantine.
    }

    /// Explicitly clear a builder's quarantine on a registry-version bump.
    ///
    /// The same reset happens implicitly the first time [`record_failure`] is
    /// called with a higher `registry_version`; this method lets a caller
    /// perform the reset eagerly when it rolls the pinned registry forward.
    pub fn on_registry_bump(&mut self, builder_id: u32, new_registry_version: u32) {
        if let Some(i) = self.find(builder_id) {
            if self.slots[i].registry_version != new_registry_version {
                self.slots[i].registry_version = new_registry_version;
                self.slots[i].strikes = 0;
                self.slots[i].phase = QuarantinePhase::Clear;
            }
        }
    }

    /// Whether `builder_id` at `registry_version` is currently quarantined.
    ///
    /// A slot recorded at a *different* version is stale for this query and
    /// reports `false` (the version was bumped ⇒ quarantine cleared).
    #[must_use]
    pub fn is_quarantined(&self, builder_id: u32, registry_version: u32) -> bool {
        self.find(builder_id).is_some_and(|i| {
            self.slots[i].registry_version == registry_version
                && self.slots[i].phase == QuarantinePhase::Quarantined
        })
    }

    /// The gate a would-be submitter MUST consult **before** any build / live
    /// use. Returns [`BuilderAdmission::Quarantined`] iff
    /// [`is_quarantined`](Self::is_quarantined) holds.
    #[must_use]
    pub fn check(&self, builder_id: u32, registry_version: u32) -> BuilderAdmission {
        if self.is_quarantined(builder_id, registry_version) {
            BuilderAdmission::Quarantined
        } else {
            BuilderAdmission::Admitted
        }
    }

    /// Current accumulated strike count for `(builder_id, registry_version)`.
    /// `0` if untracked or recorded at a different version (introspection / tests).
    #[must_use]
    pub fn strikes(&self, builder_id: u32, registry_version: u32) -> u32 {
        self.find(builder_id)
            .filter(|&i| self.slots[i].registry_version == registry_version)
            .map_or(0, |i| self.slots[i].strikes)
    }

    /// Number of builders currently tracked (bounded by [`MAX_TRACKED_BUILDERS`]).
    #[must_use]
    pub fn tracked_len(&self) -> usize {
        self.slots.len()
    }
}
