//! The **social abstraction plane**: trust, support, follow recommendation and
//! style-lens performance — the four "think like a pro" questions, answered from
//! the same episodic evidence the rest of the brain uses, and answered on the
//! REPORT plane only.
//!
//! # What this module is
//!
//! `pump_quant_brain` grew four reflection-plane estimators
//! ([`pump_quant_brain::trust`], [`pump_quant_brain::social_support`],
//! [`pump_quant_brain::follow_reco`], [`pump_quant_brain::archetype`]). This
//! module is the single app-side seam onto all four. It owns their state, feeds
//! them from the calls the engine already ingests, and renders bounded,
//! deterministic rows for [`crate::engine::Report`].
//!
//! It answers, for an operator:
//!
//! * *"Does this coin have strong social support, or is it a staged crowd?"* —
//!   [`SocialSupportRow`], distinct-originator breadth, trust-weighted, spread
//!   across platforms, penalised for coordination and near-duplicate echo.
//! * *"Who should I follow that I am not?"* — [`FollowRecoRow`], ranked by
//!   lead-time-weighted realized attribution. *"Who should I drop?"* —
//!   [`UnfollowRow`].
//! * *"Can I trust the accounts I am acting on?"* — [`CallerTrustRow`].
//! * *"Which style is actually paying for us?"* — [`LensScoreRow`].
//! * *"What should the capture layer go and fetch to sharpen this?"* —
//!   [`SupportNeed`], fed into the reflection output as a work list.
//!
//! # Three hard boundaries
//!
//! **1. Report-only (§29/§71).** Nothing in this module is read by promotion,
//! ranking, sizing or the gate. There is no call site in `engine.rs` that consults
//! a trust tier, a support score, a follow recommendation or a lens scoreboard on
//! a decision path. That is proven, not asserted, by
//! `tests/social_hardening.rs::social_plane_is_decision_inert`.
//!
//! **2. Research only, never interaction (§110).** [`FollowRecoRow`] is a list of
//! accounts to *watch*. This crate contains no posting, replying, liking or
//! following capability, no outbound social client of any kind, and none may be
//! added. The recommendation is an input to a human decision.
//!
//! **3. Provenance or nothing (§29.8/§34.3).** Every social datum that lands here
//! carries its evidence class — platform, author, designated flag, earned trust
//! tier, and the information time it was observed at ([`SocialEvidenceRow`]). An
//! anonymous social scalar has no representation in this module: there is no
//! constructor that takes a bare number. Rows past the engine's evidence TTL are
//! **dropped**, never carried forward at their last value.
//!
//! Integer-only (§22), bounded (§99), named consts with §-citations (§102).

use std::collections::BTreeMap;

use pump_quant_brain::archetype::{
    archetype_performance, best_paying_lens, StyleLens, ARCHETYPE_MIN_SAMPLE, STYLE_LENSES,
};
use pump_quant_brain::fingerprint::VenuePhase;
use pump_quant_brain::follow_reco::{FollowRecommender, FollowSet};
use pump_quant_brain::recall::{EpisodicIndex, RecallVerdict};
use pump_quant_brain::social_recall::{Platform, SocialRecallIndex};
use pump_quant_brain::social_support::{
    ContentEchoWitness, SocialSupport, SocialSupportVerdict, SupportInputNeed, SupportUnknown,
};
use pump_quant_brain::trust::{SocialTrust, SourceExposure, TrustError, TrustTier, TrustVerdict};

// ---------------------------------------------------------------------------
// Named constants (§102/§99)
// ---------------------------------------------------------------------------

/// §99 bound on the per-(mint, author) social evidence ledger. Past the cap the
/// lexicographically-smallest key is evicted — a pure function of state, so no
/// clock and no insertion order can change which row goes.
pub const SOCIAL_EVIDENCE_CAP: usize = 4_096;

/// §99 bound on the retained content-echo witnesses. Near-duplicate detection is
/// what separates BREADTH from ECHO; without a witness the support score is only
/// an upper bound, so the ring is generous but finite. Oldest call id evicted.
pub const ECHO_WITNESS_CAP: usize = 4_096;

/// §99 bound on the mints whose support verdict is surfaced on the `Report`.
pub const SUPPORT_ROW_CAP: usize = 8;

/// §99 bound on the surfaced capture work list.
pub const SUPPORT_NEED_CAP: usize = 16;

/// §99 bound on the surfaced follow / unfollow rows.
pub const FOLLOW_ROW_CAP: usize = 8;

/// §99 bound on the surfaced caller-trust rows.
pub const TRUST_ROW_CAP: usize = 8;

/// §99 bound on the surfaced social evidence rows (the provenance chain readout).
pub const EVIDENCE_ROW_CAP: usize = 16;

// ---------------------------------------------------------------------------
// Provenance (§29.8 / §34.3)
// ---------------------------------------------------------------------------

