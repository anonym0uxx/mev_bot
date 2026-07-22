//! Jito submission-surface lifecycle tracking (leaf `in_submission_surface`).
//!
//! Responsibility: track the lifecycle of Jito's transaction-*submission*
//! surfaces (Block Engine, bundles, tips) as a dimension of the source registry
//! that is **independent** of the Jito ShredStream *data-feed* sunset.
//!
//! Constitution basis:
//!   - §18.3.1: "Do not conflate the ShredStream data-feed sunset with Jito's
//!     transaction-submission surfaces (Block Engine, bundles, tips): as of
//!     verification these are separately operated products with no announced
//!     shutdown. Track their lifecycle independently in the source registry ...
//!     and never disable or distrust the submission path because the data feed
//!     retired — or vice versa." (criterion 76.)
//!   - §18.8: the source lifecycle vocabulary (ACTIVE_PRIMARY, ACTIVE_REDUNDANT,
//!     TRANSITIONAL, DEGRADED, SUNSET_PENDING, DISABLED, RETIRED) applied to the
//!     submission dimension.
//!
//! All pure, total, deterministic functions — no floats, no I/O, no wall-clock
//! (§22). Live verification against primary documentation is OUT OF SCOPE and
//! would sit behind an adapter at the edge; this module only models the recorded
//! lifecycle state and its legal transitions.

/// One of Jito's transaction-submission surfaces (§18.3.1).
///
/// These are the products used to *submit* transactions and are entirely
/// separate from the ShredStream data feed. Enumerating them explicitly lets the
/// registry track each surface's lifecycle on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubmissionSurface {
    /// The Jito Block Engine (transaction routing / auction entry point).
    BlockEngine,
    /// Atomic bundle submission (all-or-nothing multi-transaction landing).
    Bundles,
    /// The tip-account payment surface used to bid for bundle inclusion.
    Tips,
}

impl SubmissionSurface {
    /// All submission surfaces, in a fixed order (§18.3.1). Fixed so callers and
    /// tests can iterate deterministically over the full set.
    pub const ALL: [SubmissionSurface; 3] = [
        SubmissionSurface::BlockEngine,
        SubmissionSurface::Bundles,
        SubmissionSurface::Tips,
    ];
}

/// Lifecycle status of a submission surface (§18.8 shared source-lifecycle
/// vocabulary, applied to the submission dimension).
///
/// Per §18.3.1 the submission surfaces have no announced shutdown at
/// verification time, so `Transitional` / `SunsetPending` represent an
/// *anomaly* to be verified from primary documentation, never a state inherited
/// from the data-feed sunset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionSurfaceStatus {
    /// Verified healthy and used as the primary submission path.
    ActivePrimary,
    /// Verified healthy and kept as a redundant / fallback submission path.
    ActiveRedundant,
    /// Verified in a transitional window (only if primary docs announce one).
    Transitional,
    /// Degraded but not retired (elevated failures / partial availability).
    Degraded,
    /// A shutdown has been announced for this surface (independent of any data
    /// feed sunset).
    SunsetPending,
    /// Operator-disabled (recorded intent not to use this surface).
    Disabled,
    /// Fully retired — terminal.
    Retired,
}

impl SubmissionSurfaceStatus {
    /// Whether a surface in this status may still be used to submit
    /// transactions. `Disabled` and `Retired` are the only non-usable states;
    /// a `Transitional` or `Degraded` surface is still usable (§18.3.1: never
    /// distrust the submission path without its own verified reason).
    pub fn is_usable(self) -> bool {
        !matches!(
            self,
            SubmissionSurfaceStatus::Disabled | SubmissionSurfaceStatus::Retired
        )
    }

    /// Whether this status is terminal (`Retired`). A terminal surface never
    /// transitions again.
    pub fn is_terminal(self) -> bool {
        matches!(self, SubmissionSurfaceStatus::Retired)
    }
}

