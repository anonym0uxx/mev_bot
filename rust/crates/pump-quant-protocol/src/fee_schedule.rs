//! THE venue-fee authority: one VERSIONED calculation of the pump.fun bonding-curve and canonical PumpSwap venue
//! fee, keyed by (venue, fee-config version selected by slot, pool market-cap tier, order size).
//!
//! Every fee-charging quote (paper fills, liquidation valuations) and every fee ESTIMATE (round-trip cost, the
//! prompt's cost lines) reads its rates from here. Nothing else in the workspace may state a venue-fee number for
//! the model lane; the legacy-lane constants in `pump-quant-app::cost_model` are golden-pinned estimates and are
//! listed as such in `proc/FEES_TRACE.md`.
//!
//! VERSIONS (raw mainnet account bytes, read-only captures, sha256-pinned below):
//! * bonding curve: pump-fees `FeeConfig` `8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt` read at slot 454,754,100
//!   (one tier: lp 0, protocol 95, creator-or-cashback 30 = 125 bp/side).
//! * PumpSwap: pump-fees `FeeConfig` `5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx` read at slots 445,186,127 and
//!   454,754,100 (25 market-cap tiers, identical in both reads; 125 bp/side below 420 SOL down to 30 bp/side at
//!   98,240 SOL and above).
//!
//! HISTORICAL SELECTION ([`FeeSelection::BySlot`]): a trade at slot `s` is priced with the configuration known to be
//! in force at `s`. Every answer carries its [`Provenance`]; nothing is silent:
//! * `s` equals a read slot -> [`Provenance::ReadAtSlot`];
//! * `s` lies between two reads whose tier tables are identical -> [`Provenance::BetweenIdenticalReads`] (a labelled
//!   assumption: the account did not change and change back); different tables -> named refusal;
//! * `s` is after the latest read -> [`Provenance::AssumedUnchangedSinceRead`] (labelled assumption);
//! * curve only, `s` before the read but at/after [`CURVE_TRADE_EVIDENCE_FROM_SLOT`] -> [`Provenance::
//!   AssumedFromTradeEvidence`]: 100 of 100 independent curve trades in slots 445,637,759..445,638,597 charged
//!   exactly 95 + 30 (`proc/m3_hist_curve_fee_evidence.json`), the same rates the later read decodes to;
//! * anything earlier -> [`FeeScheduleRefusal::HistoricalConfigUnknown`].
//!
//! [`FeeSelection::Pinned`] ignores the slot and uses the latest read, labelled [`Provenance::Pinned`]: for
//! synthetic-slot test rigs only; the daemon selects by slot.
#![forbid(unsafe_code)]

use crate::pumpswap_fees::{decode_fee_config, fee_ceil, FeeConfig, Fees};

/// The venue a fee is charged on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FeeVenue {
    /// pump.fun bonding curve (non-mayhem, SOL quote).
    BondingCurve,
    /// Canonical pump PumpSwap pool, WSOL quote (`fee_tiers`; never `flat_fees`).
    PumpSwapCanonical,
}

impl FeeVenue {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::BondingCurve => "curve",
            Self::PumpSwapCanonical => "pumpswap",
        }
    }
}

/// One captured fee-configuration account.
#[derive(Debug, PartialEq, Eq)]
pub struct FeeConfigVersion {
    pub venue: FeeVenue,
    pub account: &'static str,
    pub read_slot: u64,
    /// sha256 of `bytes` (version identity; pinned by a test).
    pub sha256_hex: &'static str,
    pub bytes: &'static [u8],
}

