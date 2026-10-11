//! FEES R10 F6 (S2): binding a pool's base-mint supply to evidence.
//!
//! The PumpSwap fee tier uses `poolMarketCap = floor((quote + virtual quote) x LIVE supply / base)`. The supply
//! of a historical trade is only known when evidence binds it to THAT mint at THAT slot:
//! * [`SupplyProvenance::AccountRead`]: the mint account read with `context_slot == slot` — exact.
//! * [`SupplyProvenance::EventInterval`]: bounds `[lo, hi_excl)` implied by charged-fee events
//!   (proc/fees_r7_s2_reconcile.json) — an interval, NOT an exact supply; never priced as one.
//!
//! Rules: no evidence -> `pool_mint_supply_unknown` (never 1e15, never a current RPC read substituted for a
//! historical slot); a read at another slot -> `pool_mint_supply_not_at_slot`; interval only ->
//! `pool_mint_supply_interval_not_exact`; reads that disagree with each other (supply or decimals) or an exact
//! read outside an interval for the same slot -> `pool_mint_supply_snapshots_inconsistent`.
#![forbid(unsafe_code)]

/// Where a supply figure comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplyProvenance {
    /// The mint account's `supply` read at exactly `SupplyEvidence::slot` (RPC `context_slot`).
    AccountRead { supply: u128 },
    /// Bounds implied by the fee tiers charged on events at `SupplyEvidence::slot`: `lo <= supply < hi_excl`.
    EventInterval { lo: u128, hi_excl: u128 },
}

/// One piece of supply evidence, bound to a mint and a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupplyEvidence {
    pub mint: [u8; 32],
    pub decimals: u8,
    pub slot: u64,
    pub provenance: SupplyProvenance,
}

/// Why no exact supply is bound. Every variant is named; none is a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplyRefusal {
    /// No evidence for this mint.
    Unknown,
    /// Evidence exists for this mint, but not at the trade's slot (no substitution across slots).
    NotAtSlot,
    /// Only an interval is known at the slot: not an exact supply.
    IntervalNotExact,
    /// Snapshots for the same mint/slot disagree (supply, decimals, or a read outside the interval).
    SnapshotsInconsistent,
}

impl SupplyRefusal {
    /// Stable report label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "quote_unavailable:pool_mint_supply_unknown",
            Self::NotAtSlot => "quote_unavailable:pool_mint_supply_not_at_slot",
            Self::IntervalNotExact => "quote_unavailable:pool_mint_supply_interval_not_exact",
            Self::SnapshotsInconsistent => {
                "quote_unavailable:pool_mint_supply_snapshots_inconsistent"
            }
        }
    }
}

/// The exact raw supply of `mint` at `slot`, or the named reason it is not known.
///
/// # Errors
/// [`SupplyRefusal`].
pub fn bind_exact_supply(
    mint: &[u8; 32],
    slot: u64,
    evidence: &[SupplyEvidence],
) -> Result<u128, SupplyRefusal> {
    let of_mint: Vec<&SupplyEvidence> = evidence.iter().filter(|e| &e.mint == mint).collect();
    if of_mint.is_empty() {
        return Err(SupplyRefusal::Unknown);
    }
    // Decimals are a property of the mint: any disagreement means the snapshots are not of one mint state.
    if of_mint.iter().any(|e| e.decimals != of_mint[0].decimals) {
        return Err(SupplyRefusal::SnapshotsInconsistent);
    }
    let at: Vec<&SupplyEvidence> = of_mint.into_iter().filter(|e| e.slot == slot).collect();
    if at.is_empty() {
        return Err(SupplyRefusal::NotAtSlot);
    }
    let mut exact: Option<u128> = None;
    for e in &at {
        match e.provenance {
            SupplyProvenance::AccountRead { supply } => match exact {
                Some(x) if x != supply => return Err(SupplyRefusal::SnapshotsInconsistent),
                _ => exact = Some(supply),
            },
            SupplyProvenance::EventInterval { lo, hi_excl } if lo >= hi_excl => {
                return Err(SupplyRefusal::SnapshotsInconsistent);
            }
            SupplyProvenance::EventInterval { .. } => {}
        }
    }
    let Some(s) = exact else {
        return Err(SupplyRefusal::IntervalNotExact);
    };
    for e in &at {
        if let SupplyProvenance::EventInterval { lo, hi_excl } = e.provenance {
            if s < lo || s >= hi_excl {
                return Err(SupplyRefusal::SnapshotsInconsistent);
            }
        }
    }
    Ok(s)
}

