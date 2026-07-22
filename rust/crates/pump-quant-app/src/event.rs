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
            | AppEvent::OnchainConfirm { mint, .. } => Some(*mint),
            AppEvent::Tick => None,
        }
    }
}