/// Every captured version, grouped by venue, ascending read slot within a venue.
pub static VERSIONS: [FeeConfigVersion; 3] = [
    FeeConfigVersion {
        venue: FeeVenue::BondingCurve,
        account: "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt",
        read_slot: 454_754_100,
        sha256_hex: "3504fae20640e15bd441257ab118aa96fb4e99190defc06a01aae26b307bf204",
        bytes: include_bytes!("fee_configs/curve_fee_config_454754100.bin"),
    },
    FeeConfigVersion {
        venue: FeeVenue::PumpSwapCanonical,
        account: "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx",
        read_slot: 445_186_127,
        sha256_hex: "e1c4647573d8caacc33b267781272c4fa0ad30a70900dddbaab512db670d3af2",
        bytes: include_bytes!("fee_configs/amm_fee_config_445186127.bin"),
    },
    FeeConfigVersion {
        venue: FeeVenue::PumpSwapCanonical,
        account: "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx",
        read_slot: 454_754_100,
        sha256_hex: "d8706882a217e82af619b754e37beedf9338a25787ebec717e7d69d2cf347c9d",
        bytes: include_bytes!("fee_configs/amm_fee_config_454754100.bin"),
    },
];

/// First slot of the curve trade-event evidence window (`m3_hist_curve_fee_evidence.json` `window[0]`).
pub const CURVE_TRADE_EVIDENCE_FROM_SLOT: u64 = 445_637_000;
/// The (protocol, creator-or-cashback) bp every curve trade in the evidence window charged.
pub const CURVE_TRADE_EVIDENCE_RATES: (u64, u64) = (95, 30);
/// Raw supply the curve market cap is computed against (`bondingCurveMarketCap`).
pub const CURVE_MCAP_SUPPLY: u128 = 1_000_000_000_000_000;

/// How the configuration for a slot was established. Always reported with the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// The slot is the read slot of the version.
    ReadAtSlot,
    /// Between two reads with identical tier tables (assumed unchanged in between).
    BetweenIdenticalReads { lo: u64, hi: u64 },
    /// After the latest read (assumed unchanged since).
    AssumedUnchangedSinceRead { read_slot: u64 },
    /// Curve only: before the read, inside the trade-event evidence range.
    AssumedFromTradeEvidence { from_slot: u64, read_slot: u64 },
    /// [`FeeSelection::Pinned`]: slot ignored, latest read used (test rigs only).
    Pinned { read_slot: u64 },
}

impl Provenance {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadAtSlot => "read_at_slot",
            Self::BetweenIdenticalReads { .. } => "between_identical_reads",
            Self::AssumedUnchangedSinceRead { .. } => "assumed_unchanged_since_read",
            Self::AssumedFromTradeEvidence { .. } => "assumed_from_trade_evidence",
            Self::Pinned { .. } => "pinned_slot_ignored",
        }
    }
}

/// Configuration selection policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeeSelection {
    /// The configuration in force at the trade's slot (historical replay and the daemon).
    BySlot,
    /// Latest read, slot ignored, labelled (synthetic-slot test rigs).
    #[default]
    Pinned,
}

/// Named refusals. None is ever priced as zero or as a default rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeScheduleRefusal {
    /// No configuration (and no labelled evidence) is known for this slot.
    HistoricalConfigUnknown {
        venue: FeeVenue,
        slot: u64,
        earliest: u64,
    },
    /// The slot lies between two reads whose tier tables differ: the change slot is unknown.
    ConfigChangedBetweenReads { venue: FeeVenue, lo: u64, hi: u64 },
    /// A captured account failed to decode or to match its pinned digest / evidence.
    ConfigUndecodable { venue: FeeVenue, read_slot: u64 },
    /// The rates a landing event reported are not a tier of the configuration in force (wrong venue / wrong tier /
    /// fee-exempt or unknown schedule).
    RatesNotInSchedule { venue: FeeVenue },
    /// The market cap is undefined (zero reserve).
    MarketCapUndefined,
    /// Arithmetic out of range.
    Overflow,
}

impl FeeScheduleRefusal {
    /// Stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::HistoricalConfigUnknown { .. } => "fee_config_unknown_at_slot",
            Self::ConfigChangedBetweenReads { .. } => "fee_config_changed_between_reads",
            Self::ConfigUndecodable { .. } => "fee_config_undecodable",
            Self::RatesNotInSchedule { .. } => "fee_rates_not_in_schedule",
            Self::MarketCapUndefined => "fee_market_cap_undefined",
            Self::Overflow => "fee_overflow",
        }
    }
}

