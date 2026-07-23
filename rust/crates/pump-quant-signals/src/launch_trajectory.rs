//! Launch-sale-trajectory + creation-window competition feature families
//! (constitution §21.7, criterion 104).
//!
//! Two deterministic §21.7 families computed on Section 28 entity-deduplicated
//! flow. Both are **recorded empirical priors, re-measured rather than
//! assumed**, and **neither vetoes alone** (the ConvexityPreservationLedger
//! audit hook is the supervisor half, out of scope here).
//!
//! - **Launch-sale trajectory** (MemeTrans-class evidence): sale duration,
//!   tier-progression velocity, tx count + unique-buyer breadth, per-buyer
//!   accumulation-distribution shape, and bundle-adjusted top-N holding
//!   concentration at migration.
//! - **Creation-window competition** (adverse-selection meter): the first-slot
//!   distribution of *other participants'* priority fees/tips (max/mean/count/
//!   unique tippers), bundle participation, and Tier-2 sniper-cohort presence.
//!
//! # Constitution constraints (§22)
//!
//! Pure, deterministic, integer-only. Concentration and breadth are basis
//! points; SOL is lamports; time is milliseconds. Entity aggregation is on the
//! Section 28 deduplicated `entity_id` (raw wallet counts are the number the
//! adversary controls; the deduplicated distribution is the truth).

use std::collections::BTreeMap;

/// A buy transaction observed during a bonding-curve sale.
///
/// Responsibility: atomic unit of the launch-sale trajectory family (§21.7).
/// `buyer_entity` is the Section 28 deduplicated cluster id. `base_amount` is
/// tokens acquired (base units). Constitution §22: integer quantities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaleTx {
    /// Landing time in milliseconds.
    pub ts_ms: u64,
    /// Section 28 entity-deduplicated buyer cluster id.
    pub buyer_entity: u64,
    /// Base tokens acquired in this transaction.
    pub base_amount: u64,
}

/// Result of analysing a completed/completing bonding-curve sale (§21.7).
///
/// Responsibility: the sale-phase feature vector consumed as a prior by
/// graduation/post-migration lane admission and hazard features (never a
/// standalone veto). Constitution §22: integer / bps fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SaleTrajectory {
    /// Wall-clock span of the sale (last ts - first ts), milliseconds.
    pub duration_ms: u64,
    /// Number of buy transactions over the sale.
    pub tx_count: u32,
    /// Distinct Section 28 entities that bought (deduplicated breadth).
    pub unique_buyers: u32,
    /// Buyer breadth ratio in bps: `unique_buyers * 10_000 / tx_count`
    /// (high = broad/organic, low = concentrated/repeat). `0` if no txs.
    pub breadth_bps: u32,
    /// Curve tier-progression velocity in bps of curve progress per second:
    /// `(end_progress_bps - start_progress_bps) * 1_000 / duration_ms`.
    pub tier_velocity_bps_per_s: i64,
    /// Largest single-entity accumulation (base units) — the per-buyer
    /// accumulation-distribution peak.
    pub max_per_buyer_base: u64,
    /// Bundle-adjusted top-N holding concentration at migration, in bps of the
    /// total sold: `sum(top_n entity holdings) * 10_000 / total_base`.
    pub top_n_concentration_bps: u32,
}

