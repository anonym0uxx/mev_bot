//! `baseline_family` — the §52 deterministic baseline-strategy family.
//!
//! §52 requires a challenger to defeat not one but a *family* of naive baselines
//! computed over the SAME recorded decision tape and fee model. Today only a
//! buy-every-confirm-hold baseline exists; this module adds the rest —
//! random-eligible-entry, buy-every-launch, threshold-only, fixed-TP/SL, and
//! hold-to-death — and a [`run_family`] runner that returns per-baseline net-SOL
//! so the family-wise-margin verdict ([`crate::baseline_destruction`]) can run
//! against the whole field at once.
//!
//! Every baseline is a *pure function* over supplied event/outcome vectors: it
//! decides which recorded events it would have entered and sums the pre-computed
//! outcome each entered event carries under the relevant exit rule, less the fee
//! model's per-entry cost. There is no RNG anywhere — the "random"-eligible
//! baseline selects via a supplied index-hash (FNV-1a of the event index), not a
//! generator, so the selection is byte-for-byte reproducible (§22). Money is
//! `i128` lamports; no floats.

/// FNV-1a over the wrapping 2^64 ring — the *supplied index-hash* the
/// random-eligible baseline samples with. This is a hash, not an RNG: the same
/// index always maps to the same value, so the selection is fully deterministic.
fn index_hash(index: u64) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET_BASIS;
    for &b in &index.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// One recorded decision-tape event with its pre-computed outcomes under each
/// exit rule. A baseline picks *whether* to enter; the outcome it realizes is one
/// of these already-reconciled fields (so the tape, not the baseline, owns the
/// price path — the baselines stay pure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapeEvent {
    /// Stable event index (drives the deterministic index-hash selection).
    pub index: u64,
    /// Whether the event was an *eligible* entry candidate at all.
    pub eligible: bool,
    /// Whether the event was a fresh launch/confirmation.
    pub launch: bool,
    /// The event's decision score (threshold baselines compare against this).
    pub score: i64,
    /// Net lamports if entered and held to death (terminal price).
    pub net_hold_to_death: i128,
    /// Net lamports if entered under a fixed take-profit / stop-loss rule.
    pub net_fixed_tpsl: i128,
}

impl TapeEvent {
    /// Test/golden-vector constructor.
    pub fn test(
        index: u64,
        eligible: bool,
        launch: bool,
        score: i64,
        net_hold: i128,
        net_tpsl: i128,
    ) -> Self {
        TapeEvent {
            index,
            eligible,
            launch,
            score,
            net_hold_to_death: net_hold,
            net_fixed_tpsl: net_tpsl,
        }
    }
}

/// Per-entry cost model applied uniformly to every baseline (§52: baselines
/// share the challenger's fee model so the comparison is apples-to-apples).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeModel {
    /// Lamports charged per entered position (fees + tips + expected failed-cost).
    pub per_entry_lamports: u128,
}

impl FeeModel {
    /// Constructor.
    pub fn new(per_entry_lamports: u128) -> Self {
        FeeModel { per_entry_lamports }
    }
}

/// Tunables that parameterize the family without introducing any randomness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FamilyParams {
    /// Random-eligible-entry stride: enter an eligible event iff
    /// `index_hash(index) % k == phase`. `k == 0` is treated as `1` (enter all).
    pub sample_k: u64,
    /// Random-eligible-entry residue class selected.
    pub sample_phase: u64,
    /// Threshold-only entry bar: enter iff `score >= score_threshold`.
    pub score_threshold: i64,
}

impl FamilyParams {
    /// A conventional default: every 4th eligible event, phase 0, threshold 0.
    pub fn default_params() -> Self {
        FamilyParams {
            sample_k: 4,
            sample_phase: 0,
            score_threshold: 0,
        }
    }
}

/// The naive baselines §52 measures a challenger against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BaselineKind {
    /// Enter every eligible event selected by the supplied index-hash stride,
    /// realize the hold-to-death outcome.
    RandomEligibleEntry,
    /// Enter every launch/confirmation event, realize hold-to-death.
    BuyEveryLaunch,
    /// Enter every event whose score clears the threshold, realize hold-to-death.
    ThresholdOnly,
    /// Enter every eligible event, realize the fixed-TP/SL outcome.
    FixedTpSl,
    /// Enter every eligible event, realize hold-to-death (the pre-existing
    /// buy-every-confirm-hold baseline, generalized).
    HoldToDeath,
}