/// A configuration together with why it applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigInForce {
    pub version: &'static FeeConfigVersion,
    pub config: FeeConfig,
    pub provenance: Provenance,
}

fn versions_of(venue: FeeVenue) -> impl Iterator<Item = &'static FeeConfigVersion> {
    VERSIONS.iter().filter(move |v| v.venue == venue)
}

fn decode(v: &'static FeeConfigVersion) -> Result<FeeConfig, FeeScheduleRefusal> {
    let bad = FeeScheduleRefusal::ConfigUndecodable {
        venue: v.venue,
        read_slot: v.read_slot,
    };
    let cfg = decode_fee_config(v.bytes).ok_or(bad)?;
    if v.venue == FeeVenue::BondingCurve {
        // The curve's tier rule: no LP leg. A curve schedule with an LP leg is not this venue's schedule.
        if cfg.fee_tiers.iter().any(|t| t.fees.lp_bps != 0) {
            return Err(bad);
        }
    }
    Ok(cfg)
}

/// The configuration in force for `venue` at `slot` under `sel`, with its provenance.
///
/// # Errors
/// [`FeeScheduleRefusal`] — never a default.
pub fn config_in_force(
    venue: FeeVenue,
    slot: u64,
    sel: FeeSelection,
) -> Result<ConfigInForce, FeeScheduleRefusal> {
    let latest = versions_of(venue)
        .last()
        .ok_or(FeeScheduleRefusal::HistoricalConfigUnknown {
            venue,
            slot,
            earliest: u64::MAX,
        })?;
    if sel == FeeSelection::Pinned {
        return Ok(ConfigInForce {
            version: latest,
            config: decode(latest)?,
            provenance: Provenance::Pinned {
                read_slot: latest.read_slot,
            },
        });
    }
    let prev = versions_of(venue).filter(|v| v.read_slot <= slot).last();
    let next = versions_of(venue).find(|v| v.read_slot > slot);
    match (prev, next) {
        (Some(p), _) if p.read_slot == slot => Ok(ConfigInForce {
            version: p,
            config: decode(p)?,
            provenance: Provenance::ReadAtSlot,
        }),
        (Some(p), Some(n)) => {
            let (a, b) = (decode(p)?, decode(n)?);
            if a.fee_tiers != b.fee_tiers {
                return Err(FeeScheduleRefusal::ConfigChangedBetweenReads {
                    venue,
                    lo: p.read_slot,
                    hi: n.read_slot,
                });
            }
            Ok(ConfigInForce {
                version: p,
                config: a,
                provenance: Provenance::BetweenIdenticalReads {
                    lo: p.read_slot,
                    hi: n.read_slot,
                },
            })
        }
        (Some(p), None) => Ok(ConfigInForce {
            version: p,
            config: decode(p)?,
            provenance: Provenance::AssumedUnchangedSinceRead {
                read_slot: p.read_slot,
            },
        }),
        (None, Some(n))
            if venue == FeeVenue::BondingCurve && slot >= CURVE_TRADE_EVIDENCE_FROM_SLOT =>
        {
            let cfg = decode(n)?;
            // The assumption holds only if the read decodes to exactly the rates the evidence charged.
            let ok = cfg.fee_tiers.len() == 1
                && (
                    cfg.fee_tiers[0].fees.protocol_bps,
                    cfg.fee_tiers[0].fees.creator_bps,
                ) == CURVE_TRADE_EVIDENCE_RATES;
            if !ok {
                return Err(FeeScheduleRefusal::ConfigUndecodable {
                    venue,
                    read_slot: n.read_slot,
                });
            }
            Ok(ConfigInForce {
                version: n,
                config: cfg,
                provenance: Provenance::AssumedFromTradeEvidence {
                    from_slot: CURVE_TRADE_EVIDENCE_FROM_SLOT,
                    read_slot: n.read_slot,
                },
            })
        }
        (None, n) => Err(FeeScheduleRefusal::HistoricalConfigUnknown {
            venue,
            slot,
            earliest: if venue == FeeVenue::BondingCurve {
                CURVE_TRADE_EVIDENCE_FROM_SLOT
            } else {
                n.map_or(u64::MAX, |v| v.read_slot)
            },
        }),
    }
}

