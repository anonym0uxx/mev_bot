//! Venue, lane, and trade-side vocabulary.
//!
//! ## Responsibility
//! Stable, wire-safe enumerations naming *where* a market lives ([`Venue`]),
//! *which validated setup family* is acting on it ([`Lane`]), and *which
//! direction* a fill is ([`Side`]). Each variant has an explicit `#[repr(u8)]`
//! discriminant so persisted journals and `DecisionRecord`s remain stable across
//! rebuilds. No arithmetic, no float — pure classification.
//!
//! ## Constitution alignment
//! * **Section 18.1:** initial required protocols — Pump.fun, PumpSwap, Raydium.
//! * **Section 22 lane law / Section 24:** the independently-attributed setup
//!   families (early entry, graduation transition, active-market scalp); blending
//!   PnL across lanes is prohibited, so the lane label is a first-class field.

use core::fmt;

/// The trading venue / program family a market executes on.
///
/// Explicit discriminants keep the encoding stable in journals. Only the
/// Section 18.1 initial-required set is enumerated; new venues are added here
/// (never inferred at runtime from marketing claims, per Section 18.2).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum Venue {
    /// Pump.fun bonding-curve market (pre-migration).
    PumpFun = 0,
    /// PumpSwap AMM pool (Pump.fun's post-migration pool venue).
    PumpSwap = 1,
    /// Raydium pool (LaunchLab / CPMM migration target).
    Raydium = 2,
}

impl Venue {
    /// All venues in stable discriminant order (useful for exhaustive
    /// registry/iteration code that must be deterministic).
    pub const ALL: [Venue; 3] = [Venue::PumpFun, Venue::PumpSwap, Venue::Raydium];

    /// `true` for venues whose price comes from a bonding curve rather than a
    /// constant-product pool. Constitution Section 24 hold-horizon law treats
    /// bonding-curve and pool markets as mechanically distinct phases.
    #[inline]
    pub const fn is_bonding_curve(self) -> bool {
        matches!(self, Venue::PumpFun)
    }

    /// `true` for constant-product AMM pool venues (post-migration mechanics).
    #[inline]
    pub const fn is_amm_pool(self) -> bool {
        matches!(self, Venue::PumpSwap | Venue::Raydium)
    }

    /// The stable numeric discriminant (for compact serialization).
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Reconstruct from a stable discriminant; `None` for unknown values so
    /// decoding untrusted bytes fails closed (Section 18.2 "fail closed").
    #[inline]
    pub const fn from_u8(v: u8) -> Option<Venue> {
        match v {
            0 => Some(Venue::PumpFun),
            1 => Some(Venue::PumpSwap),
            2 => Some(Venue::Raydium),
            _ => None,
        }
    }
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Venue::PumpFun => "PumpFun",
            Venue::PumpSwap => "PumpSwap",
            Venue::Raydium => "Raydium",
        };
        f.write_str(s)
    }
}

/// A validated setup-family lane. Capital, compute, and PnL attribution are kept
/// strictly separate per lane; no lane is privileged by name and PnL is never
/// blended across lanes.
///
/// Constitution Section 22 (lane law) / Section 24 (EntryModes) / Section 69.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum Lane {
    /// Extremely early low-cap entry at/near creation (preserved early-entry
    /// family, sniper variant).
    CreationSniper = 0,
    /// Early entry gated on first confirmation of genuine activity.
    EarlyConfirmation = 1,
    /// Graduation / migration-window play.
    GraduationTransition = 2,
    /// Active-market scalp on an already-live market (Section 24 scalp lane).
    ActiveMarketScalp = 3,
}

impl Lane {
    /// All lanes in stable discriminant order.
    pub const ALL: [Lane; 4] = [
        Lane::CreationSniper,
        Lane::EarlyConfirmation,
        Lane::GraduationTransition,
        Lane::ActiveMarketScalp,
    ];

    /// `true` for lanes whose objective is short-horizon net-SOL harvesting
    /// (Section 24) rather than moonshot tail capture. Used to keep exit-family
    /// objectives from blending (Section 48).
    #[inline]
    pub const fn is_scalp(self) -> bool {
        matches!(self, Lane::ActiveMarketScalp)
    }

    /// The stable numeric discriminant.
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Reconstruct from a stable discriminant; `None` fails closed on unknown.
    #[inline]
    pub const fn from_u8(v: u8) -> Option<Lane> {
        match v {
            0 => Some(Lane::CreationSniper),
            1 => Some(Lane::EarlyConfirmation),
            2 => Some(Lane::GraduationTransition),
            3 => Some(Lane::ActiveMarketScalp),
            _ => None,
        }
    }
}

impl fmt::Display for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Lane::CreationSniper => "CreationSniper",
            Lane::EarlyConfirmation => "EarlyConfirmation",
            Lane::GraduationTransition => "GraduationTransition",
            Lane::ActiveMarketScalp => "ActiveMarketScalp",
        };
        f.write_str(s)
    }
}

/// The direction of a fill or order intent.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum Side {
    /// Acquiring the base token (spending quote).
    Buy = 0,
    /// Disposing of the base token (receiving quote).
    Sell = 1,
}

impl Side {
    /// The opposite side (the side that closes a position opened on `self`).
    #[inline]
    pub const fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Buy => "Buy",
            Side::Sell => "Sell",
        })
    }
}
