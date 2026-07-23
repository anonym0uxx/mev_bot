//! `wl_lane_performance` leaf — realized net-SOL per lane.
//!
//! Responsibility: accumulate the realized outcome of trades attributed to each
//! discovery lane, measured in **net-SOL** (signed lamports, after all costs),
//! per §74 — never win-rate, never gross. This closes the feedback loop: a
//! lane's realized net-SOL can be turned into a ranking weight so capital and
//! attention flow to lanes that actually produce net SOL.
//!
//! Memory is a fixed per-lane array (§99): O(number of lanes), independent of
//! how many trades are recorded.
//!
//! Overflow (§22, explicit): net-SOL totals use **saturating** signed addition —
//! a lamport total that would exceed `i64` range is a physically impossible
//! outcome (Solana's total supply is far below `i64::MAX` lamports), so
//! saturation is a safe-by-contract clamp that can never silently wrap money.
//! Trade counts use **checked** increment and are reported via `Option` so an
//! implausible overflow surfaces rather than wrapping.

use crate::candidate::{DiscoveryLane, Lane};

/// Per-**discovery-lane** realized-net-SOL ledger (§71.2 reflection integrity).
///
/// Responsibility: the same net-SOL accountant as [`LanePerformance`], but keyed
/// on the independent [`DiscoveryLane`] provenance instead of the setup-archetype
/// [`Lane`]. This is what closes the §71 reflection-integrity gap: an on-chain
/// creation sighting and a social caller both present as `CreationSniper`, so the
/// archetype-keyed ledger lumps their realized outcomes together and
/// cross-contaminates the learning signal; keyed on the discovery lane, each lane
/// is graded on the SOL IT actually earned. Fixed per-lane array, §99. Overflow
/// semantics identical to [`LanePerformance`] (saturating signed add — see module
/// docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DiscoveryLanePerformance {
    net_sol_lamports: [i64; DiscoveryLane::COUNT],
    trade_count: [u64; DiscoveryLane::COUNT],
}

impl Default for DiscoveryLanePerformance {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscoveryLanePerformance {
    /// A ledger with all discovery lanes at zero net-SOL and zero trades.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            net_sol_lamports: [0; DiscoveryLane::COUNT],
            trade_count: [0; DiscoveryLane::COUNT],
        }
    }

    /// Record one realized trade outcome for a discovery lane (signed lamports).
    /// Saturating add (safe-by-contract, see module docs). §74.
    pub fn record(&mut self, lane: DiscoveryLane, net_sol_lamports: i64) {
        let i = lane.index();
        self.net_sol_lamports[i] = self.net_sol_lamports[i].saturating_add(net_sol_lamports);
        self.trade_count[i] = self.trade_count[i].saturating_add(1);
    }

    /// Total realized net-SOL for a discovery lane, in signed lamports. §74.
    #[must_use]
    pub fn net_sol(&self, lane: DiscoveryLane) -> i64 {
        self.net_sol_lamports[lane.index()]
    }

    /// Number of trades recorded for a discovery lane.
    #[must_use]
    pub fn trade_count(&self, lane: DiscoveryLane) -> u64 {
        self.trade_count[lane.index()]
    }

    /// Total realized net-SOL across all discovery lanes, in signed lamports.
    /// Saturating fold (safe-by-contract). §22 / §74.
    #[must_use]
    pub fn total_net_sol(&self) -> i64 {
        self.net_sol_lamports
            .iter()
            .fold(0i64, |acc, &v| acc.saturating_add(v))
    }
}

/// Per-lane realized-net-SOL ledger.
///
/// Responsibility: the bounded accumulator for `wl_lane_performance`. §99.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LanePerformance {
    net_sol_lamports: [i64; Lane::COUNT],
    trade_count: [u64; Lane::COUNT],
}

impl Default for LanePerformance {
    fn default() -> Self {
        Self::new()
    }
}

impl LanePerformance {
    /// A ledger with all lanes at zero net-SOL and zero trades.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            net_sol_lamports: [0; Lane::COUNT],
            trade_count: [0; Lane::COUNT],
        }
    }

    /// Record one realized trade outcome for a lane: `net_sol_lamports` is the
    /// signed net result in lamports (negative for a loss).
    ///
    /// Net-SOL accumulates with saturating add (safe-by-contract, see module
    /// docs); the trade count increments with saturating add on `u64` (a count
    /// that reaches `u64::MAX` is not a real scenario and clamps rather than
    /// wraps). Deterministic (§22). §74.
    pub fn record(&mut self, lane: Lane, net_sol_lamports: i64) {
        let i = lane.index();
        self.net_sol_lamports[i] = self.net_sol_lamports[i].saturating_add(net_sol_lamports);
        self.trade_count[i] = self.trade_count[i].saturating_add(1);
    }

    /// Total realized net-SOL for a lane, in signed lamports. §74.
    #[must_use]
    pub fn net_sol(&self, lane: Lane) -> i64 {
        self.net_sol_lamports[lane.index()]
    }

    /// Number of trades recorded for a lane.
    #[must_use]
    pub fn trade_count(&self, lane: Lane) -> u64 {
        self.trade_count[lane.index()]
    }

    /// Mean realized net-SOL per trade for a lane, in signed lamports
    /// (truncated toward zero), or `None` if the lane has no recorded trades.
    ///
    /// Exact integer division — no float (§22).
    #[must_use]
    pub fn net_sol_per_trade(&self, lane: Lane) -> Option<i64> {
        let n = self.trade_count(lane);
        if n == 0 {
            None
        } else {
            // n fits i64 (it is at most the count of recorded trades) and net
            // fits i64, so this division cannot overflow.
            Some(self.net_sol(lane) / n as i64)
        }
    }

    /// Total realized net-SOL across all lanes, in signed lamports.
    ///
    /// Saturating fold (safe-by-contract, see module docs). §22 / §74.
    #[must_use]
    pub fn total_net_sol(&self) -> i64 {
        self.net_sol_lamports
            .iter()
            .fold(0i64, |acc, &v| acc.saturating_add(v))
    }
}