/// The per-side rates of one trade, with the tier that produced them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueRates {
    pub venue: FeeVenue,
    /// lp / protocol / creator bp. On a cashback coin the creator share is paid as cashback: `creator_bps` is 0
    /// and `cashback_bps` carries it.
    pub fees: Fees,
    pub cashback_bps: u64,
    pub tier_index: u16,
    pub tier_threshold: u128,
}

impl VenueRates {
    /// All components, bp per side.
    #[must_use]
    pub const fn total_bps(&self) -> u64 {
        self.fees.lp_bps + self.fees.protocol_bps + self.fees.creator_bps + self.cashback_bps
    }
}

/// `vsol * 1e15 / vtok` (`bondingCurveMarketCap`).
#[must_use]
pub fn curve_market_cap(vsol: u64, vtok: u64) -> Option<u128> {
    if vtok == 0 {
        return None;
    }
    u128::from(vsol)
        .checked_mul(CURVE_MCAP_SUPPLY)?
        .checked_div(u128::from(vtok))
}

/// `calculate_fee_tier` index: below the first threshold -> 0; else the highest tier whose threshold <= mcap.
fn tier_index(cfg: &FeeConfig, mcap: u128) -> Option<usize> {
    let first = cfg.fee_tiers.first()?;
    if mcap < first.threshold {
        return Some(0);
    }
    cfg.fee_tiers.iter().rposition(|t| mcap >= t.threshold)
}

/// Rates by market-cap tier (curve: the curve market cap; pool: the pool market cap at live supply).
///
/// # Errors
/// [`FeeScheduleRefusal::Overflow`] if the table is empty.
pub fn rates_at_market_cap(
    cif: &ConfigInForce,
    mcap: u128,
) -> Result<VenueRates, FeeScheduleRefusal> {
    let i = tier_index(&cif.config, mcap).ok_or(FeeScheduleRefusal::Overflow)?;
    let t = cif.config.fee_tiers[i];
    Ok(VenueRates {
        venue: cif.version.venue,
        fees: t.fees,
        cashback_bps: 0,
        tier_index: u16::try_from(i).map_err(|_| FeeScheduleRefusal::Overflow)?,
        tier_threshold: t.threshold,
    })
}

/// Rates a landing PumpSwap event reported (`lp`, `protocol`, `coin_creator` bp and, when the layout carries it,
/// `cashback` bp), VALIDATED against the configuration in force: they must be one tier of that venue's table
/// (on a cashback coin the tier's creator share appears as cashback). Anything else is refused by name — a rate
/// from another venue's schedule, a non-existent tier, or an all-zero event is never priced.
///
/// # Errors
/// [`FeeScheduleRefusal::RatesNotInSchedule`].
pub fn rates_from_event(
    cif: &ConfigInForce,
    parts: (u64, u64, u64),
    cashback_bps: Option<u64>,
) -> Result<VenueRates, FeeScheduleRefusal> {
    let venue = cif.version.venue;
    let (lp, pr, cr) = parts;
    let cb = cashback_bps.unwrap_or(0);
    let hit = cif.config.fee_tiers.iter().enumerate().find(|(_, t)| {
        let f = t.fees;
        (f.lp_bps, f.protocol_bps) == (lp, pr)
            && ((cr == f.creator_bps && cb == 0) || (cr == 0 && cb == f.creator_bps && cb > 0))
    });
    let (i, t) = hit.ok_or(FeeScheduleRefusal::RatesNotInSchedule { venue })?;
    Ok(VenueRates {
        venue,
        fees: Fees {
            lp_bps: lp,
            protocol_bps: pr,
            creator_bps: cr,
        },
        cashback_bps: cb,
        tier_index: u16::try_from(i).map_err(|_| FeeScheduleRefusal::Overflow)?,
        tier_threshold: t.threshold,
    })
}