/// One social datum's **evidence class**: which platform, which author, whether
/// they are a designated caller, what trust they have earned, and when we last
/// saw them say it.
///
/// There is deliberately no way to construct this from a bare score. A social
/// quantity with no author and no platform is not weak evidence, it is *not
/// evidence*, and it has no representation here (§29.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialEvidenceRow {
    /// The market the evidence concerns (FNV-1a of the mint bytes).
    pub mint_id: u64,
    /// The originating author.
    pub author_id: u64,
    /// [`Platform::ordinal`] of the capture lane that carried it.
    pub platform_code: u8,
    /// Whether the author is a designated (paid-room / curated) caller.
    pub designated: bool,
    /// Distinct calls this author has made about this mint.
    pub calls: u32,
    /// Engine information time (logical tick) of the FIRST observed call.
    pub first_tick: u64,
    /// Engine information time (logical tick) of the MOST RECENT observed call.
    /// The freshness this row is aged against (§34.3).
    pub last_tick: u64,
    /// The author's EARNED trust tier ([`TrustTier::ordinal`]) as of the last
    /// refresh. `Unproven` is the honest default and the only tier an
    /// insufficient record can produce.
    pub trust_tier_code: u8,
    /// The author's operator-set exposure ([`SourceExposure::ordinal`]).
    pub exposure_code: u8,
}

impl SocialEvidenceRow {
    /// Whether this evidence is still inside the engine's evidence TTL at
    /// `now_tick` (§34.3). A row that is not fresh is DROPPED, never re-used at
    /// its last value.
    #[must_use]
    pub const fn is_fresh(&self, now_tick: u64, ttl_ticks: u64) -> bool {
        now_tick.saturating_sub(self.last_tick) <= ttl_ticks
    }
}

// ---------------------------------------------------------------------------
// Report rows
// ---------------------------------------------------------------------------

/// Why the support estimator declined. Counts and floors only — no partial score
/// exists to leak (§46).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportRefusal {
    /// No call at all for this mint in the estimator's window.
    NoCallsInWindow,
    /// Some originators, but below the breadth floor (after content clustering
    /// and aggregator exclusion).
    InsufficientOriginators {
        /// Effective distinct originators found.
        n_originators: u32,
        /// The floor they failed to reach.
        min_originators: u32,
    },
    /// Originators exist, but every one carries zero trust weight.
    NoTrustedOriginator {
        /// Effective distinct originators found.
        n_originators: u32,
    },
}

/// A support verdict, in the same shape the brain gives it: an estimate, or a
/// refusal that structurally cannot carry one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportVerdictRow {
    /// Scored.
    Known {
        /// Composite support score, bps.
        support_score_bp: u32,
        /// Distinct originators after content clustering.
        n_effective_originators: u32,
        /// How many of them are [`TrustTier::Trusted`].
        n_trusted_originators: u32,
        /// Distinct originating platforms.
        distinct_platforms: u32,
        /// Coordination penalty actually applied, bps.
        coordination_penalty_bp: u32,
        /// Share of calls that were near-duplicates, bps.
        duplicate_share_bp: u32,
        /// Sub-window support velocity, signed bps.
        velocity_bp: i64,
        /// [`pump_quant_brain::social_support::SupportTrend::ordinal`].
        trend_code: u8,
        /// Whether content digests were available (false ⇒ the score is an
        /// UPPER BOUND: echo could not be detected).
        content_evidence: bool,
    },
    /// Refused.
    Unknown(SupportRefusal),
}

/// One watched mint's social-support picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialSupportRow {
    /// The market.
    pub mint_id: u64,
    /// Engine tick of the freshest social evidence backing this row (§34.3).
    pub freshest_tick: u64,
    /// The verdict.
    pub verdict: SupportVerdictRow,
}

/// A piece of EXTERNAL evidence that would sharpen a support estimate — the
/// Phase-B capture layer's work list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportNeed {
    /// Below the breadth floor: more independent originators is what would move
    /// this from Unknown to a score.
    MoreOriginators {
        /// The market.
        mint_id: u64,
        /// Effective originators observed.
        n_originators: u32,
        /// The floor they must reach.
        min_originators: u32,
    },
    /// No content digests supplied, so near-duplicate detection is off and the
    /// score is an upper bound.
    ContentDigests {
        /// The market.
        mint_id: u64,
        /// Calls in the window lacking a digest.
        n_calls: u32,
    },
    /// No originating call observed on this platform — poll it.
    PlatformCoverage {
        /// The market.
        mint_id: u64,
        /// [`Platform::ordinal`] to query.
        platform_code: u8,
    },
    /// This originator has no usable track record; attribute markouts to them.
    AuthorTrackRecord {
        /// The market.
        mint_id: u64,
        /// The author.
        author_id: u64,
    },
    /// This originator scores as trusted but their crowding is unset; §28 needs
    /// an OPERATOR judgement before we lean on them.
    SourceExposure {
        /// The market.
        mint_id: u64,
        /// The author.
        author_id: u64,
    },
}

