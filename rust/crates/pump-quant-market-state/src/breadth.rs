//! Manipulation- / cluster-adjusted breadth decomposition reducer.
//!
//! ## Responsibility
//! Reduce a stream of per-trade flow events for a single market into the
//! *decomposed* breadth structure the constitution demands: raw uniqueness
//! counts, entity/cluster-adjusted counts, manipulation-suspect counts, and
//! genuine-net-exposure breadth — **stored separately, never collapsed into one
//! opaque score** (§21.7/§28: "store separately raw unique buyers, unique token
//! accounts, unique fee payers, unique funding roots, cluster-adjusted actors,
//! suspected bundle/sniper/volume-bot/wash/coordinated buyers, repeat buyers,
//! net-new funded buyers, positive-net-inventory buyers,
//! meaningful-net-SOL-exposure buyers, genuine-net-exposure breadth,
//! creator-linked buyers, bundle-linked buyers, known rug-cluster buyers, known
//! runner-cluster buyers, independent buyer expansion, cluster-adjusted breadth
//! decay. Never collapse into one opaque score. Raw wallet count is not organic
//! breadth."). Consumed by §21.2 market-state reconstruction.
//!
//! ## Determinism & bounds
//! Pure integer reducer (§22). All time is carried in the event
//! (`event_index`); nothing reads a clock. Per-wallet and per-cluster state is
//! held in [`BoundedMap`]/[`BoundedSet`] with an explicit capacity (§99).

use crate::common::{ratio_bps, BoundedMap, BoundedSet, Completeness, EntityId};
use crate::macros::bitflags_like;

/// Side of a flow event.
///
/// Constitution: §21.2 (buyer sequence, buy/sell velocity).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// A buy (quote-in, tokens-out).
    Buy,
    /// A sell (tokens-in, quote-out).
    Sell,
}

bitflags_like! {
    /// Manipulation / relationship classification flags attached to a trade by
    /// upstream clustering and creator-attribution (§28 entity dedup, §27
    /// creator recycle, Tier-1 wallet-graph summaries §22).
    ///
    /// These are *inputs* to this reducer: it counts flagged actors separately,
    /// it does not itself decide who is a bundler or a wash trader. A latent
    /// classifier score never substitutes for these explicit, inspectable
    /// counts (§28 clustering-uncertainty law).
    pub struct BuyerFlags: u32 {
        /// Part of a launch bundle (same-block coordinated first buys).
        const BUNDLE          = 0b0000_0001;
        /// Sniper-cohort wallet (Tier-2 first-hour ring).
        const SNIPER          = 0b0000_0010;
        /// Suspected volume-bot (manufactured turnover, no net exposure).
        const VOLUME_BOT      = 0b0000_0100;
        /// Suspected wash trader (round-tripping self-prints).
        const WASH            = 0b0000_1000;
        /// Coordinated same-entity print (rotated maker wallets).
        const COORDINATED     = 0b0001_0000;
        /// Funded from / linked to the creator entity.
        const CREATOR_LINKED  = 0b0010_0000;
        /// Linked to a launch bundle cluster (distinct from being IN the bundle).
        const BUNDLE_LINKED   = 0b0100_0000;
        /// Member of a known historical rug cluster.
        const RUG_CLUSTER     = 0b1000_0000;
        /// Member of a known historical runner cluster.
        const RUNNER_CLUSTER  = 0b1_0000_0000;
    }
}

/// A single decoded flow event feeding the breadth reducer.
///
/// ## Responsibility
/// Carries the entity ids resolved upstream plus the manipulation flags and the
/// quote/token amounts, so the reducer can decompose breadth without any
/// clock, network, or float access (§22). `event_index` is a monotonically
/// non-decreasing sequence number (e.g. decoded-swap ordinal) used for the
/// trailing-window "cluster-adjusted breadth decay" measure — it is
/// caller-supplied time, never wall-clock.
#[derive(Clone, Copy, Debug)]
pub struct FlowEvent {
    /// Monotonic sequence position of this event in the stream.
    pub event_index: u64,
    /// Whether this is a buy or a sell.
    pub side: Side,
    /// Distinct signer wallet.
    pub wallet: EntityId,
    /// Distinct token account (an adversary can hold many per wallet).
    pub token_account: EntityId,
    /// Distinct fee payer (sponsors can pay for many wallets).
    pub fee_payer: EntityId,
    /// Distinct funding root (the wallet that funded this actor).
    pub funding_root: EntityId,
    /// Entity-deduplicated cluster id (§28): one real actor => one cluster id.
    pub cluster: EntityId,
    /// Quote lamports paid (buys) or received (sells). Signed accounting is
    /// applied by the reducer based on [`FlowEvent::side`].
    pub quote_lamports: u64,
    /// Tokens received (buys) or delivered (sells), in base units.
    pub token_base_units: u64,
    /// Whether this actor was funded by a net-new source not previously seen
    /// funding this market (upstream funding-graph determination).
    pub funded_net_new: bool,
    /// Manipulation / relationship flags.
    pub flags: BuyerFlags,
}