/// The fee of ONE leg of `base` lamports, each component ceil-rounded (SDK `fee`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeCharge {
    pub lp: u128,
    pub protocol: u128,
    pub creator: u128,
    /// Credited to the trader's claimable account: a cost of the trade, NOT cash.
    pub cashback: u128,
}

impl FeeCharge {
    /// Every component, lamports.
    #[must_use]
    pub const fn total(&self) -> u128 {
        self.lp + self.protocol + self.creator + self.cashback
    }
}

/// Charge `rates` on `base` lamports (the order size the program applies the fee to).
#[must_use]
pub fn charge(rates: &VenueRates, base: u128) -> Option<FeeCharge> {
    Some(FeeCharge {
        lp: fee_ceil(base, rates.fees.lp_bps)?,
        protocol: fee_ceil(base, rates.fees.protocol_bps)?,
        creator: fee_ceil(base, rates.fees.creator_bps)?,
        cashback: fee_ceil(base, rates.cashback_bps)?,
    })
}

/// Which version / provenance / tier priced a quote. Carried on every quote and counted per fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeStamp {
    pub venue: FeeVenue,
    pub read_slot: u64,
    pub provenance: Provenance,
    pub tier_index: u16,
    pub total_bps: u64,
}

impl FeeStamp {
    /// From a configuration and the rates it produced.
    #[must_use]
    pub fn of(cif: &ConfigInForce, r: &VenueRates) -> Self {
        Self {
            venue: r.venue,
            read_slot: cif.version.read_slot,
            provenance: cif.provenance,
            tier_index: r.tier_index,
            total_bps: r.total_bps(),
        }
    }

    /// `<venue>@<read_slot>:<provenance>:tier<i>:<bps>bp` — the report key.
    #[must_use]
    pub fn label(&self) -> String {
        format!(
            "{}@{}:{}:tier{}:{}bp",
            self.venue.label(),
            self.read_slot,
            self.provenance.label(),
            self.tier_index,
            self.total_bps
        )
    }
}

/// Curve rates for a trade at `slot` on reserves (`vsol`, `vtok`): config in force + market-cap tier.
///
/// # Errors
/// [`FeeScheduleRefusal`].
pub fn curve_rates(
    vsol: u64,
    vtok: u64,
    slot: u64,
    sel: FeeSelection,
) -> Result<(VenueRates, FeeStamp), FeeScheduleRefusal> {
    let cif = config_in_force(FeeVenue::BondingCurve, slot, sel)?;
    let mcap = curve_market_cap(vsol, vtok).ok_or(FeeScheduleRefusal::MarketCapUndefined)?;
    let r = rates_at_market_cap(&cif, mcap)?;
    Ok((r, FeeStamp::of(&cif, &r)))
}

/// Canonical-pool rates for a trade at `slot`, from the landing event's reported parts, validated against the
/// configuration in force.
///
/// # Errors
/// [`FeeScheduleRefusal`].
pub fn pool_rates_from_event(
    parts: (u64, u64, u64),
    cashback_bps: Option<u64>,
    slot: u64,
    sel: FeeSelection,
) -> Result<(VenueRates, FeeStamp), FeeScheduleRefusal> {
    let cif = config_in_force(FeeVenue::PumpSwapCanonical, slot, sel)?;
    let r = rates_from_event(&cif, parts, cashback_bps)?;
    Ok((r, FeeStamp::of(&cif, &r)))
}

