//! Parameter-envelope bounds enforcement — the fast-path guard of two-speed
//! governance (constitution §56.2).
//!
//! ## Responsibility
//! A promoted champion carries *validated parameter ranges* — a
//! [`ParameterEnvelope`] `[min, max]` per dimension, with an optional discrete
//! `step` grid for the allowed values a deterministic controller may select
//! (§56.2: "within the envelope, deterministic controllers may select allowed
//! values without a new experiment"). This module enforces that:
//!
//! * An online change **inside** the envelope is accepted (snapped to the grid
//!   when a step is registered) — the fast path.
//! * An online change **outside** the envelope is either **clamped** to the
//!   boundary or **rejected**, per the configured [`EnforcementMode`]. Crossing
//!   the envelope is never silently applied — "crossing the envelope requires
//!   the full slow path".
//!
//! Time-of-day and regime scheduling are explicitly eligible envelope
//! dimensions (§56.2); this guard treats every dimension uniformly as an
//! integer-valued range, so a per-window sizing range is just another
//! [`ParameterEnvelope`].
//!
//! ## §22 / §705 compliance
//! All values are `i128` fixed-point (caller-chosen scale: lamports, basis
//! points, token base units). No floating point. Span and grid arithmetic use
//! explicit `checked_*` operations; an envelope whose span is not representable
//! in `i128` is rejected at construction rather than risking silent overflow.

/// How to treat an online change that falls **outside** a registered envelope.
///
/// ## Constitution §56.2
/// Both are legitimate fast-path responses; neither ever crosses the envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnforcementMode {
    /// Bring the proposed value to the nearest in-envelope grid value.
    Clamp,
    /// Refuse the change entirely; the current value is retained unchanged.
    Reject,
}

/// Errors constructing a [`ParameterEnvelope`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeError {
    /// `min > max`: an empty range.
    InvertedBounds,
    /// `step == 0`: a zero grid resolution is meaningless.
    ZeroStep,
    /// `max - min` does not fit in `i128` (would overflow span arithmetic).
    SpanNotRepresentable,
}

/// The classification of an enforced online change.
///
/// ## Constitution §56.2
/// Distinguishes an in-envelope fast-path adaptation from an envelope-crossing
/// attempt that was clamped or rejected — the audit distinction governance
/// needs (a `Clamped`/`Rejected` outcome signals a slow-path pressure point).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// In-envelope and already on the grid: applied verbatim.
    Accepted,
    /// In-envelope but off-grid: snapped to the nearest grid value.
    Snapped,
    /// Out-of-envelope under [`EnforcementMode::Clamp`]: pinned to the boundary
    /// grid value. Signals a controller pushing at the validated edge.
    Clamped,
    /// Out-of-envelope under [`EnforcementMode::Reject`]: refused, current value
    /// retained. Signals a genuine envelope crossing that requires the slow
    /// path.
    Rejected,
}

/// A single dimension of a promoted strategy's validated parameter ranges.
///
/// ## Constitution §56.2
/// The `[min, max]` are validated ranges (the *whole* envelope was validated
/// per §53 neighborhood stability, not a point). `step` is the discrete grid a
/// fast-path controller selects from; `step == 1` means every integer in range
/// is allowed. `min` is always a grid point (offset 0); other grid points are
/// `min + k*step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParameterEnvelope {
    min: i128,
    max: i128,
    step: i128,
}

impl ParameterEnvelope {
    /// Construct and validate an envelope.
    ///
    /// Rejects inverted bounds, a zero step, and a span (`max - min`) that would
    /// overflow `i128` — the last so all later grid arithmetic is overflow-free
    /// by construction (§705 explicit overflow).
    pub fn new(min: i128, max: i128, step: i128) -> Result<Self, EnvelopeError> {
        if step <= 0 {
            return Err(EnvelopeError::ZeroStep);
        }
        if min > max {
            return Err(EnvelopeError::InvertedBounds);
        }
        // Span must be representable so `(value - min)` never overflows for any
        // value in `[min, max]`.
        if max.checked_sub(min).is_none() {
            return Err(EnvelopeError::SpanNotRepresentable);
        }
        Ok(Self { min, max, step })
    }