/// Per-cluster running accumulator held inside the reducer.
#[derive(Clone, Copy, Debug, Default)]
struct ClusterAgg {
    /// Net token inventory (buys minus sells), base units, saturating.
    net_tokens: i128,
    /// Net quote exposure (buy lamports minus sell lamports), saturating.
    net_quote: i128,
    /// Number of buy events attributed to this cluster (saturating).
    buy_events: u32,
    /// Union of manipulation flags seen for this cluster.
    flags: u32,
    /// Whether any event for this cluster was net-new funded.
    funded_net_new: bool,
    /// First event index at which this cluster placed a buy (for decay).
    first_buy_index: u64,
}

/// Configuration for the breadth reducer.
///
/// Constitution: §102 (no silent magic numbers — every threshold is an explicit,
/// versioned input), §99 (capacity bounds).
#[derive(Clone, Copy, Debug)]
pub struct BreadthConfig {
    /// Minimum net quote exposure (lamports) for a cluster to count toward
    /// "meaningful net-SOL exposure" breadth. Derived/versioned upstream, never
    /// hardcoded in a decision path.
    pub meaningful_net_quote_lamports: u64,
    /// Trailing window (in events) over which "independent buyer expansion" is
    /// measured for the breadth-decay signal.
    pub decay_window_events: u64,
    /// Maximum distinct clusters tracked before the reducer reports INCOMPLETE.
    pub max_tracked_clusters: usize,
    /// Maximum distinct ids tracked per raw-uniqueness dimension.
    pub max_tracked_ids: usize,
}

impl Default for BreadthConfig {
    /// Conservative defaults for tests / bootstrapping. Production supplies
    /// versioned values.
    fn default() -> Self {
        BreadthConfig {
            meaningful_net_quote_lamports: 10_000_000, // 0.01 SOL
            decay_window_events: 64,
            max_tracked_clusters: 4096,
            max_tracked_ids: 8192,
        }
    }
}

/// Manipulation-adjusted, cluster-aware decomposed breadth for one market.
///
/// ## Responsibility
/// The inspectable multi-dimensional snapshot (§ criterion 47). Every field is
/// stored **separately**; there is deliberately no single composite score
/// (§21.7/§28: "Never collapse into one opaque score. Raw wallet count is not
/// organic breadth.").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreadthDecomposition {
    /// Distinct signer wallets that placed at least one buy.
    pub raw_unique_buyers: u32,
    /// Distinct token accounts seen on buys.
    pub unique_token_accounts: u32,
    /// Distinct fee payers seen on buys.
    pub unique_fee_payers: u32,
    /// Distinct funding roots seen on buys.
    pub unique_funding_roots: u32,
    /// Distinct entity-deduplicated clusters that placed at least one buy.
    pub cluster_adjusted_actors: u32,
    /// Clusters flagged `BUNDLE`.
    pub suspected_bundle_buyers: u32,
    /// Clusters flagged `SNIPER`.
    pub suspected_sniper_buyers: u32,
    /// Clusters flagged `VOLUME_BOT`.
    pub suspected_volume_bot_buyers: u32,
    /// Clusters flagged `WASH`.
    pub suspected_wash_buyers: u32,
    /// Clusters flagged `COORDINATED`.
    pub suspected_coordinated_buyers: u32,
    /// Clusters that placed two or more buys.
    pub repeat_buyers: u32,
    /// Clusters funded by a net-new source.
    pub net_new_funded_buyers: u32,
    /// Clusters holding positive net token inventory (net accumulators).
    pub positive_net_inventory_buyers: u32,
    /// Clusters whose net quote exposure meets the meaningful threshold.
    pub meaningful_net_exposure_buyers: u32,
    /// Genuine-net-exposure breadth: distinct clusters that are simultaneously
    /// positive-net-inventory, meaningful-net-exposure, AND carry no
    /// manipulation/relationship flag. This is the closest thing to "organic
    /// breadth", but is still reported alongside the raw counts, never instead
    /// of them.
    pub genuine_net_exposure_breadth: u32,
    /// Clusters flagged `CREATOR_LINKED`.
    pub creator_linked_buyers: u32,
    /// Clusters flagged `BUNDLE_LINKED`.
    pub bundle_linked_buyers: u32,
    /// Clusters flagged `RUG_CLUSTER`.
    pub known_rug_cluster_buyers: u32,
    /// Clusters flagged `RUNNER_CLUSTER`.
    pub known_runner_cluster_buyers: u32,
    /// Independent buyer expansion: distinct *unflagged* clusters — the count
    /// of clusters that carry no manipulation/relationship flag at all
    /// (independent of net exposure). Independence, not size.
    pub independent_buyer_expansion: u32,
    /// Cluster-adjusted breadth decay proxy: independent (unflagged) clusters
    /// whose *first* buy fell within the trailing `decay_window_events`. A
    /// falling value across successive snapshots indicates breadth decay (new
    /// independent participation drying up).
    pub recent_independent_arrivals: u32,
    /// Ratio (bps) of genuine-net-exposure breadth to raw unique buyers — an
    /// inspectable authenticity descriptor, `None` when there are no buyers.
    /// This is a *reported ratio for inspection*, not a gating score.
    pub genuine_to_raw_bps: Option<u64>,
    /// Whether any capacity bound was hit (counts are lower bounds if so).
    pub completeness: Completeness,
}