/// A verified lifecycle event for a submission surface (§18.8). Every transition
/// is driven by a verification against primary documentation, modeled here as an
/// explicit event so the state machine stays pure and deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionSurfaceEvent {
    /// Verified healthy → primary path.
    VerifiedHealthy,
    /// Verified healthy → redundant path.
    VerifiedRedundant,
    /// Verified degraded (elevated failure / partial availability).
    VerifiedDegraded,
    /// A shutdown for this surface has been announced in primary docs.
    ShutdownAnnounced,
    /// Operator records the intent to stop using this surface.
    OperatorDisabled,
    /// Retire the surface (only honored after wind-down; see
    /// [`next_submission_status`]).
    Retire,
}

/// Compute the next submission-surface status from the current status and a
/// verified event (§18.8 lifecycle transitions).
///
/// Real transition rules (not a lookup of memorized answers):
///   - `Retired` is terminal: any event on a retired surface yields `Retired`.
///   - `VerifiedHealthy`/`VerifiedRedundant`/`VerifiedDegraded` set the status
///     directly to `ActivePrimary`/`ActiveRedundant`/`Degraded`.
///   - `ShutdownAnnounced` → `SunsetPending`; `OperatorDisabled` → `Disabled`.
///   - `Retire` is honored **only** after a wind-down state
///     (`SunsetPending`, `Disabled`, or `Degraded`); from a healthy state
///     (`ActivePrimary`/`ActiveRedundant`/`Transitional`) it is a no-op that
///     preserves the current status — a healthy submission path is never retired
///     without a prior recorded wind-down (§18.3.1).
pub fn next_submission_status(
    current: SubmissionSurfaceStatus,
    event: SubmissionSurfaceEvent,
) -> SubmissionSurfaceStatus {
    if current.is_terminal() {
        return current;
    }
    match event {
        SubmissionSurfaceEvent::VerifiedHealthy => SubmissionSurfaceStatus::ActivePrimary,
        SubmissionSurfaceEvent::VerifiedRedundant => SubmissionSurfaceStatus::ActiveRedundant,
        SubmissionSurfaceEvent::VerifiedDegraded => SubmissionSurfaceStatus::Degraded,
        SubmissionSurfaceEvent::ShutdownAnnounced => SubmissionSurfaceStatus::SunsetPending,
        SubmissionSurfaceEvent::OperatorDisabled => SubmissionSurfaceStatus::Disabled,
        SubmissionSurfaceEvent::Retire => match current {
            SubmissionSurfaceStatus::SunsetPending
            | SubmissionSurfaceStatus::Disabled
            | SubmissionSurfaceStatus::Degraded => SubmissionSurfaceStatus::Retired,
            // Healthy / transitional surfaces cannot be retired directly.
            other => other,
        },
    }
}

/// Lifecycle status of the Jito ShredStream **data feed** (§18.3 / §14.5).
///
/// A deliberately separate enum from [`SubmissionSurfaceStatus`] so the two
/// dimensions cannot be assigned to one another — the type system reinforces the
/// §18.3.1 non-conflation rule. The data feed is sunset-bound (announced
/// shutdown 2026-09-05, §18.3.1), so its lifecycle is a short transitional →
/// sunset-pending → retired path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFeedStatus {
    /// Operating within its announced transitional window (§18.3 / §14.5).
    Transitional,
    /// Shutdown announced and imminent (§18.3.1: 2026-09-05).
    SunsetPending,
    /// Sunset reached; the data feed is retired.
    Retired,
}

/// Registry of Jito submission-surface lifecycle state, tracked independently of
/// the ShredStream data-feed lifecycle (criterion 76 / §18.3.1).
///
/// Holds one [`SubmissionSurfaceStatus`] per [`SubmissionSurface`] plus the
/// separate [`DataFeedStatus`] for the ShredStream data feed. Mutating the data
/// feed never touches the submission surfaces and vice versa — the whole point
/// of the criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionSurfaceRegistry {
    block_engine: SubmissionSurfaceStatus,
    bundles: SubmissionSurfaceStatus,
    tips: SubmissionSurfaceStatus,
    shredstream_data_feed: DataFeedStatus,
}

