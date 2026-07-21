//! Creator-state reducer.
//!
//! ## Responsibility
//! Reduce the creator/deployer's own on-chain activity for a single market into
//! an inspectable creator-state snapshot: initial allocation, current position,
//! realized sells, sell fraction, and position fraction of supply (§21.2
//! "creator position and sells"; §22 behavioral-risk creator inputs; §6.4
//! "creator risk" is a *derived* value that must remain separable from raw
//! truth).
//!
//! The constitution is explicit that high creator ownership must **not**
//! automatically become a binary rejection (§22 behavioral-risk clause): it is
//! evaluated alongside buyer independence, cluster-adjusted breadth, exit
//! capacity, etc. Accordingly this reducer produces *measures*, never a verdict.
//!
//! ## Determinism & bounds
//! Pure integer reducer (§22). Slots are carried in the events; nothing reads a
//! clock. Fixed O(1) state — no per-event growth.

use crate::common::{ratio_bps, EntityId};

/// A creator-attributed event for one market.
///
/// ## Responsibility
/// Carries only what the reducer needs, with the slot supplied by the caller so
/// timing stays time-safe (§ no wall-clock). "Creator-attributed" means
/// upstream creator/deployer attribution (§6.2 on-chain creator reconstruction)
/// has already decided this action belongs to the creator entity.
#[derive(Clone, Copy, Debug)]
pub enum CreatorEvent {
    /// The create/initialize event establishing the creator's starting token
    /// allocation and the token's total supply.
    Init {
        /// Creator's initial token allocation (base units, e.g. dev pre-buy /
        /// creator vault). May be zero.
        initial_tokens: u64,
        /// Total token supply (base units), used for position-fraction math.
        total_supply: u64,
        /// Slot of the create event.
        slot: u64,
    },
    /// A creator buy (accumulation).
    Buy {
        /// Tokens acquired (base units).
        tokens: u64,
        /// Quote lamports spent.
        quote_lamports: u64,
        /// Slot of the buy.
        slot: u64,
    },
    /// A creator sell (distribution / potential extraction).
    Sell {
        /// Tokens sold (base units).
        tokens: u64,
        /// Quote lamports realized.
        quote_lamports: u64,
        /// Slot of the sell.
        slot: u64,
    },
    /// A buy by a *creator-linked* wallet (funded by / clustered with the
    /// creator, per §28 entity dedup). Tracked separately from the creator's
    /// own actions.
    LinkedBuy {
        /// The linked cluster id.
        cluster: EntityId,
        /// Tokens acquired.
        tokens: u64,
        /// Slot of the buy.
        slot: u64,
    },
}

impl CreatorEvent {
    /// Slot at which this event occurred (caller-supplied time).
    #[must_use]
    pub fn slot(&self) -> u64 {
        match *self {
            CreatorEvent::Init { slot, .. }
            | CreatorEvent::Buy { slot, .. }
            | CreatorEvent::Sell { slot, .. }
            | CreatorEvent::LinkedBuy { slot, .. } => slot,
        }
    }
}

/// Inspectable creator-state snapshot for one market.
///
/// ## Responsibility
/// Multi-dimensional, separable creator measures (§ criterion 47; §6.4). No
/// composite "creator risk score" is produced here; the fields are the inputs a
/// downstream risk-pricing stage combines under evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreatorState {
    /// Whether an `Init` event has been observed (else supply/fractions are
    /// UNKNOWN and reported as `None`).
    pub initialized: bool,
    /// Creator's initial token allocation (base units).
    pub initial_tokens: u64,
    /// Total token supply (base units), 0 until initialized.
    pub total_supply: u64,
    /// Tokens the creator has bought since init (base units, saturating).
    pub tokens_bought: u128,
    /// Tokens the creator has sold since init (base units, saturating).
    pub tokens_sold: u128,
    /// Quote lamports the creator has spent buying (saturating).
    pub quote_spent: u128,
    /// Quote lamports the creator has realized selling (saturating).
    pub quote_realized: u128,
    /// Number of creator buy events (saturating).
    pub buy_count: u32,
    /// Number of creator sell events (saturating).
    pub sell_count: u32,
    /// Current net creator position: `initial + bought - sold` (base units).
    /// Clamped at zero — a creator cannot hold negative tokens; if observed
    /// sells exceed observed acquisition the excess indicates unattributed
    /// acquisition and the position floors at zero (reported via
    /// [`CreatorState::oversold`]).
    pub current_position: u128,
    /// True if observed sells exceeded observed initial+bought (attribution gap).
    pub oversold: bool,
    /// Peak position ever held (max of running position), base units.
    pub peak_position: u128,
    /// Slot of the creator's first sell, if any (time-safe).
    pub first_sell_slot: Option<u64>,
    /// Distinct creator-linked clusters observed buying.
    pub creator_linked_clusters: u32,
    /// Fraction of *peak* position the creator has since sold, in bps.
    /// `None` when peak position is zero.
    pub sold_fraction_of_peak_bps: Option<u64>,
    /// Current position as a fraction of total supply, in bps. `None` until
    /// initialized (supply unknown).
    pub position_fraction_of_supply_bps: Option<u64>,
}