impl BaselineKind {
    /// The full family, in deterministic order.
    pub const ALL: [BaselineKind; 5] = [
        BaselineKind::RandomEligibleEntry,
        BaselineKind::BuyEveryLaunch,
        BaselineKind::ThresholdOnly,
        BaselineKind::FixedTpSl,
        BaselineKind::HoldToDeath,
    ];

    /// Would this baseline enter `event` under `params`? Pure, RNG-free.
    fn enters(&self, event: &TapeEvent, params: &FamilyParams) -> bool {
        match self {
            BaselineKind::RandomEligibleEntry => {
                if !event.eligible {
                    return false;
                }
                let k = params.sample_k.max(1);
                index_hash(event.index) % k == params.sample_phase % k
            }
            BaselineKind::BuyEveryLaunch => event.launch,
            BaselineKind::ThresholdOnly => event.score >= params.score_threshold,
            BaselineKind::FixedTpSl => event.eligible,
            BaselineKind::HoldToDeath => event.eligible,
        }
    }

    /// The pre-computed outcome field this baseline realizes on an entered event.
    fn outcome(&self, event: &TapeEvent) -> i128 {
        match self {
            BaselineKind::FixedTpSl => event.net_fixed_tpsl,
            _ => event.net_hold_to_death,
        }
    }
}

/// One baseline's reconciled result over the tape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaselineResult {
    /// Which baseline this is.
    pub kind: BaselineKind,
    /// Number of events this baseline entered.
    pub entries: u32,
    /// Gross lamports summed over entered events (before fees).
    pub gross_lamports: i128,
    /// Total fee lamports = `per_entry * entries`.
    pub fee_lamports: i128,
    /// Net lamports = gross − fees.
    pub net_lamports: i128,
}

/// Run one baseline over the tape (§52). Pure and deterministic.
///
/// Sums the pre-computed outcome each *entered* event carries and subtracts the
/// fee model's per-entry cost. A baseline that enters nothing yields an all-zero
/// result (an honest empty, not a fabricated profit).
pub fn run_baseline(
    kind: BaselineKind,
    events: &[TapeEvent],
    fee: &FeeModel,
    params: &FamilyParams,
) -> BaselineResult {
    let mut entries: u32 = 0;
    let mut gross: i128 = 0;
    for e in events {
        if kind.enters(e, params) {
            entries += 1;
            gross = gross
                .checked_add(kind.outcome(e))
                .expect("run_baseline: gross i128 overflow");
        }
    }
    let fee_lamports = (fee.per_entry_lamports as i128)
        .checked_mul(entries as i128)
        .expect("run_baseline: fee i128 overflow");
    let net = gross
        .checked_sub(fee_lamports)
        .expect("run_baseline: net i128 overflow");
    BaselineResult {
        kind,
        entries,
        gross_lamports: gross,
        fee_lamports,
        net_lamports: net,
    }
}

/// Run the whole baseline family (§52).
///
/// Returns one [`BaselineResult`] per [`BaselineKind::ALL`], in that fixed order,
/// so the caller can hand every baseline's net-SOL to the family-wise-margin
/// destruction verdict at once. Deterministic; pure over the supplied vectors.
pub fn run_family(
    events: &[TapeEvent],
    fee: &FeeModel,
    params: &FamilyParams,
) -> Vec<BaselineResult> {
    BaselineKind::ALL
        .iter()
        .map(|&kind| run_baseline(kind, events, fee, params))
        .collect()
}

