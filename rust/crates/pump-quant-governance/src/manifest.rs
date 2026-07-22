//! Infrastructure manifest — the versioned sibling of the source registry
//! (constitution §13, §18.9, §43, §62).
//!
//! ## Responsibility
//! `pq-governance hosts source registry + infrastructure manifest` (§13). The
//! source-registry half ([`crate::lifecycle`]) models *what we observe*; this
//! module models *what we pay for and provision*: the concrete
//! provider/product/plan capabilities the bot depends on — a Helius developer
//! plan on `mainnet-beta`, a Jito block-engine region, a canonical-RPC
//! historical plan — each with its integer cost, credit allotment, rate limits,
//! regional endpoints, auth model and replay window (§18.9).
//!
//! Like the source registry it is *versioned* (§43 `infrastructure_manifest`
//! table, §59 manifest-versioning tests, §62 M0 initialization): a manifest
//! carries a monotone [`ManifestEntry::version`], and every revision pushes the
//! prior capability snapshot into a bounded revision history that mirrors
//! [`crate::lifecycle::SourceEntry`]'s transition log. A manifest may also be
//! *superseded* — a one-way terminal move recording which manifest took over its
//! role (the §18.9 analogue of the source registry's replacement pointer).
//!
//! ## §22 / §57 / §705 compliance
//! No floating point and no wall-clock. Every quantity is integer / fixed-point:
//! cost is an `i128` in a caller-chosen scale (micro-USD, lamports), credits and
//! rate limits and the replay window are `u64`, and the "verified date" is a
//! caller-supplied monotone `u64` sequence (never a clock read, §22). Provider /
//! product / plan / region / doc references are interned `u32` identifiers,
//! keeping the manifest deterministic and dependency-free. The revision history
//! is a fixed-capacity ring buffer that evicts oldest entries (§57).

/// A stable identifier for a registered infrastructure manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ManifestId(pub u32);

/// An interned provider identifier (e.g. Helius, Jito, Triton).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(pub u32);

/// An interned product identifier within a provider (LaserStream, ShredStream,
/// canonical RPC, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductId(pub u32);

/// An interned billing/plan-tier identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanId(pub u32);

/// An interned regional endpoint identifier (§18.9 regional endpoints).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionId(pub u32);

/// An interned documentation reference (§18.9 doc reference — a pointer into the
/// sealed doc set, not free text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocRef(pub u32);

/// The Solana network a capability is provisioned against (§18.9 network).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Network {
    /// Production mainnet-beta.
    MainnetBeta,
    /// Public devnet.
    Devnet,
    /// Public testnet.
    Testnet,
}

/// The authentication model a capability uses (§18.9 auth model).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuthModel {
    /// API key passed as a URL query parameter.
    ApiKeyQuery,
    /// API key passed in a request header.
    ApiKeyHeader,
    /// A bearer token in the `Authorization` header.
    BearerToken,
    /// A signed JWT.
    Jwt,
    /// Mutual TLS with a client certificate.
    MutualTls,
}

/// An integer rate limit: `requests` permitted per `per_seconds` window
/// (§18.9 rate limits). No floating point (§22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    /// Requests permitted per window.
    pub requests: u64,
    /// Window length in seconds. Must be non-zero (validated at construction).
    pub per_seconds: u64,
}

/// Maximum regional endpoints a single capability may enumerate (§57 bound).
pub const MAX_REGIONS: usize = 16;

/// Errors constructing a [`Capability`] or transitioning an
/// [`InfrastructureManifest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// More than [`MAX_REGIONS`] regional endpoints were supplied (§57).
    TooManyRegions,
    /// A [`RateLimit`] with a zero-length window was supplied (meaningless).
    ZeroRateWindow,
    /// A revision or supersede was attempted on an already-superseded manifest
    /// (terminal; §18.9 supersede is one-way).
    AlreadySuperseded,
    /// The supplied verified-date sequence is older than the current entry's —
    /// a manifest's verification never travels backward (§22 monotone ordering).
    VerifiedSequenceRegressed,
    /// The monotone version counter would overflow `u32`.
    VersionOverflow,
}

