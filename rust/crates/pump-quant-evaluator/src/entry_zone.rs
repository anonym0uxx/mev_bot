//! `entry_zone` — dynamic entry-zone taxonomy and per-zone outcome
//! stratification (constitution §25).
//!
//! §25 requires the frozen evaluator to stratify reconciled outcomes by *entry
//! zone* — a decision-time classification combining a token's market-cap band
//! with its migration phase — and to measure per-zone net-SOL, fees, price
//! impact, rug rate and MFE/MAE. The existing `evaluator_stats` excursion path
//! keys only by [`ArchetypeKey`](crate::evaluator_stats::ArchetypeKey); there was
//! no zone key at all. This module adds the shared [`EntryZone`] taxonomy, the
//! deterministic band+phase classifier, and the per-zone stratifier.
//!
//! §22: integer / fixed-point only. Market caps are integer quote-units (e.g.
//! whole USD or lamport-equivalents — the unit is the caller's, the thresholds
//! are in the same unit). Rug rate is bps. No floats, no wall-clock, no RNG.
//! Grouping uses `BTreeMap` keyed by the zone's stable ordinal so output order is
//! a deterministic function of the taxonomy.

use std::collections::BTreeMap;

// ============================================================================
// Migration phase (decision-time venue state).
// ============================================================================

/// Where a token sits in the bonding-curve -> pool migration lifecycle at the
/// moment of the entry decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MigrationPhase {
    /// Still on the bonding curve, not yet at the migration edge.
    PreMigration,
    /// In the migration window itself (curve completing / pool seeding).
    Migrating,
    /// Migrated to an AMM pool.
    PostMigration,
}

// ============================================================================
// Entry-zone taxonomy (§25).
// ============================================================================

/// The seven §25 entry zones. The discriminant order is the deterministic
/// output order of any per-zone stratification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryZone {
    /// Below the attention floor, pre-attention accumulation.
    Sub5kPreAttention,
    /// Early-validation band.
    Band5kTo9kEarlyValidation,
    /// The core target band.
    Band9kTo20kTarget,
    /// Momentum-confirmed band.
    Band20kTo50kMomentumConfirmed,
    /// Large-cap but still on the curve, close to migrating — late pre-migration.
    PreMigrationLate,
    /// In the migration window itself.
    MigrationEdge,
    /// Post-migration revival (already in a pool).
    PostMigrationRevival,
}

impl EntryZone {
    /// Stable ordinal for the zone (matches discriminant order).
    pub fn ordinal(self) -> u8 {
        match self {
            EntryZone::Sub5kPreAttention => 0,
            EntryZone::Band5kTo9kEarlyValidation => 1,
            EntryZone::Band9kTo20kTarget => 2,
            EntryZone::Band20kTo50kMomentumConfirmed => 3,
            EntryZone::PreMigrationLate => 4,
            EntryZone::MigrationEdge => 5,
            EntryZone::PostMigrationRevival => 6,
        }
    }
}

/// Market-cap band thresholds for the pre-migration zones, in the caller's
/// integer quote unit. All bands are half-open `[lo, hi)` on ascending caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneThresholds {
    /// Upper edge of the sub-attention band (exclusive).
    pub sub5k: u64,
    /// Upper edge of the early-validation band (exclusive).
    pub early9k: u64,
    /// Upper edge of the target band (exclusive).
    pub target20k: u64,
    /// Upper edge of the momentum-confirmed band (exclusive); at/above this cap
    /// a still-pre-migration token is classified [`EntryZone::PreMigrationLate`].
    pub momentum50k: u64,
}

impl ZoneThresholds {
    /// Standard pump.fun-scaled band edges (in whole quote units), the reference
    /// taxonomy §25 names: 5k / 9k / 20k / 50k.
    pub fn standard() -> Self {
        ZoneThresholds {
            sub5k: 5_000,
            early9k: 9_000,
            target20k: 20_000,
            momentum50k: 50_000,
        }
    }

    /// True iff the thresholds are strictly ascending (a well-formed band ladder).
    pub fn is_well_formed(&self) -> bool {
        self.sub5k < self.early9k
            && self.early9k < self.target20k
            && self.target20k < self.momentum50k
    }
}