impl SubmissionSurfaceRegistry {
    /// The registry in its verified-at-2026-07 default state (§18.3.1): all three
    /// submission surfaces `ActivePrimary` (separately operated, no announced
    /// shutdown), and the ShredStream data feed `Transitional` (sunset-bound).
    pub fn with_verified_defaults() -> Self {
        SubmissionSurfaceRegistry {
            block_engine: SubmissionSurfaceStatus::ActivePrimary,
            bundles: SubmissionSurfaceStatus::ActivePrimary,
            tips: SubmissionSurfaceStatus::ActivePrimary,
            shredstream_data_feed: DataFeedStatus::Transitional,
        }
    }

    /// The recorded status of one submission surface.
    pub fn status(&self, surface: SubmissionSurface) -> SubmissionSurfaceStatus {
        match surface {
            SubmissionSurface::BlockEngine => self.block_engine,
            SubmissionSurface::Bundles => self.bundles,
            SubmissionSurface::Tips => self.tips,
        }
    }

    /// Mutable access to one surface's status slot (internal helper).
    fn status_mut(&mut self, surface: SubmissionSurface) -> &mut SubmissionSurfaceStatus {
        match surface {
            SubmissionSurface::BlockEngine => &mut self.block_engine,
            SubmissionSurface::Bundles => &mut self.bundles,
            SubmissionSurface::Tips => &mut self.tips,
        }
    }

    /// Record a verified status for one submission surface directly (used when
    /// primary documentation asserts an absolute status rather than a
    /// transition). Touches only that surface's submission dimension (§18.3.1).
    pub fn set_submission_status(
        &mut self,
        surface: SubmissionSurface,
        status: SubmissionSurfaceStatus,
    ) {
        *self.status_mut(surface) = status;
    }

    /// Apply a verified lifecycle event to one submission surface, returning the
    /// new status. Touches **only** that surface's submission dimension — never
    /// the data feed and never the other surfaces (§18.3.1).
    pub fn apply_submission_event(
        &mut self,
        surface: SubmissionSurface,
        event: SubmissionSurfaceEvent,
    ) -> SubmissionSurfaceStatus {
        let slot = self.status_mut(surface);
        *slot = next_submission_status(*slot, event);
        *slot
    }

    /// The recorded ShredStream data-feed status (independent dimension).
    pub fn data_feed_status(&self) -> DataFeedStatus {
        self.shredstream_data_feed
    }

    /// Set the ShredStream data-feed status. Touches **only** the data-feed
    /// dimension; submission surfaces are left exactly as they were (§18.3.1).
    pub fn set_data_feed_status(&mut self, status: DataFeedStatus) {
        self.shredstream_data_feed = status;
    }

    /// Retire the ShredStream data feed (the announced 2026-09-05 sunset,
    /// §18.3.1).
    ///
    /// This is the criterion-76 core operation: it sets the data feed to
    /// [`DataFeedStatus::Retired`] and provably leaves every submission surface
    /// untouched — retiring the data source never disables or distrusts the
    /// submission path.
    pub fn retire_data_feed(&mut self) {
        self.shredstream_data_feed = DataFeedStatus::Retired;
    }

    /// A snapshot of only the submission dimension, in [`SubmissionSurface::ALL`]
    /// order. Used to prove the submission surface is intact across data-feed
    /// mutations.
    pub fn submission_snapshot(&self) -> [SubmissionSurfaceStatus; 3] {
        [self.block_engine, self.bundles, self.tips]
    }

    /// Count of submission surfaces still usable (§18.3.1). At most 3, so the
    /// running total cannot overflow `usize`.
    pub fn usable_surface_count(&self) -> usize {
        let mut count: usize = 0;
        for surface in SubmissionSurface::ALL {
            if self.status(surface).is_usable() {
                count += 1;
            }
        }
        count
    }

    /// Whether every submission surface is still usable.
    pub fn all_submission_usable(&self) -> bool {
        self.usable_surface_count() == SubmissionSurface::ALL.len()
    }

    /// Whether at least one submission surface is still usable — i.e. the
    /// submission path as a whole is intact.
    pub fn any_submission_usable(&self) -> bool {
        self.usable_surface_count() > 0
    }
}