/// The concrete §18.9 capability fields of one infrastructure dependency.
///
/// Immutable once constructed; a new capability is installed via
/// [`InfrastructureManifest::revise`], which snapshots the prior one into the
/// revision history. Regional endpoints are normalized (sorted, de-duplicated)
/// so byte-equivalent inputs yield an identical capability regardless of caller
/// ordering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    provider: ProviderId,
    product: ProductId,
    network: Network,
    plan: PlanId,
    monthly_cost: i128,
    credits: u64,
    rate_limit: RateLimit,
    regions: Vec<RegionId>,
    auth: AuthModel,
    replay_window: u64,
    doc: DocRef,
}

impl Capability {
    /// Construct a capability, validating the region bound and rate window and
    /// normalizing the region list (§57 / §22).
    ///
    /// `monthly_cost` is fixed-point in a caller-chosen scale (micro-USD,
    /// lamports); `credits` and `replay_window` are integer counts.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: ProviderId,
        product: ProductId,
        network: Network,
        plan: PlanId,
        monthly_cost: i128,
        credits: u64,
        rate_limit: RateLimit,
        regions: &[RegionId],
        auth: AuthModel,
        replay_window: u64,
        doc: DocRef,
    ) -> Result<Self, ManifestError> {
        if regions.len() > MAX_REGIONS {
            return Err(ManifestError::TooManyRegions);
        }
        if rate_limit.per_seconds == 0 {
            return Err(ManifestError::ZeroRateWindow);
        }
        // Normalize for a deterministic capability identity (§22): sort + dedup.
        let mut normalized: Vec<RegionId> = regions.to_vec();
        normalized.sort();
        normalized.dedup();
        Ok(Self {
            provider,
            product,
            network,
            plan,
            monthly_cost,
            credits,
            rate_limit,
            regions: normalized,
            auth,
            replay_window,
            doc,
        })
    }

    /// The provider identifier.
    pub fn provider(&self) -> ProviderId {
        self.provider
    }
    /// The product identifier.
    pub fn product(&self) -> ProductId {
        self.product
    }
    /// The provisioned network.
    pub fn network(&self) -> Network {
        self.network
    }
    /// The billing/plan-tier identifier.
    pub fn plan(&self) -> PlanId {
        self.plan
    }
    /// The fixed-point monthly cost (caller-chosen scale).
    pub fn monthly_cost(&self) -> i128 {
        self.monthly_cost
    }
    /// The credit allotment.
    pub fn credits(&self) -> u64 {
        self.credits
    }
    /// The rate limit.
    pub fn rate_limit(&self) -> RateLimit {
        self.rate_limit
    }
    /// The normalized regional endpoints (sorted, de-duplicated).
    pub fn regions(&self) -> &[RegionId] {
        &self.regions
    }
    /// The auth model.
    pub fn auth(&self) -> AuthModel {
        self.auth
    }
    /// The replay window (integer, caller-chosen unit — slots or events).
    pub fn replay_window(&self) -> u64 {
        self.replay_window
    }
    /// The documentation reference.
    pub fn doc(&self) -> DocRef {
        self.doc
    }

    /// Integer count of requests permitted over `window_seconds`, floored.
    ///
    /// Pure integer arithmetic (§22): `requests * window_seconds / per_seconds`,
    /// saturating on overflow (§705). `per_seconds` is guaranteed non-zero by
    /// construction, so this never divides by zero.
    pub fn requests_over(&self, window_seconds: u64) -> u64 {
        self.rate_limit.requests.saturating_mul(window_seconds) / self.rate_limit.per_seconds
    }
}