/// An account worth following — **and monitoring, not interacting with** (§110).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowRecoRow {
    /// The author.
    pub author_id: u64,
    /// [`Platform::ordinal`] carrying most of their attributed calls.
    pub platform_code: u8,
    /// Attributed calls backing the row.
    pub n_calls: u32,
    /// Lead-time-weighted realized net attributed to them, lamports.
    pub realized_net_attributed_lamports: i128,
    /// Median lead from their call to our decision, nanoseconds.
    pub median_lead_ns: u64,
    /// [`TrustTier::ordinal`] — context, never the reason they rank.
    pub trust_tier_code: u8,
    /// Confidence in the row, bps.
    pub confidence_bp: u32,
}

/// A followed account whose attributed contribution has gone negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnfollowRow {
    /// The author.
    pub author_id: u64,
    /// [`Platform::ordinal`] carrying most of their attributed calls.
    pub platform_code: u8,
    /// Attributed calls backing the row.
    pub n_calls: u32,
    /// Lead-time-weighted realized net attributed to them — negative by
    /// definition of appearing here.
    pub realized_net_attributed_lamports: i128,
    /// Median lead, nanoseconds.
    pub median_lead_ns: u64,
    /// [`TrustTier::ordinal`].
    pub trust_tier_code: u8,
}

/// An earned trust verdict, or an explicit refusal carrying no score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustVerdictRow {
    /// Scored from realized net SOL alone (popularity is structurally
    /// unreachable from the data the trust module reads).
    Known {
        /// Post-demotion trust score, signed bps.
        trust_score_bp: i32,
        /// Attributed markouts behind it.
        n_markouts: u32,
        /// Decay-adjusted effective weight, in weight units.
        effective_weight_units: u64,
        /// [`TrustTier::ordinal`].
        tier_code: u8,
    },
    /// Below the evidence floor. Carries NO estimate, by construction.
    Unknown {
        /// Always [`TrustTier::Unproven`]'s ordinal — the only tier an
        /// insufficient record can produce.
        tier_code: u8,
    },
}

/// The trust standing of one caller we are actually acting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallerTrustRow {
    /// The author.
    pub author_id: u64,
    /// Operator-set exposure ([`SourceExposure::ordinal`]) — §28, never inferred.
    pub exposure_code: u8,
    /// The verdict.
    pub verdict: TrustVerdictRow,
}

/// Realized per-lens performance, or an explicit refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensVerdictRow {
    /// Measured over admitted episodes in this venue phase that fit the lens.
    Known {
        /// Episodes matched.
        n_matched: u32,
        /// Median realized net, lamports.
        median_net_lamports: i128,
        /// Mean realized net, lamports.
        mean_net_lamports: i128,
        /// Win rate over decisive episodes, bps.
        win_rate_bp: u32,
        /// Median hold, nanoseconds.
        median_hold_ns: u64,
    },
    /// Below the sample floor, or nothing in scope. No estimate exists.
    Unknown,
}

/// One row of the "which style is actually paying for us" scoreboard.
///
/// **Phase-separated (§100).** `archetype_performance` REQUIRES a venue phase —
/// there is no phase-pooled statistic, because the bonding curve and the migrated
/// pool have different fee, slippage and adversary structure and pooling their
/// outcomes into one number is a lie. So the scoreboard is emitted once per phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LensScoreRow {
    /// [`StyleLens::ordinal`].
    pub lens_code: u8,
    /// [`VenuePhase::ordinal`].
    pub venue_phase_code: u8,
    /// The verdict.
    pub verdict: LensVerdictRow,
}

// ---------------------------------------------------------------------------
// The plane
// ---------------------------------------------------------------------------

/// One social call, presented to the plane **with its full evidence class**.
///
/// This type is the enforcement of "provenance or nothing" (§29.8): every field
/// is a fact about WHERE the datum came from, and [`SocialPlane::record_call`]
/// takes nothing else. There is no path that admits a social quantity with no
/// author and no platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocialCallEvidence {
    /// The market called (FNV-1a of the mint bytes).
    pub mint_id: u64,
    /// The originating author.
    pub author_id: u64,
    /// The capture lane that carried it.
    pub platform: Platform,
    /// Whether the author is a designated (paid-room / curated) caller.
    pub designated: bool,
    /// Engine information time (logical tick) of the observation.
    pub now_tick: u64,
    /// The brain's issued call id, when the call landed in its ledger. `None`
    /// means no echo witness can be bound and the support score stays an upper
    /// bound.
    pub call_id: Option<u64>,
    /// Digest of the post's content, for near-duplicate (echo) detection.
    pub content_digest: u64,
}