/// Classify an entry decision into its [`EntryZone`] from market cap + phase.
///
/// Migration phase dominates the cap band: a [`MigrationPhase::PostMigration`]
/// token is always [`EntryZone::PostMigrationRevival`] and a
/// [`MigrationPhase::Migrating`] token is always [`EntryZone::MigrationEdge`],
/// regardless of cap. Only [`MigrationPhase::PreMigration`] tokens fall through
/// to the cap-band ladder, and a pre-migration token at/above `momentum50k` is
/// [`EntryZone::PreMigrationLate`] rather than a momentum band. Deterministic and
/// total.
///
/// Panics if `thresholds` are not strictly ascending — an ill-formed ladder
/// would silently misclassify.
pub fn classify_entry_zone(
    market_cap: u64,
    phase: MigrationPhase,
    thresholds: ZoneThresholds,
) -> EntryZone {
    assert!(
        thresholds.is_well_formed(),
        "classify_entry_zone: thresholds must be strictly ascending"
    );
    match phase {
        MigrationPhase::PostMigration => EntryZone::PostMigrationRevival,
        MigrationPhase::Migrating => EntryZone::MigrationEdge,
        MigrationPhase::PreMigration => {
            if market_cap < thresholds.sub5k {
                EntryZone::Sub5kPreAttention
            } else if market_cap < thresholds.early9k {
                EntryZone::Band5kTo9kEarlyValidation
            } else if market_cap < thresholds.target20k {
                EntryZone::Band9kTo20kTarget
            } else if market_cap < thresholds.momentum50k {
                EntryZone::Band20kTo50kMomentumConfirmed
            } else {
                EntryZone::PreMigrationLate
            }
        }
    }
}

// ============================================================================
// Per-zone outcome stratification.
// ============================================================================

/// One reconciled outcome tagged with the zone its entry was decided in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneOutcomeRow {
    /// Zone the entry decision was classified into.
    pub zone: EntryZone,
    /// Reconciled net lamports (may be negative).
    pub net_lamports: i128,
    /// Fees paid, lamports.
    pub fees: u128,
    /// Price-impact cost, lamports.
    pub impact_lamports: u128,
    /// Whether this position terminated as a rug.
    pub rugged: bool,
    /// Maximum favorable excursion, bps.
    pub mfe_bps: i64,
    /// Maximum adverse excursion, bps.
    pub mae_bps: i64,
}

impl ZoneOutcomeRow {
    /// Test/golden-vector constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn test(
        zone: EntryZone,
        net: i128,
        fees: u128,
        impact: u128,
        rugged: bool,
        mfe: i64,
        mae: i64,
    ) -> Self {
        ZoneOutcomeRow {
            zone,
            net_lamports: net,
            fees,
            impact_lamports: impact,
            rugged,
            mfe_bps: mfe,
            mae_bps: mae,
        }
    }
}

/// Per-zone aggregate: net-SOL, fees, impact, rug rate and MFE/MAE medians.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneStrata {
    /// Which zone this aggregate covers.
    pub zone: EntryZone,
    /// Number of outcomes in the zone.
    pub n: u32,
    /// Net lamports summed over the zone.
    pub net_lamports: i128,
    /// Fees summed over the zone, lamports.
    pub fees: u128,
    /// Price-impact cost summed over the zone, lamports.
    pub impact_lamports: u128,
    /// Number of rugged terminations.
    pub rug_count: u32,
    /// Rug rate in bps of the zone's outcomes (`rug_count * 10_000 / n`).
    pub rug_rate_bps: u32,
    /// Median MFE across the zone, bps.
    pub median_mfe_bps: i64,
    /// Median MAE across the zone, bps.
    pub median_mae_bps: i64,
}

/// Deterministic integer median of an already-sorted slice (even -> average of
/// the two central elements, integer division).
fn median_sorted(sorted: &[i64]) -> i64 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        let a = sorted[n / 2 - 1] as i128;
        let b = sorted[n / 2] as i128;
        ((a + b) / 2) as i64
    }
}