/// Streaming reducer building a [`CreatorState`].
///
/// ## Responsibility
/// Fixed-size, deterministic accumulation of creator activity (§22). Bounded by
/// construction: O(1) scalar state plus a small bounded set of linked clusters.
#[derive(Clone, Debug)]
pub struct CreatorStateReducer {
    initialized: bool,
    initial_tokens: u64,
    total_supply: u64,
    tokens_bought: u128,
    tokens_sold: u128,
    quote_spent: u128,
    quote_realized: u128,
    buy_count: u32,
    sell_count: u32,
    peak_position: u128,
    first_sell_slot: Option<u64>,
    linked_clusters: crate::common::BoundedSet,
    oversold: bool,
}

impl CreatorStateReducer {
    /// Create a reducer tracking at most `max_linked_clusters` distinct
    /// creator-linked clusters (§99 memory bound).
    #[must_use]
    pub fn new(max_linked_clusters: usize) -> Self {
        CreatorStateReducer {
            initialized: false,
            initial_tokens: 0,
            total_supply: 0,
            tokens_bought: 0,
            tokens_sold: 0,
            quote_spent: 0,
            quote_realized: 0,
            buy_count: 0,
            sell_count: 0,
            peak_position: 0,
            first_sell_slot: None,
            linked_clusters: crate::common::BoundedSet::with_capacity(max_linked_clusters),
            oversold: false,
        }
    }

    /// Current net creator position, clamped at zero.
    fn current_position(&self) -> u128 {
        let gross = u128::from(self.initial_tokens).saturating_add(self.tokens_bought);
        gross.saturating_sub(self.tokens_sold)
    }

    /// Ingest one creator event, updating running state.
    ///
    /// Overflow discipline: token/quote sums use `u128` saturating adds; counts
    /// saturate (§22). A later `Init` (protocol re-init edge case) overwrites
    /// the supply/initial-allocation baseline but preserves accumulated flow.
    pub fn ingest(&mut self, ev: &CreatorEvent) {
        match *ev {
            CreatorEvent::Init {
                initial_tokens,
                total_supply,
                ..
            } => {
                self.initialized = true;
                self.initial_tokens = initial_tokens;
                self.total_supply = total_supply;
                let pos = self.current_position();
                if pos > self.peak_position {
                    self.peak_position = pos;
                }
            }
            CreatorEvent::Buy {
                tokens,
                quote_lamports,
                ..
            } => {
                self.tokens_bought = self.tokens_bought.saturating_add(u128::from(tokens));
                self.quote_spent = self.quote_spent.saturating_add(u128::from(quote_lamports));
                self.buy_count = self.buy_count.saturating_add(1);
                let pos = self.current_position();
                if pos > self.peak_position {
                    self.peak_position = pos;
                }
            }
            CreatorEvent::Sell {
                tokens,
                quote_lamports,
                slot,
            } => {
                let gross = u128::from(self.initial_tokens).saturating_add(self.tokens_bought);
                let prospective = self.tokens_sold.saturating_add(u128::from(tokens));
                if prospective > gross {
                    self.oversold = true;
                }
                self.tokens_sold = prospective;
                self.quote_realized = self
                    .quote_realized
                    .saturating_add(u128::from(quote_lamports));
                self.sell_count = self.sell_count.saturating_add(1);
                if self.first_sell_slot.is_none() {
                    self.first_sell_slot = Some(slot);
                }
            }
            CreatorEvent::LinkedBuy { cluster, .. } => {
                self.linked_clusters.insert(cluster);
            }
        }
    }

    /// Produce the current inspectable creator-state snapshot.
    #[must_use]
    pub fn snapshot(&self) -> CreatorState {
        let current_position = self.current_position();
        let sold_fraction_of_peak_bps = if self.peak_position == 0 {
            None
        } else {
            ratio_bps(self.tokens_sold, self.peak_position)
        };
        let position_fraction_of_supply_bps = if self.initialized && self.total_supply > 0 {
            ratio_bps(current_position, u128::from(self.total_supply))
        } else {
            None
        };

        CreatorState {
            initialized: self.initialized,
            initial_tokens: self.initial_tokens,
            total_supply: self.total_supply,
            tokens_bought: self.tokens_bought,
            tokens_sold: self.tokens_sold,
            quote_spent: self.quote_spent,
            quote_realized: self.quote_realized,
            buy_count: self.buy_count,
            sell_count: self.sell_count,
            current_position,
            oversold: self.oversold,
            peak_position: self.peak_position,
            first_sell_slot: self.first_sell_slot,
            creator_linked_clusters: self.linked_clusters.len(),
            sold_fraction_of_peak_bps,
            position_fraction_of_supply_bps,
        }
    }
}