/// Streaming reducer that builds a [`BreadthDecomposition`] from [`FlowEvent`]s.
///
/// ## Responsibility
/// Maintains bounded per-cluster and per-dimension state and emits an
/// inspectable snapshot on demand. Deterministic and memory-bounded (§22, §99).
#[derive(Clone, Debug)]
pub struct BreadthReducer {
    config: BreadthConfig,
    clusters: BoundedMap<ClusterAgg>,
    wallets: BoundedSet,
    token_accounts: BoundedSet,
    fee_payers: BoundedSet,
    funding_roots: BoundedSet,
    last_event_index: u64,
}

impl BreadthReducer {
    /// Create a reducer with the given configuration.
    #[must_use]
    pub fn new(config: BreadthConfig) -> Self {
        BreadthReducer {
            clusters: BoundedMap::with_capacity(config.max_tracked_clusters),
            wallets: BoundedSet::with_capacity(config.max_tracked_ids),
            token_accounts: BoundedSet::with_capacity(config.max_tracked_ids),
            fee_payers: BoundedSet::with_capacity(config.max_tracked_ids),
            funding_roots: BoundedSet::with_capacity(config.max_tracked_ids),
            last_event_index: 0,
            config,
        }
    }

    /// The largest `event_index` ingested so far (caller-supplied time).
    #[must_use]
    pub fn last_event_index(&self) -> u64 {
        self.last_event_index
    }

    /// Ingest one flow event. Buys and sells both update net inventory /
    /// exposure; only buys create the "buyer" uniqueness records, matching the
    /// constitution's buyer-breadth semantics.
    ///
    /// Overflow discipline: value sums use `i128` saturating accumulation;
    /// counts saturate; distinct-id capacity is enforced by the bounded
    /// containers (§22, §99).
    pub fn ingest(&mut self, ev: &FlowEvent) {
        self.last_event_index = self.last_event_index.max(ev.event_index);

        let is_buy = ev.side == Side::Buy;
        if is_buy {
            self.wallets.insert(ev.wallet);
            self.token_accounts.insert(ev.token_account);
            self.fee_payers.insert(ev.fee_payer);
            self.funding_roots.insert(ev.funding_root);
        }

        let first_index = ev.event_index;
        if let Some(agg) = self.clusters.get_or_insert_with(ev.cluster, || ClusterAgg {
            first_buy_index: u64::MAX,
            ..ClusterAgg::default()
        }) {
            let signed_tokens = i128::from(ev.token_base_units);
            let signed_quote = i128::from(ev.quote_lamports);
            match ev.side {
                Side::Buy => {
                    agg.net_tokens = agg.net_tokens.saturating_add(signed_tokens);
                    agg.net_quote = agg.net_quote.saturating_add(signed_quote);
                    agg.buy_events = agg.buy_events.saturating_add(1);
                    if first_index < agg.first_buy_index {
                        agg.first_buy_index = first_index;
                    }
                }
                Side::Sell => {
                    agg.net_tokens = agg.net_tokens.saturating_sub(signed_tokens);
                    agg.net_quote = agg.net_quote.saturating_sub(signed_quote);
                }
            }
            agg.flags |= ev.flags.bits();
            agg.funded_net_new |= ev.funded_net_new;
        }
    }