/// One versioned snapshot of a manifest's capability (a revision-history
/// element, mirroring [`crate::lifecycle::TransitionRecord`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// The monotone version this snapshot was recorded at.
    pub version: u32,
    /// The capability fields at this version.
    pub capability: Capability,
    /// The caller-supplied monotone verified-date sequence for this version
    /// (§18.9 verified-date; never a wall-clock read, §22).
    pub verified_seq: u64,
}

/// The lifecycle status of a manifest (§18.9). `Superseded` is terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManifestStatus {
    /// The manifest is the current record for its role.
    Active,
    /// A newer manifest has taken over this role; terminal (§18.9 one-way).
    Superseded,
}

impl ManifestStatus {
    /// Whether this status admits no further transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(self, ManifestStatus::Superseded)
    }
}

/// A registered infrastructure manifest: its current versioned capability, its
/// status, an optional supersede pointer, and a bounded revision history.
///
/// ## Constitution §18.9 / §43 / §57
/// Models one `infrastructure_manifest` row. `version` is monotone and increases
/// by exactly one per [`revise`](InfrastructureManifest::revise). The revision
/// `history` is a fixed-capacity ring buffer (§57): once full, the oldest prior
/// version is overwritten.
#[derive(Clone, Debug)]
pub struct InfrastructureManifest {
    id: ManifestId,
    current: ManifestEntry,
    status: ManifestStatus,
    superseded_by: Option<ManifestId>,
    history: Vec<ManifestEntry>,
    history_capacity: usize,
    history_head: usize,
}

impl InfrastructureManifest {
    /// The version a freshly registered manifest starts at.
    pub const INITIAL_VERSION: u32 = 1;

    /// Register a new manifest at [`INITIAL_VERSION`](Self::INITIAL_VERSION).
    ///
    /// `history_capacity` bounds the revision history (§57); it is raised to a
    /// minimum of 1 so at least one prior version is always retained.
    pub fn new(
        id: ManifestId,
        capability: Capability,
        verified_seq: u64,
        history_capacity: usize,
    ) -> Self {
        let cap = history_capacity.max(1);
        Self {
            id,
            current: ManifestEntry {
                version: Self::INITIAL_VERSION,
                capability,
                verified_seq,
            },
            status: ManifestStatus::Active,
            superseded_by: None,
            history: Vec::with_capacity(cap),
            history_capacity: cap,
            history_head: 0,
        }
    }

    /// The manifest's stable id.
    pub fn id(&self) -> ManifestId {
        self.id
    }

    /// The current monotone version.
    pub fn version(&self) -> u32 {
        self.current.version
    }

    /// The current versioned entry.
    pub fn current(&self) -> &ManifestEntry {
        &self.current
    }

    /// The current capability fields.
    pub fn capability(&self) -> &Capability {
        &self.current.capability
    }

    /// The current status.
    pub fn status(&self) -> ManifestStatus {
        self.status
    }

    /// The manifest that superseded this one, if any (§18.9 supersede pointer).
    pub fn superseded_by(&self) -> Option<ManifestId> {
        self.superseded_by
    }

    /// Install a new capability, bumping the version by one and snapshotting the
    /// prior version into the bounded revision history.
    ///
    /// Returns the new version on success. `verified_seq` is a caller-supplied
    /// monotone verified-date sequence and must not regress below the current
    /// entry's (§22 monotone ordering). Refused if the manifest is already
    /// superseded (terminal).
    pub fn revise(
        &mut self,
        capability: Capability,
        verified_seq: u64,
    ) -> Result<u32, ManifestError> {
        if self.status.is_terminal() {
            return Err(ManifestError::AlreadySuperseded);
        }
        if verified_seq < self.current.verified_seq {
            return Err(ManifestError::VerifiedSequenceRegressed);
        }
        let new_version = self
            .current
            .version
            .checked_add(1)
            .ok_or(ManifestError::VersionOverflow)?;
        let superseded_entry = core::mem::replace(
            &mut self.current,
            ManifestEntry {
                version: new_version,
                capability,
                verified_seq,
            },
        );
        self.push_history(superseded_entry);
        Ok(new_version)
    }