    /// Inclusive lower bound.
    pub fn min(&self) -> i128 {
        self.min
    }

    /// Inclusive upper bound.
    pub fn max(&self) -> i128 {
        self.max
    }

    /// Grid resolution (`>= 1`).
    pub fn step(&self) -> i128 {
        self.step
    }

    /// Is `value` inside the inclusive `[min, max]` envelope?
    pub fn contains(&self, value: i128) -> bool {
        value >= self.min && value <= self.max
    }

    /// Is `value` exactly on the grid *and* inside the envelope?
    pub fn is_grid_value(&self, value: i128) -> bool {
        if !self.contains(value) {
            return false;
        }
        // `value - min` is in `[0, span]` and cannot overflow (span was checked
        // at construction). `% step` is exact integer arithmetic.
        (value - self.min) % self.step == 0
    }

    /// Snap an *in-envelope* value to the nearest grid point (ties toward
    /// `min`). Precondition: `self.contains(value)`.
    ///
    /// Returns a value in `[min, largest_grid_point <= max]`, always on the grid
    /// and always in-envelope.
    fn snap_in_envelope(&self, value: i128) -> i128 {
        debug_assert!(self.contains(value));
        // Largest grid point <= value. `offset` is in `[0, span]`, no overflow.
        let offset = value - self.min;
        let lo = self.min + (offset / self.step) * self.step;
        if lo == value {
            return lo;
        }
        // Candidate next grid point up; guard against i128 overflow and the max
        // bound. `checked_add` covers the overflow edge (§705).
        match lo.checked_add(self.step) {
            Some(hi) if hi <= self.max => {
                // Nearest wins; tie (equal distance) goes to `lo` (toward min).
                if (value - lo) <= (hi - value) {
                    lo
                } else {
                    hi
                }
            }
            // No valid higher grid point within bounds: floor is the answer.
            _ => lo,
        }
    }

    /// The result of applying enforcement: what happened and the resulting
    /// value (unchanged from `current` when [`ChangeOutcome::Rejected`]).
    pub fn enforce(
        &self,
        proposed: i128,
        current: i128,
        mode: EnforcementMode,
    ) -> EnvelopeDecision {
        if proposed < self.min {
            return match mode {
                EnforcementMode::Clamp => EnvelopeDecision {
                    outcome: ChangeOutcome::Clamped,
                    // `min` is always a grid point.
                    value: self.min,
                },
                EnforcementMode::Reject => EnvelopeDecision {
                    outcome: ChangeOutcome::Rejected,
                    value: current,
                },
            };
        }
        if proposed > self.max {
            return match mode {
                EnforcementMode::Clamp => EnvelopeDecision {
                    outcome: ChangeOutcome::Clamped,
                    // Largest grid point <= max (never above the bound).
                    value: self.snap_in_envelope(self.max),
                },
                EnforcementMode::Reject => EnvelopeDecision {
                    outcome: ChangeOutcome::Rejected,
                    value: current,
                },
            };
        }
        // In-envelope: fast path. Snap to grid.
        let snapped = self.snap_in_envelope(proposed);
        EnvelopeDecision {
            outcome: if snapped == proposed {
                ChangeOutcome::Accepted
            } else {
                ChangeOutcome::Snapped
            },
            value: snapped,
        }
    }
}

/// The outcome of a single [`ParameterEnvelope::enforce`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvelopeDecision {
    /// How the proposed change was classified.
    pub outcome: ChangeOutcome,
    /// The value the controller should now use.
    pub value: i128,
}

/// A stable identifier for a governed parameter dimension.
///
/// ## Constitution §56.2
/// Identifies one envelope dimension (a sizing knob, a fee bound, a per-window
/// exposure range, …). A plain `u32` keeps registry iteration deterministic and
/// float-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DimensionId(pub u32);

/// One registered dimension: its envelope plus the controller's current value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisteredParameter {
    /// The dimension this envelope governs.
    pub dimension: DimensionId,
    /// The validated envelope.
    pub envelope: ParameterEnvelope,
    /// The current in-envelope value.
    pub current: i128,
}

