//! The deterministic input stream the nervous system consumes.
//!
//! In laptop (Phase-A) mode these events come from a replay journal — a recorded
//! or synthetic sequence — never from a live RPC socket. The engine's determinism
//! contract is: *the same `[AppEvent]` slice always yields the same decisions and
//! the same net-SOL*, which is what makes replay a correctness authority (§22, §54).
//!
//! Events carry integer/fixed-point payloads only. Wall-clock never appears; time
//! is the explicit `tick` logical clock advanced by [`AppEvent::Tick`].

use pump_quant_domain::ids::Mint;

/// The lane an observation belongs to. Mirrors `watchlist::Lane` but is the
/// engine-facing name; the four lanes are unioned, not intersected (§71).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LaneKind {
    /// On-chain numeric microstructure (flow, liquidity, velocity).
    Numeric,
    /// Narrative / attention-velocity signal (virality, meta emergence).
    Narrative,
    /// Social-source chatter (calls, mentions) — corroboration-tier only.
    Social,
    /// Smart-money / wallet-graph activity.
    Wallet,
}

impl LaneKind {
    /// All four lanes, in canonical order.
    pub const ALL: [LaneKind; 4] = [
        LaneKind::Numeric,
        LaneKind::Narrative,
        LaneKind::Social,
        LaneKind::Wallet,
    ];

    /// A lane whose evidence, on its own, may authorise capital. Only the on-chain
    /// numeric lane qualifies; narrative, social and wallet lanes are corroboration
    /// that can raise a candidate's rank but never trigger entry alone (§29 fade-
    /// first discipline, §71 corroboration tier).
    #[must_use]
    pub const fn is_self_authorizing(self) -> bool {
        matches!(self, LaneKind::Numeric)
    }
}

/// The specific creator-attributed action carried by an [`AppEvent::CreatorAction`].
///
/// Mirrors `pump_quant_market_state::creator::CreatorEvent` one-to-one **minus the
/// slot** — the carrying `AppEvent::CreatorAction` supplies the slot once — so the
/// engine translates it without interpretation. All integer, `Copy` (§22). Creator
/// measures are corroboration-tier behavioural-risk inputs: they only ever *reduce*
/// size within an already-admitted band, never authorise or veto (§22 behavioral-
/// risk clause — high creator ownership is never an automatic binary reject).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreatorActionKind {
    /// Create/initialize: the creator's starting allocation and the token supply.
    Init {
        /// Creator's initial token allocation (base units); may be zero.
        initial_tokens: u64,
        /// Total token supply (base units), for position-fraction math.
        total_supply: u64,
    },
    /// A creator buy (accumulation).
    Buy {
        /// Tokens acquired (base units).
        tokens: u64,
        /// Quote lamports spent.
        quote_lamports: u64,
    },
    /// A creator sell (distribution / potential extraction).
    Sell {
        /// Tokens sold (base units).
        tokens: u64,
        /// Quote lamports realized.
        quote_lamports: u64,
    },
    /// A buy by a creator-linked (funded/clustered) wallet, tracked separately from
    /// the creator's own actions (§28 entity dedup).
    LinkedBuy {
        /// The linked cluster id.
        cluster: u64,
        /// Tokens acquired (base units).
        tokens: u64,
    },
}