/// Stratify reconciled outcomes by entry zone (§25).
///
/// Every zone present in `rows` is emitted exactly once, in ascending
/// [`EntryZone`] ordinal order (deterministic). Sums are `i128`/`u128` with
/// checked accumulation; `rug_rate_bps = rug_count * 10_000 / n`; MFE/MAE are
/// per-zone medians of the excursions. Zones with no outcomes are simply absent
/// (never fabricated as zero rows). Deterministic; empty input -> empty vector.
pub fn stratify_by_zone(rows: &[ZoneOutcomeRow]) -> Vec<ZoneStrata> {
    struct Acc {
        n: u32,
        net: i128,
        fees: u128,
        impact: u128,
        rug: u32,
        mfe: Vec<i64>,
        mae: Vec<i64>,
    }
    let mut groups: BTreeMap<u8, (EntryZone, Acc)> = BTreeMap::new();

    for r in rows {
        let entry = groups.entry(r.zone.ordinal()).or_insert_with(|| {
            (
                r.zone,
                Acc {
                    n: 0,
                    net: 0,
                    fees: 0,
                    impact: 0,
                    rug: 0,
                    mfe: Vec::new(),
                    mae: Vec::new(),
                },
            )
        });
        let acc = &mut entry.1;
        acc.n += 1;
        acc.net = acc
            .net
            .checked_add(r.net_lamports)
            .expect("stratify_by_zone: net overflow");
        acc.fees = acc
            .fees
            .checked_add(r.fees)
            .expect("stratify_by_zone: fees overflow");
        acc.impact = acc
            .impact
            .checked_add(r.impact_lamports)
            .expect("stratify_by_zone: impact overflow");
        if r.rugged {
            acc.rug += 1;
        }
        acc.mfe.push(r.mfe_bps);
        acc.mae.push(r.mae_bps);
    }

    groups
        .into_values()
        .map(|(zone, mut acc)| {
            acc.mfe.sort_unstable();
            acc.mae.sort_unstable();
            let rug_rate_bps = if acc.n == 0 {
                0
            } else {
                ((acc.rug as u64 * 10_000) / acc.n as u64) as u32
            };
            ZoneStrata {
                zone,
                n: acc.n,
                net_lamports: acc.net,
                fees: acc.fees,
                impact_lamports: acc.impact,
                rug_count: acc.rug,
                rug_rate_bps,
                median_mfe_bps: median_sorted(&acc.mfe),
                median_mae_bps: median_sorted(&acc.mae),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn th() -> ZoneThresholds {
        ZoneThresholds::standard()
    }

    #[test]
    fn classify_cap_bands_pre_migration() {
        let p = MigrationPhase::PreMigration;
        assert_eq!(
            classify_entry_zone(4_999, p, th()),
            EntryZone::Sub5kPreAttention
        );
        assert_eq!(
            classify_entry_zone(5_000, p, th()),
            EntryZone::Band5kTo9kEarlyValidation
        );
        assert_eq!(
            classify_entry_zone(9_000, p, th()),
            EntryZone::Band9kTo20kTarget
        );
        assert_eq!(
            classify_entry_zone(19_999, p, th()),
            EntryZone::Band9kTo20kTarget
        );
        assert_eq!(
            classify_entry_zone(20_000, p, th()),
            EntryZone::Band20kTo50kMomentumConfirmed
        );
        assert_eq!(
            classify_entry_zone(50_000, p, th()),
            EntryZone::PreMigrationLate
        );
        assert_eq!(
            classify_entry_zone(1_000_000, p, th()),
            EntryZone::PreMigrationLate
        );
    }

    #[test]
    fn phase_dominates_cap_band() {
        // A tiny-cap token that is migrating is MigrationEdge, not Sub5k.
        assert_eq!(
            classify_entry_zone(100, MigrationPhase::Migrating, th()),
            EntryZone::MigrationEdge
        );
        // A large post-migration token is Revival regardless of cap.
        assert_eq!(
            classify_entry_zone(999_999, MigrationPhase::PostMigration, th()),
            EntryZone::PostMigrationRevival
        );
    }

    #[test]
    #[should_panic(expected = "strictly ascending")]
    fn ill_formed_thresholds_panic() {
        let bad = ZoneThresholds {
            sub5k: 9_000,
            early9k: 5_000,
            target20k: 20_000,
            momentum50k: 50_000,
        };
        let _ = classify_entry_zone(1_000, MigrationPhase::PreMigration, bad);
    }

    #[test]
    fn stratify_groups_and_orders() {
        let z = EntryZone::Band9kTo20kTarget;
        let rows = vec![
            ZoneOutcomeRow::test(z, 1_000, 100, 10, false, 5_000, -1_000),
            ZoneOutcomeRow::test(z, -400, 50, 5, true, 2_000, -3_000),
            ZoneOutcomeRow::test(
                EntryZone::Sub5kPreAttention,
                9_999,
                1,
                1,
                false,
                8_000,
                -500,
            ),
        ];
        let out = stratify_by_zone(&rows);
        assert_eq!(out.len(), 2);
        // Sub5k (ordinal 0) comes before Target (ordinal 2).
        assert_eq!(out[0].zone, EntryZone::Sub5kPreAttention);
        assert_eq!(out[1].zone, z);
        let target = out[1];
        assert_eq!(target.n, 2);
        assert_eq!(target.net_lamports, 600);
        assert_eq!(target.fees, 150);
        assert_eq!(target.impact_lamports, 15);
        assert_eq!(target.rug_count, 1);
        assert_eq!(target.rug_rate_bps, 5_000); // 1 of 2
        assert_eq!(target.median_mfe_bps, 3_500); // median(2_000, 5_000)
        assert_eq!(target.median_mae_bps, -2_000); // median(-3_000, -1_000)
    }

    #[test]
    fn stratify_empty_is_empty() {
        assert!(stratify_by_zone(&[]).is_empty());
    }

    #[test]
    fn rug_rate_all_rugged() {
        let z = EntryZone::MigrationEdge;
        let rows = vec![
            ZoneOutcomeRow::test(z, -1_000, 10, 0, true, 100, -9_000),
            ZoneOutcomeRow::test(z, -2_000, 10, 0, true, 50, -9_500),
        ];
        let out = stratify_by_zone(&rows);
        assert_eq!(out[0].rug_rate_bps, 10_000);
    }
}