/// The point-in-time context one [`SocialPlane::refresh`] runs at.
#[derive(Clone, Copy, Debug)]
pub struct RefreshAt<'a> {
    /// Mint ids currently on the watchlist, in the engine's own order.
    pub watched: &'a [u64],
    /// Engine information time (logical tick).
    pub now_tick: u64,
    /// The same instant on the brain's nanosecond information-time axis.
    pub as_of_ns: u64,
    /// The engine's evidence TTL in ticks (§34.3).
    pub ttl_ticks: u64,
    /// §46 sample floor for the style-lens scoreboard.
    pub min_sample: u32,
}

/// Trust + support + follow-recommendation + archetype state, plus the bounded
/// cached readouts the `Report` renders.
#[derive(Debug)]
pub struct SocialPlane {
    trust: SocialTrust,
    support: SocialSupport,
    follow: FollowRecommender,
    followed: FollowSet,
    /// call_id → content digest, ascending by call id (bounded ring).
    witnesses: Vec<ContentEchoWitness>,
    /// (mint_id, author_id) → provenance. Bounded (§99).
    evidence: BTreeMap<(u64, u64), SocialEvidenceRow>,

    // cached readouts (rebuilt at the reflection cadence)
    support_rows: Vec<SocialSupportRow>,
    needs: Vec<SupportNeed>,
    follow_rows: Vec<FollowRecoRow>,
    unfollow_rows: Vec<UnfollowRow>,
    trust_rows: Vec<CallerTrustRow>,
    lens_rows: Vec<LensScoreRow>,
    best_lens: Vec<(u8, u8)>,
    evidence_rows: Vec<SocialEvidenceRow>,
}

impl Default for SocialPlane {
    fn default() -> Self {
        Self::new()
    }
}

impl SocialPlane {
    /// An empty plane. Every estimator refuses in this state.
    #[must_use]
    pub fn new() -> Self {
        SocialPlane {
            trust: SocialTrust::new(),
            support: SocialSupport::new(),
            follow: FollowRecommender::new(),
            followed: FollowSet::new(),
            witnesses: Vec::new(),
            evidence: BTreeMap::new(),
            support_rows: Vec::new(),
            needs: Vec::new(),
            follow_rows: Vec::new(),
            unfollow_rows: Vec::new(),
            trust_rows: Vec::new(),
            lens_rows: Vec::new(),
            best_lens: Vec::new(),
            evidence_rows: Vec::new(),
        }
    }

    /// Set an author's §28 exposure. **Operator-set, never inferred** — nothing
    /// in this crate derives crowding from data, because "how many other people
    /// read this source" is not observable from our own realized outcomes.
    pub fn set_exposure(
        &mut self,
        author_id: u64,
        exposure: SourceExposure,
    ) -> Result<Option<SourceExposure>, TrustError> {
        self.trust.set_exposure(author_id, exposure)
    }

    /// Add `author_id` to the followed set (operator action). Returns whether the
    /// set changed. This records a follow the OPERATOR has made; it performs no
    /// social action of any kind (§110).
    pub fn follow(&mut self, author_id: u64) -> bool {
        self.followed.follow(author_id).unwrap_or(false)
    }

    /// Remove `author_id` from the followed set (operator action).
    pub fn unfollow(&mut self, author_id: u64) -> bool {
        self.followed.unfollow(author_id)
    }

    /// Whether the operator follows `author_id`.
    #[must_use]
    pub fn is_followed(&self, author_id: u64) -> bool {
        self.followed.contains(author_id)
    }

    /// Record one social call's **evidence class** and, when the brain issued a
    /// call id, bind the post's content digest to it for echo detection.
    ///
    /// This is the only ingestion path, and its argument is a
    /// [`SocialCallEvidence`] whose every field is a provenance fact. There is no
    /// overload, no default, and no constructor that takes a bare social score
    /// (§29.8) — an anonymous scalar simply cannot be expressed here.
    pub fn record_call(&mut self, ev: SocialCallEvidence) {
        let SocialCallEvidence {
            mint_id,
            author_id,
            platform,
            designated,
            now_tick,
            call_id,
            content_digest,
        } = ev;
        let key = (mint_id, author_id);
        match self.evidence.get_mut(&key) {
            Some(row) => {
                row.calls = row.calls.saturating_add(1);
                row.last_tick = row.last_tick.max(now_tick);
                row.designated |= designated;
            }
            None => {
                if self.evidence.len() >= SOCIAL_EVIDENCE_CAP {
                    if let Some(&victim) = self.evidence.keys().next() {
                        self.evidence.remove(&victim);
                    }
                }
                self.evidence.insert(
                    key,
                    SocialEvidenceRow {
                        mint_id,
                        author_id,
                        platform_code: platform.ordinal(),
                        designated,
                        calls: 1,
                        first_tick: now_tick,
                        last_tick: now_tick,
                        trust_tier_code: TrustTier::Unproven.ordinal(),
                        exposure_code: SourceExposure::Niche.ordinal(),
                    },
                );
            }
        }
        if let Some(id) = call_id {
            if self.witnesses.len() >= ECHO_WITNESS_CAP {
                self.witnesses.remove(0);
            }
            self.witnesses.push(ContentEchoWitness {
                call_id: id,
                content_digest,
            });
        }
    }

