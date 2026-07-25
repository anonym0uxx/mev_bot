//! The episodic record — one immutable "this happened" row (constitution 22, 57).
//!
//! An [`Episode`] binds three things that must never drift apart: **what the setup
//! looked like** ([`crate::fingerprint::SetupFingerprint`]), **where and when it
//! happened** ([`EpisodeContext`]), and **what it actually paid**
//! ([`EpisodeOutcome`]).
//!
//! # Immutability is the product
//!
//! An episode's fields are private and there are no setters. The only way to build
//! one is [`Episode::new`], and the only way to read one is through accessors. That
//! is not ceremony: an episodic memory whose history can be edited is a memory that
//! can be talked into agreeing with whatever the strategy currently believes. The
//! append-only journal in [`crate::persist`] enforces the same discipline on disk.
//!
//! # `was_admitted` and the counterfactual problem
//!
//! The index stores both admitted (actually traded) and rejected setups. Only
//! admitted ones have a realized P&L; a rejected one's `realized_net_lamports` is
//! structurally zero, because nothing was risked. Recall statistics therefore
//! default to admitted-only ([`crate::recall::RecallParams::require_admitted`]) —
//! pooling the two would drag every estimate toward zero and manufacture a
//! flattering, meaningless "low variance". Rejected episodes remain in the index
//! because *how often this setup appears* is a separate, real question.

use crate::concentration::{ConcentrationReading, ConcentrationTrajectory};
use crate::fingerprint::{SetupFingerprint, VenuePhase};

/// Wire/schema version of the episode record (constitution 56 versioned memory).
/// Bumped whenever the field set or its encoding changes; [`crate::persist`]
/// refuses to load a record whose version it does not understand.
///
/// * **1** — the original 20-field fingerprint.
/// * **2** — adds the holder-growth VELOCITY fingerprint field
///   ([`crate::fingerprint::F_HOLDER_GROWTH_VELOCITY`], which changes both the
///   bucket-vector width and [`crate::fingerprint::SIGNATURE_BITS`]) and the
///   [`crate::concentration`] parallel stream on [`EpisodeContext`].
///
/// A schema bump **invalidates the persisted corpus**, and that is the intended
/// behaviour, not a regrettable side effect: a schema-1 bucket vector reinterpreted
/// under schema 2 would silently shift every field's meaning. Old records are
/// REFUSED — by [`crate::recall::EpisodicIndex::push`] on the way in, and by
/// [`crate::persist::decode_episode`] on the way off disk — never reinterpreted.
pub const EPISODE_SCHEMA_VERSION: u16 = 2;

/// Which discovery lane surfaced this token (constitution 29.9). Nominal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiscoveryLane {
    /// Fresh mint observed on the launch stream.
    NewMint,
    /// Curve-to-pool migration event.
    Migration,
    /// Followed from a tracked whale wallet.
    WhaleFollow,
    /// Surfaced by a social call.
    SocialCall,
    /// Promoted from the persistent watchlist.
    Watchlist,
    /// Surfaced by a periodic re-scan of already-known mints.
    Rescan,
}

impl DiscoveryLane {
    /// Dense ordinal used for filter-key packing and the wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::NewMint => 0,
            Self::Migration => 1,
            Self::WhaleFollow => 2,
            Self::SocialCall => 3,
            Self::Watchlist => 4,
            Self::Rescan => 5,
        }
    }

    /// Inverse of [`DiscoveryLane::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::NewMint),
            1 => Some(Self::Migration),
            2 => Some(Self::WhaleFollow),
            3 => Some(Self::SocialCall),
            4 => Some(Self::Watchlist),
            5 => Some(Self::Rescan),
            _ => None,
        }
    }
}