    /// Produce the current decomposed breadth snapshot.
    ///
    /// A cluster is a "buyer" (contributes to buyer counts) iff it placed at
    /// least one buy (`buy_events >= 1`). Sell-only clusters still affect the
    /// net inventory / exposure of their own cluster but are not counted as
    /// buyers.
    #[must_use]
    pub fn snapshot(&self) -> BreadthDecomposition {
        let has = |agg: &ClusterAgg, flag: BuyerFlags| agg.flags & flag.bits() != 0;
        let any_flag_mask = BuyerFlags::all().bits();
        let window_start = self
            .last_event_index
            .saturating_sub(self.config.decay_window_events);

        let mut cluster_adjusted_actors: u32 = 0;
        let mut suspected_bundle: u32 = 0;
        let mut suspected_sniper: u32 = 0;
        let mut suspected_volume_bot: u32 = 0;
        let mut suspected_wash: u32 = 0;
        let mut suspected_coordinated: u32 = 0;
        let mut repeat_buyers: u32 = 0;
        let mut net_new_funded: u32 = 0;
        let mut positive_net_inventory: u32 = 0;
        let mut meaningful_net_exposure: u32 = 0;
        let mut genuine_breadth: u32 = 0;
        let mut creator_linked: u32 = 0;
        let mut bundle_linked: u32 = 0;
        let mut rug_cluster: u32 = 0;
        let mut runner_cluster: u32 = 0;
        let mut independent_expansion: u32 = 0;
        let mut recent_independent: u32 = 0;

        for agg in self.clusters.values() {
            if agg.buy_events == 0 {
                continue; // sell-only cluster: not a buyer
            }
            cluster_adjusted_actors = cluster_adjusted_actors.saturating_add(1);

            if has(agg, BuyerFlags::BUNDLE) {
                suspected_bundle = suspected_bundle.saturating_add(1);
            }
            if has(agg, BuyerFlags::SNIPER) {
                suspected_sniper = suspected_sniper.saturating_add(1);
            }
            if has(agg, BuyerFlags::VOLUME_BOT) {
                suspected_volume_bot = suspected_volume_bot.saturating_add(1);
            }
            if has(agg, BuyerFlags::WASH) {
                suspected_wash = suspected_wash.saturating_add(1);
            }
            if has(agg, BuyerFlags::COORDINATED) {
                suspected_coordinated = suspected_coordinated.saturating_add(1);
            }
            if has(agg, BuyerFlags::CREATOR_LINKED) {
                creator_linked = creator_linked.saturating_add(1);
            }
            if has(agg, BuyerFlags::BUNDLE_LINKED) {
                bundle_linked = bundle_linked.saturating_add(1);
            }
            if has(agg, BuyerFlags::RUG_CLUSTER) {
                rug_cluster = rug_cluster.saturating_add(1);
            }
            if has(agg, BuyerFlags::RUNNER_CLUSTER) {
                runner_cluster = runner_cluster.saturating_add(1);
            }
            if agg.buy_events >= 2 {
                repeat_buyers = repeat_buyers.saturating_add(1);
            }
            if agg.funded_net_new {
                net_new_funded = net_new_funded.saturating_add(1);
            }

            let positive_inv = agg.net_tokens > 0;
            let meaningful_exp =
                agg.net_quote >= i128::from(self.config.meaningful_net_quote_lamports);
            let unflagged = agg.flags & any_flag_mask == 0;

            if positive_inv {
                positive_net_inventory = positive_net_inventory.saturating_add(1);
            }
            if meaningful_exp {
                meaningful_net_exposure = meaningful_net_exposure.saturating_add(1);
            }
            if positive_inv && meaningful_exp && unflagged {
                genuine_breadth = genuine_breadth.saturating_add(1);
            }
            if unflagged {
                independent_expansion = independent_expansion.saturating_add(1);
                if agg.first_buy_index >= window_start && agg.first_buy_index != u64::MAX {
                    recent_independent = recent_independent.saturating_add(1);
                }
            }
        }

        let completeness = self
            .clusters
            .completeness()
            .merge(self.wallets.completeness())
            .merge(self.token_accounts.completeness())
            .merge(self.fee_payers.completeness())
            .merge(self.funding_roots.completeness());

        let genuine_to_raw_bps =
            ratio_bps(u128::from(genuine_breadth), u128::from(self.wallets.len()));

        BreadthDecomposition {
            raw_unique_buyers: self.wallets.len(),
            unique_token_accounts: self.token_accounts.len(),
            unique_fee_payers: self.fee_payers.len(),
            unique_funding_roots: self.funding_roots.len(),
            cluster_adjusted_actors,
            suspected_bundle_buyers: suspected_bundle,
            suspected_sniper_buyers: suspected_sniper,
            suspected_volume_bot_buyers: suspected_volume_bot,
            suspected_wash_buyers: suspected_wash,
            suspected_coordinated_buyers: suspected_coordinated,
            repeat_buyers,
            net_new_funded_buyers: net_new_funded,
            positive_net_inventory_buyers: positive_net_inventory,
            meaningful_net_exposure_buyers: meaningful_net_exposure,
            genuine_net_exposure_breadth: genuine_breadth,
            creator_linked_buyers: creator_linked,
            bundle_linked_buyers: bundle_linked,
            known_rug_cluster_buyers: rug_cluster,
            known_runner_cluster_buyers: runner_cluster,
            independent_buyer_expansion: independent_expansion,
            recent_independent_arrivals: recent_independent,
            genuine_to_raw_bps,
            completeness,
        }
    }
}