/// One unit of input to the engine.
///
/// `Copy` and small so a journal of millions of events replays without allocation
/// churn. Every mint-bearing event names the market it concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// A decoded on-chain swap on a market. Buy pressure is signed into
    /// `signed_base` (positive = net buy). This is the only lane that produces
    /// self-authorizing evidence.
    MarketTrade {
        /// The market this trade hit.
        mint: Mint,
        /// Reserve-derived execution price of this print, fixed-point in
        /// `pump_quant_features::types::PRICE_SCALE` (1e9) units. Feeds VWAP and
        /// CVD/price-divergence — the real microstructure the numeric lane scores on
        /// (§21.7). Integer/fixed-point only (§22).
        price_fp: i128,
        /// Quote (lamport) volume of this print. Signed by `signed_base`'s sign into
        /// CVD (cumulative volume delta) — the primary order-flow-intent proxy.
        quote_lamports: u64,
        /// Pool quote-reserve depth after the trade, in lamports.
        liquidity_lamports: u64,
        /// Signed base volume of this print (positive = buy, negative = sell).
        signed_base: i64,
        /// A stable per-entity id so distinct buyers can be counted without a float.
        buyer_entity: u64,
        /// Market age at this trade, in slots.
        age_slots: u32,
    },

    /// A narrative attention sample for a market: how many fresh mentions arrived
    /// against how many were already active. Feeds the virality coefficient.
    NarrativeSample {
        /// The market the narrative concerns.
        mint: Mint,
        /// Mentions already active in the prior window.
        prior_active: u64,
        /// New mentions this window.
        new_mentions: u64,
    },

    /// A social-source call/mention for a market from a scored source. Corroboration
    /// only — never sufficient for entry.
    SocialCall {
        /// The market called.
        mint: Mint,
        /// Source quality, bps (0..=10_000); higher = more historically reliable.
        source_quality_bp: u32,
    },

    /// A smart-money wallet action on a market.
    WalletAction {
        /// The market acted on.
        mint: Mint,
        /// Whether the acting wallet is classified followable/smart.
        followable: bool,
        /// Size of the action, lamports.
        size_lamports: u64,
    },

    /// An explicit on-chain confirmation that a market is real and sellable at the
    /// given depth. The gate REQUIRES one of these before it will admit capital to
    /// a candidate, regardless of how loud the corroboration lanes are (§29, §71).
    OnchainConfirm {
        /// The confirmed market.
        mint: Mint,
        /// Sellable depth proven on-chain, lamports.
        sellable_depth_lamports: u64,
    },

    /// A deterministic, **on-chain-led** category assignment for a market. The
    /// category classifier ran UPSTREAM on the token's decoded name/symbol (an
    /// `[S]`-boundary concern in `token_ingest`); the engine sees only the resolved
    /// integer `category_id` (0 = UNCLASSIFIED), never a string — factual category
    /// state is never populated by social interpretation (§21.4, criterion 83/§85).
    /// Feeds the per-category `MetaRotationState` measures (launches, then flow).
    TokenMetadata {
        /// The market this metadata describes.
        mint: Mint,
        /// Resolved category id (0 = UNCLASSIFIED).
        category_id: u64,
        /// Taxonomy version the id was assigned under (non-retroactive, criterion 81).
        taxonomy_version: u32,
        /// Creator/deployer entity id (upstream on-chain attribution).
        creator: u64,
        /// Slot at which the metadata was observed (caller time; no wall-clock).
        slot: u64,
    },

    /// A creator-attributed on-chain action for a market, feeding the `CreatorState`
    /// reducer. Corroboration-tier: creator measures only ever *reduce* size within
    /// an admitted band, never authorise or veto (§22 behavioral-risk clause).
    CreatorAction {
        /// The market acted on.
        mint: Mint,
        /// The specific creator action (mirrors `creator::CreatorEvent`).
        kind: CreatorActionKind,
        /// Slot of the action (caller-supplied time).
        slot: u64,
    },

    /// The market migrated from its bonding curve to a pool (graduation). Flips the
    /// market's venue-mechanics **phase** (§21.7 phase asymmetry / §24 hold-horizon:
    /// curve and pool are never pooled into one model): exit-cost pricing, hazard
    /// conditioning, and lifecycle parameters consult the phase from this point on.
    Migration {
        /// The graduated market.
        mint: Mint,
        /// Slot of the migration (caller-supplied time).
        slot: u64,
    },

    /// Advance the logical clock by one tick. Recency decay, TTL pruning and the
    /// reflection cadence are all measured in ticks — never wall-clock.
    Tick,
}

impl AppEvent {
    /// The market this event concerns, if any (`Tick` concerns none).
    #[must_use]
    pub const fn mint(&self) -> Option<Mint> {
        match self {
            AppEvent::MarketTrade { mint, .. }
            | AppEvent::NarrativeSample { mint, .. }
            | AppEvent::SocialCall { mint, .. }
            | AppEvent::WalletAction { mint, .. }
            | AppEvent::OnchainConfirm { mint, .. }
            | AppEvent::TokenMetadata { mint, .. }
            | AppEvent::CreatorAction { mint, .. }
            | AppEvent::Migration { mint, .. } => Some(*mint),
            AppEvent::Tick => None,
        }
    }
}