/// How the position ended (constitution 21.8 exit lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitReason {
    /// Setup was never admitted; there is no position and no realized P&L.
    NotAdmitted,
    /// Take-profit target reached.
    TakeProfit,
    /// Hard stop hit.
    StopLoss,
    /// Trailing stop hit after a favourable excursion.
    TrailingStop,
    /// Maximum hold duration elapsed.
    TimeStop,
    /// Exited because structure invalidated (thesis broke before either stop).
    StructureBreak,
    /// Exited because exit liquidity degraded below the safe-unwind floor.
    LiquidityFail,
    /// Operator or governance kill-switch.
    ManualKill,
}

impl ExitReason {
    /// Dense ordinal used in the wire format.
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::NotAdmitted => 0,
            Self::TakeProfit => 1,
            Self::StopLoss => 2,
            Self::TrailingStop => 3,
            Self::TimeStop => 4,
            Self::StructureBreak => 5,
            Self::LiquidityFail => 6,
            Self::ManualKill => 7,
        }
    }

    /// Inverse of [`ExitReason::ordinal`]; out-of-range input yields `None`.
    #[must_use]
    pub const fn from_ordinal(o: u8) -> Option<Self> {
        match o {
            0 => Some(Self::NotAdmitted),
            1 => Some(Self::TakeProfit),
            2 => Some(Self::StopLoss),
            3 => Some(Self::TrailingStop),
            4 => Some(Self::TimeStop),
            5 => Some(Self::StructureBreak),
            6 => Some(Self::LiquidityFail),
            7 => Some(Self::ManualKill),
            _ => None,
        }
    }
}

/// Where and when the episode happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpisodeContext {
    /// Dense internal mint identifier (not the base58 address — the brain is
    /// integer-only, and the address mapping lives in the canonical plane).
    pub mint_id: u64,
    /// Bonding curve or migrated pool at decision time (constitution 100).
    pub venue_phase: VenuePhase,
    /// Exact meta-category identifier — the un-mixed id, so conditioned recall can
    /// filter precisely even though the signature only carries a 16-slot digest.
    pub meta_category_id: u32,
    /// Which lane surfaced the token.
    pub discovery_lane: DiscoveryLane,
    /// Decision-time *information time* in nanoseconds. Never a wall clock.
    pub info_time_ns: u64,
    /// Solana slot at decision time — the replay anchor.
    pub slot: u64,
    /// **PARALLEL STREAM (schema 2).** The holder-distribution LEVEL the episode
    /// was entered under, banded — or the explicit reason none was available.
    ///
    /// Deliberately *not* a fingerprint field. See [`crate::concentration`] for the
    /// full argument; the short version is that a concentration share needs an
    /// `Exact` holder basis, `Exact` is rare, and the fingerprint has no UNKNOWN
    /// rung — so a thin-coverage level inside the signature would fabricate a
    /// neutral bucket for the majority of episodes and make "unknown"
    /// indistinguishable from "measured-low".
    ///
    /// Carried here so every episode records the shape it was entered under, which
    /// is what makes the recall conditioner possible at all. `Unknown` is never
    /// collapsed into a numeric bucket.
    pub concentration: ConcentrationReading,
    /// **PARALLEL STREAM (schema 2).** Which way, and how fast, the *tracked
    /// cohort's internal* concentration was moving at decision time.
    ///
    /// A derivative, and therefore valid on a delta-only ledger where the absolute
    /// level is not — a distinct type from [`Self::concentration`] so the two can
    /// never be substituted for one another.
    pub concentration_trajectory: ConcentrationTrajectory,
}

impl EpisodeContext {
    /// The neutral parallel-stream pair: both channels explicitly `Unknown` with
    /// the `Disarmed` reason.
    ///
    /// For call sites that do not run the holder plane at all (tests, the archetype
    /// exemplars, restore paths for a producer that never measured). It returns
    /// REFUSALS, not zeros — there is no way to get a band out of this.
    #[must_use]
    pub const fn disarmed_concentration() -> (ConcentrationReading, ConcentrationTrajectory) {
        (
            ConcentrationReading::Unknown(crate::concentration::ConcentrationUnknown::Disarmed),
            ConcentrationTrajectory::Unknown(crate::concentration::TrajectoryUnknown::Disarmed),
        )
    }
}