/// Canonical-pool rates by pool market cap (for estimates where no landing event exists).
///
/// # Errors
/// [`FeeScheduleRefusal`].
pub fn pool_rates_at_market_cap(
    mcap: u128,
    slot: u64,
    sel: FeeSelection,
) -> Result<(VenueRates, FeeStamp), FeeScheduleRefusal> {
    let cif = config_in_force(FeeVenue::PumpSwapCanonical, slot, sel)?;
    let r = rates_at_market_cap(&cif, mcap)?;
    Ok((r, FeeStamp::of(&cif, &r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::{sha256, to_hex};

    const SOL: u128 = 1_000_000_000;

    #[test]
    fn every_version_matches_its_pinned_digest_and_decodes() {
        for v in &VERSIONS {
            assert_eq!(to_hex(&sha256(v.bytes)), v.sha256_hex, "{}", v.read_slot);
            assert!(decode(v).is_ok(), "{:?} {}", v.venue, v.read_slot);
        }
    }

    #[test]
    fn curve_is_one_tier_95_plus_30() {
        let (r, s) = curve_rates(
            32_000_000_000,
            1_000_000_000_000_000,
            454_754_100,
            FeeSelection::BySlot,
        )
        .unwrap();
        assert_eq!(
            (r.fees.lp_bps, r.fees.protocol_bps, r.fees.creator_bps),
            (0, 95, 30)
        );
        assert_eq!(r.total_bps(), 125);
        assert_eq!(s.provenance, Provenance::ReadAtSlot);
        assert_eq!(s.label(), "curve@454754100:read_at_slot:tier0:125bp");
    }

    #[test]
    fn wrong_venue_the_curve_never_reads_the_pool_table_and_the_reverse() {
        let c = config_in_force(FeeVenue::BondingCurve, 454_754_100, FeeSelection::BySlot).unwrap();
        let p = config_in_force(
            FeeVenue::PumpSwapCanonical,
            454_754_100,
            FeeSelection::BySlot,
        )
        .unwrap();
        assert_eq!(c.version.venue, FeeVenue::BondingCurve);
        assert_eq!(
            c.version.account,
            "8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt"
        );
        assert_eq!(c.config.fee_tiers.len(), 1);
        assert_eq!(p.version.venue, FeeVenue::PumpSwapCanonical);
        assert_eq!(p.config.fee_tiers.len(), 25);
        // The curve's own rates reported on a pool event are not a pool tier: refused, never priced.
        assert_eq!(
            pool_rates_from_event((0, 95, 30), None, 454_754_100, FeeSelection::BySlot),
            Err(FeeScheduleRefusal::RatesNotInSchedule {
                venue: FeeVenue::PumpSwapCanonical
            })
        );
        // A curve at pool-tier market caps still pays the curve rate (one tier), never a pool tier.
        let (r, _) = curve_rates(
            400_000_000_000,
            1_000_000_000_000_000,
            454_754_100,
            FeeSelection::BySlot,
        )
        .unwrap();
        assert_eq!(r.total_bps(), 125);
        assert_eq!(r.fees.lp_bps, 0);
    }

    #[test]
    fn wrong_tier_pool_tiers_switch_exactly_at_their_thresholds() {
        let at = |sol: u128| {
            pool_rates_at_market_cap(sol, 454_754_100, FeeSelection::BySlot)
                .unwrap()
                .0
        };
        assert_eq!(at(420 * SOL - 1).total_bps(), 125);
        assert_eq!(at(420 * SOL - 1).tier_index, 0);
        assert_eq!(at(420 * SOL).total_bps(), 120);
        assert_eq!(at(420 * SOL).tier_index, 1);
        assert_eq!(at(29_470 * SOL - 1).total_bps(), 80);
        assert_eq!(at(29_470 * SOL).total_bps(), 75);
        assert_eq!(at(98_240 * SOL - 1).total_bps(), 33);
        assert_eq!(at(98_240 * SOL).total_bps(), 30);
        assert_eq!(at(u128::MAX / 2).total_bps(), 30);
    }

    #[test]
    fn wrong_tier_event_rates_must_be_a_tier_and_cashback_takes_the_creator_share() {
        let ev = |p, cb| pool_rates_from_event(p, cb, 454_754_100, FeeSelection::BySlot);
        assert_eq!(ev((20, 5, 50), None).unwrap().0.tier_index, 10);
        assert_eq!(ev((2, 93, 30), Some(0)).unwrap().0.total_bps(), 125);
        let cb = ev((20, 5, 0), Some(5)).unwrap().0;
        assert_eq!(
            (cb.fees.creator_bps, cb.cashback_bps, cb.tier_index),
            (0, 5, 24)
        );
        // Not a tier: an all-zero event, a mixed tier, a creator AND cashback leg, a cashback rate off-table.
        for (p, c) in [
            ((0, 0, 0), Some(0)),
            ((20, 5, 51), None),
            ((2, 93, 95), None),
            ((20, 5, 5), Some(5)),
            ((20, 5, 0), Some(7)),
            ((25, 5, 0), Some(0)),
        ] {
            assert_eq!(
                ev(p, c),
                Err(FeeScheduleRefusal::RatesNotInSchedule {
                    venue: FeeVenue::PumpSwapCanonical
                }),
                "{p:?} {c:?}"
            );
        }
    }

    #[test]
    fn historical_config_is_selected_by_slot_with_named_provenance() {
        let by = |v, s| config_in_force(v, s, FeeSelection::BySlot);
        use FeeVenue::{BondingCurve as C, PumpSwapCanonical as P};
        // Pool: read, bracketed, after, before.
        assert_eq!(
            by(P, 445_186_127).unwrap().provenance,
            Provenance::ReadAtSlot
        );
        assert_eq!(by(P, 445_186_127).unwrap().version.read_slot, 445_186_127);
        assert_eq!(
            by(P, 445_637_627).unwrap().provenance,
            Provenance::BetweenIdenticalReads {
                lo: 445_186_127,
                hi: 454_754_100
            }
        );
        assert_eq!(by(P, 445_637_627).unwrap().version.read_slot, 445_186_127);
        assert_eq!(
            by(P, 460_000_000).unwrap().provenance,
            Provenance::AssumedUnchangedSinceRead {
                read_slot: 454_754_100
            }
        );
        assert_eq!(
            by(P, 445_186_126),
            Err(FeeScheduleRefusal::HistoricalConfigUnknown {
                venue: P,
                slot: 445_186_126,
                earliest: 445_186_127
            })
        );
        // Curve: evidence-backed assumption inside the window, refusal before it.
        assert_eq!(
            by(C, 445_637_627).unwrap().provenance,
            Provenance::AssumedFromTradeEvidence {
                from_slot: 445_637_000,
                read_slot: 454_754_100
            }
        );
        assert_eq!(
            by(C, 445_636_999),
            Err(FeeScheduleRefusal::HistoricalConfigUnknown {
                venue: C,
                slot: 445_636_999,
                earliest: 445_637_000
            })
        );
        assert_eq!(
            by(C, 2_000).unwrap_err().label(),
            "fee_config_unknown_at_slot"
        );
        // Pinned ignores the slot, labelled.
        assert_eq!(
            config_in_force(C, 2_000, FeeSelection::Pinned)
                .unwrap()
                .provenance,
            Provenance::Pinned {
                read_slot: 454_754_100
            }
        );
    }

    #[test]
    fn the_two_pool_reads_have_identical_tier_tables_but_different_exotic_fees() {
        let a = decode(&VERSIONS[1]).unwrap();
        let b = decode(&VERSIONS[2]).unwrap();
        assert_eq!(a.fee_tiers, b.fee_tiers);
        assert_ne!(a.exotic_flat_fees, b.exotic_flat_fees);
    }

    #[test]
    fn charge_rounds_each_component_up_once() {
        let (r, _) = curve_rates(
            32_000_000_000,
            1_000_000_000_000_000,
            454_754_100,
            FeeSelection::BySlot,
        )
        .unwrap();
        let c = charge(&r, 1_000_001).unwrap();
        assert_eq!(
            (c.protocol, c.creator, c.lp, c.cashback),
            (9_501, 3_001, 0, 0)
        );
        assert_eq!(c.total(), 12_502);
    }
}