    /// Number of retained provenance rows (bounded, §99).
    #[must_use]
    pub fn evidence_len(&self) -> usize {
        self.evidence.len()
    }

    /// The provenance row for one (mint, author) pair, if it is still retained.
    #[must_use]
    pub fn evidence_of(&self, mint_id: u64, author_id: u64) -> Option<SocialEvidenceRow> {
        self.evidence.get(&(mint_id, author_id)).copied()
    }

    /// The [`Platform::ordinal`] carrying most of this author's retained calls, or
    /// `None` when we hold no fresh provenance for them at all.
    ///
    /// Ties break to the LOWEST platform ordinal — an arbitrary but total rule, so
    /// the answer is a pure function of the ledger and never of iteration order
    /// (§22). `None` is the honest answer for an author whose evidence went stale:
    /// the export emits `null` rather than inventing a platform.
    #[must_use]
    pub fn dominant_platform_of(&self, author_id: u64) -> Option<u8> {
        // Platform ordinals are dense and small; a fixed array is bounded (§99)
        // and needs no allocation.
        let mut calls = [0u64; 8];
        let mut any = false;
        for row in self.evidence.values() {
            if row.author_id != author_id {
                continue;
            }
            let idx = usize::from(row.platform_code).min(calls.len() - 1);
            calls[idx] = calls[idx].saturating_add(u64::from(row.calls));
            any = true;
        }
        if !any {
            return None;
        }
        let mut best = 0usize;
        for (i, c) in calls.iter().enumerate() {
            if *c > calls[best] {
                best = i;
            }
        }
        // `best` indexes a dense ordinal by construction.
        u8::try_from(best).ok()
    }

    /// The live trust verdict for one author (inspection seam).
    #[must_use]
    pub fn author_trust(
        &self,
        social: &SocialRecallIndex,
        author_id: u64,
        as_of_ns: u64,
    ) -> TrustVerdict {
        self.trust.author_trust(social, author_id, as_of_ns)
    }

    /// The live support verdict for one mint (inspection seam).
    #[must_use]
    pub fn support_of(
        &self,
        social: &SocialRecallIndex,
        mint_id: u64,
        as_of_ns: u64,
    ) -> SocialSupportVerdict {
        let snap = self.trust.snapshot(social, as_of_ns);
        self.support.evaluate_with_content(
            social,
            &self.trust,
            &snap,
            mint_id,
            as_of_ns,
            &self.witnesses,
        )
    }

    // ---- readouts --------------------------------------------------------

    /// Cached social-support rows for the watched mints.
    #[must_use]
    pub fn support_rows(&self) -> Vec<SocialSupportRow> {
        self.support_rows.clone()
    }
    /// Cached capture work list (§ Phase-B).
    #[must_use]
    pub fn needs(&self) -> Vec<SupportNeed> {
        self.needs.clone()
    }
    /// Cached follow recommendations.
    #[must_use]
    pub fn follow_rows(&self) -> Vec<FollowRecoRow> {
        self.follow_rows.clone()
    }
    /// Cached unfollow candidates.
    #[must_use]
    pub fn unfollow_rows(&self) -> Vec<UnfollowRow> {
        self.unfollow_rows.clone()
    }
    /// Cached trust standing of the callers we are acting on.
    #[must_use]
    pub fn trust_rows(&self) -> Vec<CallerTrustRow> {
        self.trust_rows.clone()
    }
    /// Cached per-lens realized scoreboard, phase-separated (§100).
    #[must_use]
    pub fn lens_rows(&self) -> Vec<LensScoreRow> {
        self.lens_rows.clone()
    }
    /// Cached best-paying lens per venue phase: `(venue_phase_code, lens_code)`.
    #[must_use]
    pub fn best_paying_lens(&self) -> Vec<(u8, u8)> {
        self.best_lens.clone()
    }
    /// Cached provenance readout — the FRESH social evidence chain (§34.3).
    #[must_use]
    pub fn evidence_rows(&self) -> Vec<SocialEvidenceRow> {
        self.evidence_rows.clone()
    }