/// Convenience: the family's net-SOL vector as
/// [`crate::baseline_destruction::Competitor`] baselines, ready for
/// [`crate::baseline_destruction::baseline_destruction`]. Preserves family order.
pub fn as_competitors(results: &[BaselineResult]) -> Vec<crate::baseline_destruction::Competitor> {
    results
        .iter()
        .map(|r| crate::baseline_destruction::Competitor::baseline(r.net_lamports))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tape() -> Vec<TapeEvent> {
        vec![
            // idx, eligible, launch, score, hold, tpsl
            TapeEvent::test(0, true, true, 10, 5_000, 3_000),
            TapeEvent::test(1, true, false, -5, -2_000, -1_000),
            TapeEvent::test(2, true, true, 20, 9_000, 4_000),
            TapeEvent::test(3, false, false, 100, 50_000, 50_000),
        ]
    }

    #[test]
    fn hold_to_death_enters_all_eligible() {
        let fee = FeeModel::new(100);
        let r = run_baseline(
            BaselineKind::HoldToDeath,
            &tape(),
            &fee,
            &FamilyParams::default_params(),
        );
        // eligible events: 0,1,2 -> gross 5000-2000+9000 = 12000, fee 300.
        assert_eq!(r.entries, 3);
        assert_eq!(r.gross_lamports, 12_000);
        assert_eq!(r.fee_lamports, 300);
        assert_eq!(r.net_lamports, 11_700);
    }

    #[test]
    fn buy_every_launch_only_launches() {
        let fee = FeeModel::new(0);
        let r = run_baseline(
            BaselineKind::BuyEveryLaunch,
            &tape(),
            &fee,
            &FamilyParams::default_params(),
        );
        // launches: 0,2 -> 5000+9000 = 14000.
        assert_eq!(r.entries, 2);
        assert_eq!(r.net_lamports, 14_000);
    }

    #[test]
    fn threshold_only_filters_by_score() {
        let fee = FeeModel::new(0);
        let params = FamilyParams {
            score_threshold: 15,
            ..FamilyParams::default_params()
        };
        let r = run_baseline(BaselineKind::ThresholdOnly, &tape(), &fee, &params);
        // score>=15: event 2 (20) and event 3 (100).
        assert_eq!(r.entries, 2);
        assert_eq!(r.gross_lamports, 9_000 + 50_000);
    }

    #[test]
    fn fixed_tpsl_uses_tpsl_outcome() {
        let fee = FeeModel::new(0);
        let r = run_baseline(
            BaselineKind::FixedTpSl,
            &tape(),
            &fee,
            &FamilyParams::default_params(),
        );
        // eligible 0,1,2 tpsl: 3000-1000+4000 = 6000.
        assert_eq!(r.gross_lamports, 6_000);
    }

    #[test]
    fn random_eligible_is_deterministic_and_rng_free() {
        let fee = FeeModel::new(0);
        let params = FamilyParams {
            sample_k: 2,
            sample_phase: 0,
            ..FamilyParams::default_params()
        };
        let a = run_baseline(BaselineKind::RandomEligibleEntry, &tape(), &fee, &params);
        let b = run_baseline(BaselineKind::RandomEligibleEntry, &tape(), &fee, &params);
        assert_eq!(a, b);
        // Only eligible events are candidates; entries <= 3.
        assert!(a.entries <= 3);
    }

    #[test]
    fn random_eligible_k1_enters_all_eligible() {
        let fee = FeeModel::new(0);
        let params = FamilyParams {
            sample_k: 1,
            sample_phase: 0,
            ..FamilyParams::default_params()
        };
        let r = run_baseline(BaselineKind::RandomEligibleEntry, &tape(), &fee, &params);
        // k==1 -> every hash % 1 == 0 -> all eligible (0,1,2).
        assert_eq!(r.entries, 3);
    }

    #[test]
    fn family_runs_all_in_order() {
        let fee = FeeModel::new(10);
        let fam = run_family(&tape(), &fee, &FamilyParams::default_params());
        assert_eq!(fam.len(), 5);
        assert_eq!(fam[0].kind, BaselineKind::RandomEligibleEntry);
        assert_eq!(fam[4].kind, BaselineKind::HoldToDeath);
        let comps = as_competitors(&fam);
        assert_eq!(comps.len(), 5);
    }

    #[test]
    fn empty_tape_is_all_zero() {
        let fee = FeeModel::new(999);
        let r = run_baseline(
            BaselineKind::HoldToDeath,
            &[],
            &fee,
            &FamilyParams::default_params(),
        );
        assert_eq!(r.entries, 0);
        assert_eq!(r.net_lamports, 0);
    }

    #[test]
    fn index_hash_is_stable() {
        assert_eq!(index_hash(0), index_hash(0));
        assert_ne!(index_hash(0), index_hash(1));
    }
}