/// The v2 cost-block market for a pool, with the supply BOUND by evidence (never assumed): the only constructor
/// of `ExecMarket::Pool { mint_supply: Some(..) }` from evidence. A refusal names why the supply is not exact.
///
/// # Errors
/// [`SupplyRefusal`].
pub fn pool_market_from_evidence(
    effective_quote: u128,
    base_reserve: u128,
    mint: &[u8; 32],
    slot: u64,
    evidence: &[SupplyEvidence],
) -> Result<crate::exec_costs_v2::ExecMarket, SupplyRefusal> {
    let s = bind_exact_supply(mint, slot, evidence)?;
    Ok(crate::exec_costs_v2::ExecMarket::Pool {
        effective_quote,
        base_reserve,
        mint_supply: Some(s),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: [u8; 32] = [7; 32];
    const OTHER: [u8; 32] = [8; 32];
    const SLOT: u64 = 445_637_656;
    // GhMkc... proven interval from proc/fees_r7_s2_reconcile.json.
    const LO: u128 = 985_900_751_621_885;
    const HI: u128 = 990_177_854_156_920;

    fn read(mint: [u8; 32], slot: u64, supply: u128) -> SupplyEvidence {
        SupplyEvidence {
            mint,
            decimals: 6,
            slot,
            provenance: SupplyProvenance::AccountRead { supply },
        }
    }
    fn interval(slot: u64) -> SupplyEvidence {
        SupplyEvidence {
            mint: M,
            decimals: 6,
            slot,
            provenance: SupplyProvenance::EventInterval {
                lo: LO,
                hi_excl: HI,
            },
        }
    }

    #[test]
    fn f6_unknown_stays_unknown_never_1e15_and_no_cross_slot_or_cross_mint_substitution() {
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[]),
            Err(SupplyRefusal::Unknown)
        );
        // A read of ANOTHER mint (the only RPC reads on disk are of other mints) never binds.
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[read(OTHER, SLOT, 1_000_000_000_000_000)]),
            Err(SupplyRefusal::Unknown)
        );
        // A "current" read ~9.1M slots later is not the historical supply.
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[read(M, SLOT + 9_100_000, LO)]),
            Err(SupplyRefusal::NotAtSlot)
        );
        assert_eq!(
            SupplyRefusal::Unknown.label(),
            "quote_unavailable:pool_mint_supply_unknown"
        );
    }

    #[test]
    fn f6_an_interval_is_not_an_exact_supply() {
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[interval(SLOT)]),
            Err(SupplyRefusal::IntervalNotExact)
        );
    }

    #[test]
    fn f6_inconsistent_snapshots_are_refused_by_name() {
        let inc = Err(SupplyRefusal::SnapshotsInconsistent);
        // Two reads at the same slot disagree.
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[read(M, SLOT, LO), read(M, SLOT, LO + 1)]),
            inc
        );
        // A read outside the event interval at the same slot.
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[read(M, SLOT, HI), interval(SLOT)]),
            inc
        );
        // Decimals disagree across snapshots of one mint.
        let mut d9 = read(M, SLOT, LO);
        d9.decimals = 9;
        assert_eq!(bind_exact_supply(&M, SLOT, &[read(M, SLOT, LO), d9]), inc);
        // A malformed interval.
        let mut bad = interval(SLOT);
        bad.provenance = SupplyProvenance::EventInterval {
            lo: HI,
            hi_excl: LO,
        };
        assert_eq!(bind_exact_supply(&M, SLOT, &[read(M, SLOT, LO), bad]), inc);
    }

    #[test]
    fn f6_an_exact_read_at_the_slot_inside_the_interval_binds() {
        let s = LO + 1_000;
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[read(M, SLOT, s), interval(SLOT)]),
            Ok(s)
        );
        assert_eq!(
            bind_exact_supply(&M, SLOT, &[read(M, SLOT, s), read(M, SLOT, s)]),
            Ok(s)
        );
        // Only an evidence-bound supply builds a pool market; an interval alone does not.
        assert!(matches!(
            pool_market_from_evidence(1, 1, &M, SLOT, &[read(M, SLOT, s)]),
            Ok(crate::exec_costs_v2::ExecMarket::Pool { mint_supply: Some(x), .. }) if x == s
        ));
        assert_eq!(
            pool_market_from_evidence(1, 1, &M, SLOT, &[interval(SLOT)]),
            Err(SupplyRefusal::IntervalNotExact)
        );
    }
}