    /// Rebuild every cached readout. Called at the reflection cadence.
    ///
    /// `watched` is the set of mint ids currently on the watchlist, in the
    /// engine's own order; `ttl_ticks` is the engine's evidence TTL (§34.3).
    ///
    /// **Staleness law.** Evidence whose most recent call is older than
    /// `ttl_ticks` is REMOVED from the ledger here — not merely hidden. A stale
    /// social input therefore degrades to "no evidence" and can never be carried
    /// forward at its last value, which is exactly the failure mode §34.3/§29.6
    /// exist to prevent.
    pub fn refresh(
        &mut self,
        index: &EpisodicIndex,
        social: &SocialRecallIndex,
        at: RefreshAt<'_>,
    ) {
        let RefreshAt {
            watched,
            now_tick,
            as_of_ns,
            ttl_ticks,
            min_sample,
        } = at;
        // 1. §34.3 staleness sweep — drop, never decay-in-place.
        self.evidence
            .retain(|_, row| row.is_fresh(now_tick, ttl_ticks));

        let snap = self.trust.snapshot(social, as_of_ns);

        // 2. Refresh each surviving row's earned tier + operator exposure, so the
        //    provenance chain always carries a CURRENT evidence class.
        for row in self.evidence.values_mut() {
            let tier = self.trust.trust_from_snapshot(&snap, row.author_id).tier();
            row.trust_tier_code = tier.ordinal();
            row.exposure_code = self.trust.exposure_of(row.author_id).ordinal();
        }

        // 3. Provenance readout: freshest first, then mint, then author (total).
        let mut ev: Vec<SocialEvidenceRow> = self.evidence.values().copied().collect();
        ev.sort_by(|a, b| {
            b.last_tick
                .cmp(&a.last_tick)
                .then(a.mint_id.cmp(&b.mint_id))
                .then(a.author_id.cmp(&b.author_id))
        });
        ev.truncate(EVIDENCE_ROW_CAP);
        self.evidence_rows = ev;

        // 4. Support verdicts + capture needs for the watched mints that still
        //    have FRESH evidence. A mint whose social evidence went stale simply
        //    disappears from the readout.
        let mut rows: Vec<SocialSupportRow> = Vec::new();
        let mut needs: Vec<SupportNeed> = Vec::new();
        let mut seen: Vec<u64> = Vec::new();
        for &mint_id in watched {
            if seen.contains(&mint_id) {
                continue;
            }
            seen.push(mint_id);
            let Some(freshest) = self.freshest_tick_for(mint_id) else {
                continue;
            };
            let verdict = self.support.evaluate_with_content(
                social,
                &self.trust,
                &snap,
                mint_id,
                as_of_ns,
                &self.witnesses,
            );
            rows.push(SocialSupportRow {
                mint_id,
                freshest_tick: freshest,
                verdict: support_row_of(&verdict),
            });
            for need in self.support.support_inputs_needed(
                social,
                &self.trust,
                &snap,
                mint_id,
                as_of_ns,
                &self.witnesses,
            ) {
                needs.push(need_row_of(mint_id, need));
            }
        }
        // Strongest support first, then freshest, then mint id — a total order.
        rows.sort_by(|a, b| {
            score_of(&b.verdict)
                .cmp(&score_of(&a.verdict))
                .then(b.freshest_tick.cmp(&a.freshest_tick))
                .then(a.mint_id.cmp(&b.mint_id))
        });
        rows.truncate(SUPPORT_ROW_CAP);
        self.support_rows = rows;
        needs.truncate(SUPPORT_NEED_CAP);
        self.needs = needs;

        // 5. Follow / unfollow. Research only (§110).
        self.follow_rows = self
            .follow
            .recommend_follows(index, social, &self.trust, &snap, &self.followed, as_of_ns)
            .recommendations()
            .map(|rs| {
                rs.iter()
                    .take(FOLLOW_ROW_CAP)
                    .map(|r| FollowRecoRow {
                        author_id: r.author_id,
                        platform_code: r.platform.ordinal(),
                        n_calls: r.n_calls,
                        realized_net_attributed_lamports: r.realized_net_attributed_lamports,
                        median_lead_ns: r.median_lead_ns,
                        trust_tier_code: r.trust_tier.ordinal(),
                        confidence_bp: r.confidence_bp,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.unfollow_rows = self
            .follow
            .unfollow_candidates(index, social, &self.trust, &snap, &self.followed, as_of_ns)
            .into_iter()
            .take(FOLLOW_ROW_CAP)
            .map(|r| UnfollowRow {
                author_id: r.author_id,
                platform_code: r.platform.ordinal(),
                n_calls: r.n_calls,
                realized_net_attributed_lamports: r.realized_net_attributed_lamports,
                median_lead_ns: r.median_lead_ns,
                trust_tier_code: r.trust_tier.ordinal(),
            })
            .collect();

        // 6. Trust standing of the callers we are ACTING ON (the authors behind
        //    the surviving fresh evidence), most-called first then author id.
        let mut authors: Vec<(u32, u64)> = Vec::new();
        for row in self.evidence.values() {
            match authors.iter_mut().find(|(_, a)| *a == row.author_id) {
                Some(e) => e.0 = e.0.saturating_add(row.calls),
                None => authors.push((row.calls, row.author_id)),
            }
        }
        authors.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        authors.truncate(TRUST_ROW_CAP);
        self.trust_rows = authors
            .into_iter()
            .map(|(_, author_id)| CallerTrustRow {
                author_id,
                exposure_code: self.trust.exposure_of(author_id).ordinal(),
                verdict: trust_row_of(&self.trust.trust_from_snapshot(&snap, author_id)),
            })
            .collect();

        // 7. Style-lens scoreboard, PHASE-SEPARATED (§100 — there is no
        //    phase-pooled statistic, by design).
        let floor = min_sample.max(ARCHETYPE_MIN_SAMPLE);
        let mut lens_rows: Vec<LensScoreRow> = Vec::new();
        let mut best: Vec<(u8, u8)> = Vec::new();
        for phase in [VenuePhase::Curve, VenuePhase::Pool] {
            for lens in STYLE_LENSES {
                lens_rows.push(LensScoreRow {
                    lens_code: lens.ordinal(),
                    venue_phase_code: phase.ordinal(),
                    verdict: lens_row_of(&archetype_performance(index, lens, phase, floor)),
                });
            }
            if let Some((lens, _)) = best_paying_lens(index, phase, floor) {
                best.push((phase.ordinal(), lens.ordinal()));
            }
        }
        self.lens_rows = lens_rows;
        self.best_lens = best;
    }

    /// Freshest engine tick at which any author called `mint_id`, over the rows
    /// that survived the staleness sweep.
    fn freshest_tick_for(&self, mint_id: u64) -> Option<u64> {
        self.evidence
            .range((mint_id, 0)..=(mint_id, u64::MAX))
            .map(|(_, r)| r.last_tick)
            .max()
    }
}

// ---------------------------------------------------------------------------
// Pure verdict crosswalks
// ---------------------------------------------------------------------------

/// Sort key for support rows: the score when known, `0` when refused. Refusals
/// therefore sink to the bottom rather than being ranked as if they were zero
/// support — they are simply last, and their variant still says why.
const fn score_of(v: &SupportVerdictRow) -> u32 {
    match v {
        SupportVerdictRow::Known {
            support_score_bp, ..
        } => *support_score_bp,
        SupportVerdictRow::Unknown(_) => 0,
    }
}

/// Crosswalk a brain support verdict onto its report row.
#[must_use]
pub fn support_row_of(v: &SocialSupportVerdict) -> SupportVerdictRow {
    match v {
        SocialSupportVerdict::Known(s) => SupportVerdictRow::Known {
            support_score_bp: s.support_score_bp,
            n_effective_originators: s.n_effective_originators,
            n_trusted_originators: s.n_trusted_originators,
            distinct_platforms: s.distinct_platforms,
            coordination_penalty_bp: s.coordination_penalty_bp,
            duplicate_share_bp: s.duplicate_share_bp,
            velocity_bp: s.velocity_bp,
            trend_code: s.trend.ordinal(),
            content_evidence: s.content_evidence,
        },
        SocialSupportVerdict::Unknown(u) => SupportVerdictRow::Unknown(match u {
            SupportUnknown::NoCallsInWindow { .. } => SupportRefusal::NoCallsInWindow,
            SupportUnknown::InsufficientOriginators {
                n_originators,
                min_originators,
            } => SupportRefusal::InsufficientOriginators {
                n_originators: *n_originators,
                min_originators: *min_originators,
            },
            SupportUnknown::NoTrustedOriginator { n_originators } => {
                SupportRefusal::NoTrustedOriginator {
                    n_originators: *n_originators,
                }
            }
        }),
    }
}

/// Crosswalk one brain capture need onto its report row.
#[must_use]
pub const fn need_row_of(mint_id: u64, need: SupportInputNeed) -> SupportNeed {
    match need {
        SupportInputNeed::MoreOriginators {
            n_originators,
            min_originators,
        } => SupportNeed::MoreOriginators {
            mint_id,
            n_originators,
            min_originators,
        },
        SupportInputNeed::ContentDigests { n_calls } => {
            SupportNeed::ContentDigests { mint_id, n_calls }
        }
        SupportInputNeed::PlatformCoverage { platform } => SupportNeed::PlatformCoverage {
            mint_id,
            platform_code: platform.ordinal(),
        },
        SupportInputNeed::AuthorTrackRecord { author_id } => {
            SupportNeed::AuthorTrackRecord { mint_id, author_id }
        }
        SupportInputNeed::SourceExposure { author_id } => {
            SupportNeed::SourceExposure { mint_id, author_id }
        }
    }
}

/// Crosswalk a brain trust verdict onto its report row. `Unknown` carries no
/// score by construction, and the row mirrors that: the score fields do not exist
/// on the Unknown variant.
#[must_use]
pub fn trust_row_of(v: &TrustVerdict) -> TrustVerdictRow {
    match v.score() {
        Some(s) => TrustVerdictRow::Known {
            trust_score_bp: s.trust_score_bp,
            n_markouts: s.n_markouts,
            effective_weight_units: s.effective_weight_units,
            tier_code: s.tier.ordinal(),
        },
        None => TrustVerdictRow::Unknown {
            tier_code: v.tier().ordinal(),
        },
    }
}

/// Crosswalk a lens recall verdict onto its report row.
#[must_use]
pub fn lens_row_of(v: &RecallVerdict) -> LensVerdictRow {
    match v.stats() {
        Some(s) => LensVerdictRow::Known {
            n_matched: s.n_matched,
            median_net_lamports: s.median_net_lamports,
            mean_net_lamports: s.mean_net_lamports,
            win_rate_bp: s.win_rate_bp,
            median_hold_ns: s.median_hold_ns,
        },
        None => LensVerdictRow::Unknown,
    }
}

/// The stable machine name of a lens ordinal, for operator listings.
#[must_use]
pub fn lens_name(lens_code: u8) -> &'static str {
    StyleLens::from_ordinal(lens_code).map_or("unknown", StyleLens::name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(watched: &[u64], now_tick: u64) -> RefreshAt<'_> {
        RefreshAt {
            watched,
            now_tick,
            as_of_ns: 0,
            ttl_ticks: 100,
            min_sample: 8,
        }
    }

    fn plane_with_call(tick: u64) -> SocialPlane {
        let mut p = SocialPlane::new();
        p.record_call(SocialCallEvidence {
            mint_id: 11,
            author_id: 22,
            platform: Platform::X,
            designated: true,
            now_tick: tick,
            call_id: Some(1),
            content_digest: 0xDEAD,
        });
        p
    }

    #[test]
    fn every_ingested_datum_carries_its_evidence_class() {
        let p = plane_with_call(5);
        let row = p.evidence_of(11, 22).expect("provenance recorded");
        assert_eq!(row.platform_code, Platform::X.ordinal());
        assert_eq!(row.author_id, 22);
        assert!(row.designated);
        assert_eq!(row.last_tick, 5);
        // Trust is UNPROVEN until realized net SOL earns it — never assumed.
        assert_eq!(row.trust_tier_code, TrustTier::Unproven.ordinal());
    }

    #[test]
    fn stale_evidence_is_dropped_not_carried_forward() {
        let mut p = plane_with_call(5);
        let index = EpisodicIndex::new();
        let social = SocialRecallIndex::new();
        // Inside the TTL the row survives.
        p.refresh(&index, &social, at(&[11], 10));
        assert_eq!(p.evidence_len(), 1);
        assert_eq!(p.evidence_rows().len(), 1);
        // Past the TTL it is REMOVED — not decayed, not held at its last value.
        p.refresh(&index, &social, at(&[11], 500));
        assert_eq!(p.evidence_len(), 0, "stale social evidence must be DROPPED");
        assert!(p.evidence_rows().is_empty());
        assert!(
            p.support_rows().is_empty(),
            "a mint whose evidence went stale carries no support verdict at all"
        );
    }

    #[test]
    fn an_empty_plane_refuses_every_estimator() {
        let mut p = SocialPlane::new();
        let index = EpisodicIndex::new();
        let social = SocialRecallIndex::new();
        p.refresh(&index, &social, at(&[], 1));
        assert!(p.support_rows().is_empty());
        assert!(p.follow_rows().is_empty());
        assert!(p.unfollow_rows().is_empty());
        assert!(p.trust_rows().is_empty());
        assert!(p.best_paying_lens().is_empty());
        // The lens scoreboard still emits one row per (phase, lens) — each an
        // explicit Unknown, which is the honest answer, not an omission.
        assert_eq!(p.lens_rows().len(), 2 * STYLE_LENSES.len());
        assert!(p
            .lens_rows()
            .iter()
            .all(|r| r.verdict == LensVerdictRow::Unknown));
    }

    #[test]
    fn exposure_is_operator_set_and_demotes() {
        let mut p = SocialPlane::new();
        p.set_exposure(22, SourceExposure::PublicBurned).unwrap();
        p.record_call(SocialCallEvidence {
            mint_id: 11,
            author_id: 22,
            platform: Platform::X,
            designated: false,
            now_tick: 1,
            call_id: Some(1),
            content_digest: 7,
        });
        let index = EpisodicIndex::new();
        let social = SocialRecallIndex::new();
        p.refresh(&index, &social, at(&[11], 2));
        let row = p.evidence_of(11, 22).unwrap();
        assert_eq!(row.exposure_code, SourceExposure::PublicBurned.ordinal());
    }

    #[test]
    fn the_followed_set_is_an_operator_record_not_an_action() {
        let mut p = SocialPlane::new();
        assert!(p.follow(42));
        assert!(p.is_followed(42));
        assert!(p.unfollow(42));
        assert!(!p.is_followed(42));
    }
}