/// What the episode paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpisodeOutcome {
    /// Realized net proceeds in lamports, **after** fees, priority fees, tips and
    /// slippage (constitution 22 money is integer lamports). Signed: this is net
    /// SOL, the only number that matters. Structurally `0` when `was_admitted` is
    /// false.
    pub realized_net_lamports: i128,
    /// Time from entry to exit, nanoseconds of information time. `0` if unadmitted.
    pub hold_duration_ns: u64,
    /// How the position ended.
    pub exit_reason: ExitReason,
    /// Maximum favourable excursion, basis points from entry.
    pub mfe_bps: i64,
    /// Maximum adverse excursion, basis points from entry (reported as a
    /// non-positive-facing magnitude by convention of the caller; the brain does not
    /// reinterpret the sign).
    pub mae_bps: i64,
    /// Whether the setup was actually traded. Drives the admitted-only default in
    /// recall — see the module docs.
    pub was_admitted: bool,
}

impl EpisodeOutcome {
    /// The canonical outcome for a setup the engine looked at and declined.
    #[must_use]
    pub const fn rejected() -> Self {
        Self {
            realized_net_lamports: 0,
            hold_duration_ns: 0,
            exit_reason: ExitReason::NotAdmitted,
            mfe_bps: 0,
            mae_bps: 0,
            was_admitted: false,
        }
    }

    /// `true` when the episode made money net of all costs.
    #[must_use]
    pub const fn is_win(&self) -> bool {
        self.was_admitted && self.realized_net_lamports > 0
    }

    /// `true` when the episode lost money net of all costs.
    #[must_use]
    pub const fn is_loss(&self) -> bool {
        self.was_admitted && self.realized_net_lamports < 0
    }
}

/// One immutable episodic memory.
///
/// Fields are private by design: an episode is written once and read forever. See
/// the module docs for why that is the load-bearing property rather than a style
/// preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Episode {
    schema_version: u16,
    episode_id: u64,
    fingerprint: SetupFingerprint,
    context: EpisodeContext,
    outcome: EpisodeOutcome,
}

impl Episode {
    /// Seal a new episode at the current [`EPISODE_SCHEMA_VERSION`].
    ///
    /// `episode_id` must be assigned by the caller from a strictly increasing
    /// counter; [`crate::recall::EpisodicIndex`] rejects non-monotone ids, which is
    /// what makes recall tie-breaks total and therefore deterministic.
    #[must_use]
    pub const fn new(
        episode_id: u64,
        fingerprint: SetupFingerprint,
        context: EpisodeContext,
        outcome: EpisodeOutcome,
    ) -> Self {
        Self {
            schema_version: EPISODE_SCHEMA_VERSION,
            episode_id,
            fingerprint,
            context,
            outcome,
        }
    }

    /// Rebuild an episode carrying an explicit schema version — only for
    /// [`crate::persist`] restore, where the version comes off the wire.
    #[must_use]
    pub const fn with_schema_version(
        schema_version: u16,
        episode_id: u64,
        fingerprint: SetupFingerprint,
        context: EpisodeContext,
        outcome: EpisodeOutcome,
    ) -> Self {
        Self {
            schema_version,
            episode_id,
            fingerprint,
            context,
            outcome,
        }
    }

    /// Schema version this record was written under.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Monotone episode identifier; also the deterministic recall tie-break key.
    #[must_use]
    pub const fn episode_id(&self) -> u64 {
        self.episode_id
    }

    /// The quantized setup signature.
    #[must_use]
    pub const fn fingerprint(&self) -> &SetupFingerprint {
        &self.fingerprint
    }

    /// Where and when.
    #[must_use]
    pub const fn context(&self) -> &EpisodeContext {
        &self.context
    }