    /// Supersede this manifest, recording which manifest took over its role
    /// (§18.9). One-way and terminal: the status becomes
    /// [`ManifestStatus::Superseded`] and no further revisions are legal.
    ///
    /// `verified_seq` must not regress (§22). Refused if already superseded.
    pub fn supersede_with(
        &mut self,
        replacement: ManifestId,
        verified_seq: u64,
    ) -> Result<(), ManifestError> {
        if self.status.is_terminal() {
            return Err(ManifestError::AlreadySuperseded);
        }
        if verified_seq < self.current.verified_seq {
            return Err(ManifestError::VerifiedSequenceRegressed);
        }
        self.status = ManifestStatus::Superseded;
        self.superseded_by = Some(replacement);
        Ok(())
    }

    /// Number of prior versions retained in the bounded revision history.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// The retained revision history in chronological (oldest-first) order.
    ///
    /// Bounded to `history_capacity` entries (§57); older revisions beyond the
    /// bound have been evicted.
    pub fn history(&self) -> Vec<ManifestEntry> {
        let n = self.history.len();
        let mut out = Vec::with_capacity(n);
        for offset in 0..n {
            out.push(self.history[(self.history_head + offset) % n].clone());
        }
        out
    }

    /// Push a prior version into the bounded ring buffer, evicting the oldest
    /// when full (§57 no-unbounded-growth).
    fn push_history(&mut self, entry: ManifestEntry) {
        if self.history.len() < self.history_capacity {
            self.history.push(entry);
        } else {
            self.history[self.history_head] = entry;
            self.history_head = (self.history_head + 1) % self.history_capacity;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rl(requests: u64, per_seconds: u64) -> RateLimit {
        RateLimit {
            requests,
            per_seconds,
        }
    }

    fn cap(cost: i128, credits: u64, regions: &[RegionId]) -> Capability {
        Capability::new(
            ProviderId(1),
            ProductId(2),
            Network::MainnetBeta,
            PlanId(3),
            cost,
            credits,
            rl(100, 10),
            regions,
            AuthModel::ApiKeyHeader,
            512,
            DocRef(9),
        )
        .expect("valid capability")
    }

    #[test]
    fn capability_construction_and_accessors() {
        let c = cap(1_000_000, 5_000, &[RegionId(4)]);
        assert_eq!(c.provider(), ProviderId(1));
        assert_eq!(c.product(), ProductId(2));
        assert_eq!(c.network(), Network::MainnetBeta);
        assert_eq!(c.plan(), PlanId(3));
        assert_eq!(c.monthly_cost(), 1_000_000);
        assert_eq!(c.credits(), 5_000);
        assert_eq!(c.rate_limit(), rl(100, 10));
        assert_eq!(c.auth(), AuthModel::ApiKeyHeader);
        assert_eq!(c.replay_window(), 512);
        assert_eq!(c.doc(), DocRef(9));
        assert_eq!(c.regions(), &[RegionId(4)]);
    }

    #[test]
    fn regions_are_normalized_sorted_and_deduped() {
        let c = cap(0, 0, &[RegionId(7), RegionId(3), RegionId(7), RegionId(1)]);
        assert_eq!(c.regions(), &[RegionId(1), RegionId(3), RegionId(7)]);
    }

    #[test]
    fn too_many_regions_rejected() {
        let regions: Vec<RegionId> = (0..(MAX_REGIONS as u32 + 1)).map(RegionId).collect();
        let err = Capability::new(
            ProviderId(1),
            ProductId(2),
            Network::MainnetBeta,
            PlanId(3),
            0,
            0,
            rl(1, 1),
            &regions,
            AuthModel::Jwt,
            0,
            DocRef(0),
        )
        .unwrap_err();
        assert_eq!(err, ManifestError::TooManyRegions);
    }

    #[test]
    fn max_regions_exactly_is_accepted() {
        let regions: Vec<RegionId> = (0..MAX_REGIONS as u32).map(RegionId).collect();
        let c = Capability::new(
            ProviderId(1),
            ProductId(2),
            Network::MainnetBeta,
            PlanId(3),
            0,
            0,
            rl(1, 1),
            &regions,
            AuthModel::Jwt,
            0,
            DocRef(0),
        )
        .expect("exactly MAX_REGIONS is legal");
        assert_eq!(c.regions().len(), MAX_REGIONS);
    }

    #[test]
    fn zero_rate_window_rejected() {
        let err = Capability::new(
            ProviderId(1),
            ProductId(2),
            Network::Devnet,
            PlanId(3),
            0,
            0,
            rl(100, 0),
            &[],
            AuthModel::BearerToken,
            0,
            DocRef(0),
        )
        .unwrap_err();
        assert_eq!(err, ManifestError::ZeroRateWindow);
    }

    #[test]
    fn requests_over_is_integer_floored_and_saturating() {
        // 100 requests / 10s window.
        let c = cap(0, 0, &[]);
        assert_eq!(c.requests_over(10), 100);
        assert_eq!(c.requests_over(60), 600);
        // 5s window -> 100*5/10 = 50.
        assert_eq!(c.requests_over(5), 50);
        // Flooring: 100*1/10 = 10.
        assert_eq!(c.requests_over(1), 10);
        // 100*7/10 = 70 (integer floor of 70.0), 100*3/10 = 30.
        assert_eq!(c.requests_over(3), 30);
        // Saturation on multiply overflow rather than panic.
        let big = Capability::new(
            ProviderId(1),
            ProductId(2),
            Network::MainnetBeta,
            PlanId(3),
            0,
            0,
            rl(u64::MAX, 1),
            &[],
            AuthModel::ApiKeyQuery,
            0,
            DocRef(0),
        )
        .unwrap();
        assert_eq!(big.requests_over(u64::MAX), u64::MAX);
    }

    #[test]
    fn manifest_starts_at_initial_version_active() {
        let m = InfrastructureManifest::new(ManifestId(1), cap(10, 10, &[]), 100, 4);
        assert_eq!(m.id(), ManifestId(1));
        assert_eq!(m.version(), InfrastructureManifest::INITIAL_VERSION);
        assert_eq!(m.version(), 1);
        assert_eq!(m.status(), ManifestStatus::Active);
        assert_eq!(m.superseded_by(), None);
        assert_eq!(m.history_len(), 0);
        assert_eq!(m.current().verified_seq, 100);
    }

    #[test]
    fn revise_bumps_version_monotonically_and_records_history() {
        let mut m = InfrastructureManifest::new(ManifestId(1), cap(10, 10, &[]), 100, 8);
        let v2 = m.revise(cap(20, 20, &[]), 200).unwrap();
        assert_eq!(v2, 2);
        assert_eq!(m.version(), 2);
        assert_eq!(m.capability().monthly_cost(), 20);
        let v3 = m.revise(cap(30, 30, &[]), 300).unwrap();
        assert_eq!(v3, 3);

        let hist = m.history();
        assert_eq!(hist.len(), 2);
        // Chronological: v1 then v2.
        assert_eq!(hist[0].version, 1);
        assert_eq!(hist[0].capability.monthly_cost(), 10);
        assert_eq!(hist[0].verified_seq, 100);
        assert_eq!(hist[1].version, 2);
        assert_eq!(hist[1].capability.monthly_cost(), 20);
    }

    #[test]
    fn revise_rejects_regressed_verified_sequence() {
        let mut m = InfrastructureManifest::new(ManifestId(1), cap(10, 10, &[]), 500, 4);
        let err = m.revise(cap(20, 20, &[]), 499).unwrap_err();
        assert_eq!(err, ManifestError::VerifiedSequenceRegressed);
        // State unchanged.
        assert_eq!(m.version(), 1);
        assert_eq!(m.history_len(), 0);
        // Equal sequence is allowed (a same-instant re-verification).
        assert_eq!(m.revise(cap(20, 20, &[]), 500).unwrap(), 2);
    }

    #[test]
    fn history_ring_buffer_is_bounded_and_evicts_oldest() {
        let mut m = InfrastructureManifest::new(ManifestId(1), cap(0, 0, &[]), 0, 2);
        // Push four revisions; capacity 2 keeps only the two most recent priors.
        for i in 1..=4u64 {
            m.revise(cap(i as i128 * 10, 0, &[]), i * 100).unwrap();
        }
        assert_eq!(m.version(), 5);
        assert_eq!(m.history_len(), 2);
        let hist = m.history();
        // Priors retained are versions 3 and 4 (v1, v2 evicted).
        assert_eq!(hist[0].version, 3);
        assert_eq!(hist[1].version, 4);
    }

    #[test]
    fn history_capacity_raised_to_minimum_one() {
        let mut m = InfrastructureManifest::new(ManifestId(1), cap(0, 0, &[]), 0, 0);
        m.revise(cap(10, 0, &[]), 1).unwrap();
        m.revise(cap(20, 0, &[]), 2).unwrap();
        // Capacity floored to 1: only the most recent prior kept.
        assert_eq!(m.history_len(), 1);
        assert_eq!(m.history()[0].version, 2);
    }

    #[test]
    fn supersede_is_terminal_and_records_replacement() {
        let mut m = InfrastructureManifest::new(ManifestId(1), cap(10, 10, &[]), 100, 4);
        m.revise(cap(20, 20, &[]), 200).unwrap();
        m.supersede_with(ManifestId(2), 300).unwrap();
        assert_eq!(m.status(), ManifestStatus::Superseded);
        assert!(m.status().is_terminal());
        assert_eq!(m.superseded_by(), Some(ManifestId(2)));

        // No further revision or supersede is legal.
        assert_eq!(
            m.revise(cap(30, 30, &[]), 400).unwrap_err(),
            ManifestError::AlreadySuperseded
        );
        assert_eq!(
            m.supersede_with(ManifestId(3), 400).unwrap_err(),
            ManifestError::AlreadySuperseded
        );
        // Version pinned at the last active revision.
        assert_eq!(m.version(), 2);
    }

    #[test]
    fn supersede_rejects_regressed_verified_sequence() {
        let mut m = InfrastructureManifest::new(ManifestId(1), cap(10, 10, &[]), 500, 4);
        let err = m.supersede_with(ManifestId(2), 499).unwrap_err();
        assert_eq!(err, ManifestError::VerifiedSequenceRegressed);
        assert_eq!(m.status(), ManifestStatus::Active);
        assert_eq!(m.superseded_by(), None);
    }

    #[test]
    fn manifests_of_different_networks_are_independent() {
        let a = InfrastructureManifest::new(ManifestId(10), cap(100, 0, &[RegionId(1)]), 1, 2);
        let b_cap = Capability::new(
            ProviderId(5),
            ProductId(6),
            Network::Testnet,
            PlanId(7),
            250,
            9,
            rl(50, 5),
            &[RegionId(2), RegionId(2)],
            AuthModel::MutualTls,
            1024,
            DocRef(11),
        )
        .unwrap();
        let b = InfrastructureManifest::new(ManifestId(11), b_cap, 1, 2);
        assert_eq!(a.capability().network(), Network::MainnetBeta);
        assert_eq!(b.capability().network(), Network::Testnet);
        assert_eq!(b.capability().regions(), &[RegionId(2)]);
        assert_eq!(b.capability().auth(), AuthModel::MutualTls);
    }
}