/// Errors from [`ParameterRegistry`] operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// The registry is at capacity and cannot register another dimension
    /// (§57 memory bound).
    CapacityExceeded,
    /// A dimension with this id is already registered.
    DuplicateDimension,
    /// The initial value is outside the envelope.
    InitialOutOfEnvelope,
    /// No envelope is registered for the referenced dimension.
    UnknownDimension,
}

/// A memory-bounded set of governed parameters and their live values.
///
/// ## Constitution §56.2 / §57
/// Holds one [`ParameterEnvelope`] per dimension and applies fast-path changes
/// against it. Capacity is fixed at construction (§57: every collection has an
/// explicit bound); entries are kept sorted by [`DimensionId`] for deterministic
/// iteration and `O(log n)` lookup.
#[derive(Clone, Debug)]
pub struct ParameterRegistry {
    params: Vec<RegisteredParameter>,
    capacity: usize,
}

impl ParameterRegistry {
    /// Construct an empty registry with a fixed capacity (§57 memory bound).
    pub fn new(capacity: usize) -> Self {
        Self {
            params: Vec::new(),
            capacity,
        }
    }

    /// Number of registered dimensions.
    pub fn len(&self) -> usize {
        self.params.len()
    }

    /// Whether the registry holds no dimensions.
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    /// Fixed capacity bound.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Deterministic (sorted-by-dimension) view of all registered parameters.
    pub fn parameters(&self) -> &[RegisteredParameter] {
        &self.params
    }

    /// Register a new dimension with its envelope and an initial value.
    ///
    /// The initial value must lie inside the envelope; it is snapped to the grid
    /// so the stored `current` is always a legal grid value.
    pub fn register(
        &mut self,
        dimension: DimensionId,
        envelope: ParameterEnvelope,
        initial: i128,
    ) -> Result<(), RegistryError> {
        if !envelope.contains(initial) {
            return Err(RegistryError::InitialOutOfEnvelope);
        }
        match self
            .params
            .binary_search_by(|p| p.dimension.cmp(&dimension))
        {
            Ok(_) => Err(RegistryError::DuplicateDimension),
            Err(insert_at) => {
                if self.params.len() >= self.capacity {
                    return Err(RegistryError::CapacityExceeded);
                }
                let current = envelope.snap_in_envelope(initial);
                self.params.insert(
                    insert_at,
                    RegisteredParameter {
                        dimension,
                        envelope,
                        current,
                    },
                );
                Ok(())
            }
        }
    }

    /// The current value of a registered dimension, if any.
    pub fn current(&self, dimension: DimensionId) -> Option<i128> {
        self.params
            .binary_search_by(|p| p.dimension.cmp(&dimension))
            .ok()
            .map(|i| self.params[i].current)
    }

    /// The envelope of a registered dimension, if any.
    pub fn envelope(&self, dimension: DimensionId) -> Option<ParameterEnvelope> {
        self.params
            .binary_search_by(|p| p.dimension.cmp(&dimension))
            .ok()
            .map(|i| self.params[i].envelope)
    }

    /// Apply a fast-path online change to a dimension under `mode`.
    ///
    /// On [`ChangeOutcome::Accepted`], [`ChangeOutcome::Snapped`], or
    /// [`ChangeOutcome::Clamped`] the stored `current` is updated to the
    /// enforced value; on [`ChangeOutcome::Rejected`] it is left unchanged
    /// (§56.2: envelope crossings require the slow path).
    pub fn propose(
        &mut self,
        dimension: DimensionId,
        proposed: i128,
        mode: EnforcementMode,
    ) -> Result<EnvelopeDecision, RegistryError> {
        let idx = self
            .params
            .binary_search_by(|p| p.dimension.cmp(&dimension))
            .map_err(|_| RegistryError::UnknownDimension)?;
        let param = &mut self.params[idx];
        let decision = param.envelope.enforce(proposed, param.current, mode);
        if decision.outcome != ChangeOutcome::Rejected {
            param.current = decision.value;
        }
        Ok(decision)
    }
}