/// Analyse a bonding-curve sale into its §21.7 trajectory features.
///
/// `start_progress_bps`/`end_progress_bps` are decoded curve-completion
/// fractions (0..=10_000) at the first/last observed sale tx; `top_n` is the
/// holder-concentration cohort size. Holdings are aggregated on the Section 28
/// `buyer_entity` (bundle-adjusted by construction). Empty input yields
/// all-zero.
///
/// Responsibility: single entry point producing the launch-sale trajectory
/// vector (§21.7). Constitution §22: integer aggregation, `u128` widening on
/// the concentration ratio, division guards on empty/zero denominators.
pub fn analyze_sale_trajectory(
    txs: &[SaleTx],
    top_n: usize,
    start_progress_bps: u32,
    end_progress_bps: u32,
) -> SaleTrajectory {
    let mut out = SaleTrajectory::default();
    if txs.is_empty() {
        return out;
    }
    let first_ts = txs.iter().map(|t| t.ts_ms).min().unwrap(); // LINT-ALLOW(hot_panic): infallible — `txs.is_empty()` returned above
    let last_ts = txs.iter().map(|t| t.ts_ms).max().unwrap(); // LINT-ALLOW(hot_panic): infallible — `txs.is_empty()` returned above
    out.duration_ms = last_ts - first_ts;
    out.tx_count = txs.len() as u32;

    // Aggregate holdings per deduplicated entity (deterministic BTreeMap order).
    let mut by_entity: BTreeMap<u64, u64> = BTreeMap::new();
    let mut total_base: u128 = 0;
    for t in txs {
        let e = by_entity.entry(t.buyer_entity).or_insert(0);
        *e = e.saturating_add(t.base_amount);
        total_base += t.base_amount as u128;
    }
    out.unique_buyers = by_entity.len() as u32;
    out.breadth_bps = ((out.unique_buyers as u128 * 10_000) / out.tx_count as u128) as u32;

    // Per-buyer accumulation distribution: peak, and top-N concentration.
    let mut holdings: Vec<u64> = by_entity.values().copied().collect();
    holdings.sort_unstable_by(|a, b| b.cmp(a)); // descending
    out.max_per_buyer_base = holdings.first().copied().unwrap_or(0);
    let top_sum: u128 = holdings.iter().take(top_n).map(|&h| h as u128).sum();
    out.top_n_concentration_bps = (top_sum * 10_000).checked_div(total_base).unwrap_or(0) as u32;

    // Tier-progression velocity in bps/sec of curve progress.
    out.tier_velocity_bps_per_s = if out.duration_ms == 0 {
        0
    } else {
        let dp = end_progress_bps as i128 - start_progress_bps as i128;
        (dp * 1_000 / out.duration_ms as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64
    };
    out
}

/// A first-slot transaction by *another* participant at token creation.
///
/// Responsibility: atomic unit of the creation-window competition family
/// (§21.7 adverse-selection meter). `tipper_entity` is the Section 28
/// deduplicated cluster id. Constitution §22: integer lamports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSlotTx {
    /// Section 28 entity-deduplicated tipper cluster id.
    pub tipper_entity: u64,
    /// Priority fee paid (lamports).
    pub priority_fee_lamports: u64,
    /// Additional tip paid (e.g. Jito bundle tip), lamports.
    pub tip_lamports: u64,
    /// Whether this tx participated in a bundle.
    pub is_bundle: bool,
    /// Whether the entity is a known Tier-2 sniper-cohort wallet.
    pub is_known_sniper: bool,
}

/// Creation-window competition statistics (§21.7).
///
/// Responsibility: the adverse-selection meter for early-entry lanes —
/// evaluator-weighed, two-sided by construction, **never a binary veto**.
/// Constitution §22: integer lamports / bps counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CreationWindowStats {
    /// Number of first-slot competitor transactions.
    pub tx_count: u32,
    /// Distinct Section 28 tipper entities (unique tippers).
    pub unique_tippers: u32,
    /// Maximum combined competitive spend (priority_fee + tip), lamports.
    pub max_spend_lamports: u64,
    /// Mean combined competitive spend (integer `sum / count`), lamports.
    pub mean_spend_lamports: u64,
    /// Bundle participation in bps: `bundle_txs * 10_000 / tx_count`.
    pub bundle_participation_bps: u32,
    /// Count of transactions from known Tier-2 sniper-cohort wallets.
    pub sniper_cohort_count: u32,
}

/// Analyse the creation-window competitor set into its §21.7 statistics.
///
/// Combined competitive spend is `priority_fee_lamports + tip_lamports` per tx.
/// Empty input yields all-zero. Interpretation is two-sided (hot launch vs
/// insider extraction) and is left to the supervisor's evaluator.
///
/// Responsibility: single entry point for creation-window competition (§21.7).
/// Constitution §22: `saturating_add` on spend, `u128` widening on mean,
/// division guards on empty input.
pub fn analyze_creation_window(txs: &[FirstSlotTx]) -> CreationWindowStats {
    let mut out = CreationWindowStats::default();
    if txs.is_empty() {
        return out;
    }
    out.tx_count = txs.len() as u32;
    let mut tippers: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut sum: u128 = 0;
    let mut bundle_txs: u32 = 0;
    for t in txs {
        let spend = t.priority_fee_lamports.saturating_add(t.tip_lamports);
        out.max_spend_lamports = out.max_spend_lamports.max(spend);
        sum += spend as u128;
        tippers.insert(t.tipper_entity);
        if t.is_bundle {
            bundle_txs += 1;
        }
        if t.is_known_sniper {
            out.sniper_cohort_count += 1;
        }
    }
    out.unique_tippers = tippers.len() as u32;
    out.mean_spend_lamports = (sum / out.tx_count as u128) as u64;
    out.bundle_participation_bps = (bundle_txs as u128 * 10_000 / out.tx_count as u128) as u32;
    out
}