    /// What it paid.
    #[must_use]
    pub const fn outcome(&self) -> &EpisodeOutcome {
        &self.outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{SetupFingerprint, SetupInputs};

    fn ctx() -> EpisodeContext {
        let (concentration, concentration_trajectory) = EpisodeContext::disarmed_concentration();
        EpisodeContext {
            mint_id: 7,
            venue_phase: VenuePhase::Curve,
            meta_category_id: 3,
            discovery_lane: DiscoveryLane::NewMint,
            info_time_ns: 1_000,
            slot: 99,
            concentration,
            concentration_trajectory,
        }
    }

    #[test]
    fn new_stamps_the_current_schema_version() {
        let fp = SetupFingerprint::from_inputs(&SetupInputs::default());
        let e = Episode::new(1, fp, ctx(), EpisodeOutcome::rejected());
        assert_eq!(e.schema_version(), EPISODE_SCHEMA_VERSION);
        assert_eq!(e.episode_id(), 1);
        assert_eq!(e.context().mint_id, 7);
    }

    /// The schema-2 bump is load-bearing: a persisted schema-1 corpus must be
    /// REFUSED, never reinterpreted under the new field layout.
    #[test]
    fn the_schema_version_is_two_and_one_is_no_longer_current() {
        assert_eq!(EPISODE_SCHEMA_VERSION, 2);
        let fp = SetupFingerprint::from_inputs(&SetupInputs::default());
        let stale = Episode::with_schema_version(1, 1, fp, ctx(), EpisodeOutcome::rejected());
        assert_ne!(stale.schema_version(), EPISODE_SCHEMA_VERSION);
    }

    /// The parallel stream defaults to REFUSALS, not to a fabricated neutral band.
    #[test]
    fn the_disarmed_parallel_stream_carries_no_number() {
        let (level, traj) = EpisodeContext::disarmed_concentration();
        assert!(!level.is_known());
        assert_eq!(level.shape(), None);
        assert!(!traj.is_known());
        assert_eq!(traj.shape(), None);
    }

    #[test]
    fn rejected_outcome_is_structurally_flat() {
        let o = EpisodeOutcome::rejected();
        assert_eq!(o.realized_net_lamports, 0);
        assert_eq!(o.hold_duration_ns, 0);
        assert_eq!(o.exit_reason, ExitReason::NotAdmitted);
        assert!(!o.was_admitted);
        assert!(!o.is_win());
        assert!(!o.is_loss());
    }

    #[test]
    fn win_and_loss_require_admission() {
        let mut o = EpisodeOutcome::rejected();
        o.realized_net_lamports = 1_000_000;
        // Not admitted: a "profit" on a trade that was never taken is not a win.
        assert!(!o.is_win());
        o.was_admitted = true;
        assert!(o.is_win());
        o.realized_net_lamports = -1_000_000;
        assert!(o.is_loss());
        o.realized_net_lamports = 0;
        assert!(!o.is_win() && !o.is_loss());
    }

    #[test]
    fn enum_ordinals_round_trip() {
        for o in 0u8..6 {
            let lane = DiscoveryLane::from_ordinal(o).expect("in range");
            assert_eq!(lane.ordinal(), o);
        }
        assert!(DiscoveryLane::from_ordinal(6).is_none());
        for o in 0u8..8 {
            let r = ExitReason::from_ordinal(o).expect("in range");
            assert_eq!(r.ordinal(), o);
        }
        assert!(ExitReason::from_ordinal(8).is_none());
    }

    #[test]
    fn episode_is_copy_and_comparable_but_has_no_setters() {
        let fp = SetupFingerprint::from_inputs(&SetupInputs::default());
        let a = Episode::new(5, fp, ctx(), EpisodeOutcome::rejected());
        let b = a; // Copy, not a mutation handle.
        assert_eq!(a, b);
        assert_eq!(a.outcome(), b.outcome());
        assert_eq!(a.fingerprint(), b.fingerprint());
    }
}
