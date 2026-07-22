//! `convexity_ledger` — unified per-rule convexity-preservation ledger
//! (constitution §49).
//!
//! §49 requires a *single* frozen ledger that scores every suppression-class rule
//! — vetoes, confidence-reducers, EntryMode rules, entry-zone / setup / social /
//! creator / cluster gates, late-entry aborts, economic gates, exit policies,
//! partial de-risks, moonbag rules — on the SAME two-sided ruler: losses avoided
//! vs runners missed, right-tail preserved vs destroyed, MFE captured vs killed,
//! top-1/5/10% participation, and net SOL saved vs forgone. The existing
//! primitives (`prfs_fold`, `topk_excision`, `mfe_capture`) each measure one
//! sliver keyed by gate or lane; none keys by a rule id spanning the full rule
//! set. This is that ledger.
//!
//! §22: integer / bps only, deterministic. Every event carries the counterfactual
//! outcome of the *full, unsuppressed* position; the ledger folds both what a
//! firing rule saved and what it cost. No floats, no wall-clock, no RNG; grouping
//! is a `BTreeMap` over a stable rule id.

use std::collections::BTreeMap;

/// The family of rule a ledger entry belongs to (§49's full rule set).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleKind {
    /// Hard veto (blocks entry).
    Veto,
    /// Confidence-reducer (shrinks size).
    ConfidenceReducer,
    /// EntryMode selection rule.
    EntryMode,
    /// Entry-zone gate.
    EntryZone,
    /// Setup / pattern gate.
    Setup,
    /// Social-signal gate.
    Social,
    /// Creator-reputation gate.
    Creator,
    /// Cluster / co-holder gate.
    Cluster,
    /// Late-entry abort.
    LateEntryAbort,
    /// Economic gate (cost/EV).
    EconomicGate,
    /// Exit policy rule.
    ExitPolicy,
    /// Partial de-risk rule.
    PartialDeRisk,
    /// Moonbag-retention rule.
    Moonbag,
}

/// Stable identifier for one rule: its kind plus an opaque within-kind id.
/// Ordering drives deterministic per-rule output order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleId {
    /// Rule family.
    pub kind: RuleKind,
    /// Opaque within-kind discriminator.
    pub id: u64,
}

impl RuleId {
    /// Constructor.
    pub fn new(kind: RuleKind, id: u64) -> Self {
        RuleId { kind, id }
    }
}

/// One convexity observation for a rule: the counterfactual full-position
/// outcome and whether the rule suppressed (removed / shrank) the exposure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConvexityEvent {
    /// Which rule this event scores.
    pub rule: RuleId,
    /// True iff the rule suppressed the position (vetoed entry, cut the exit,
    /// de-risked). False iff the rule allowed full participation.
    pub suppressed: bool,
    /// Net outcome, bps, of the FULL unsuppressed position (the counterfactual).
    pub counterfactual_bps: i64,
    /// Net outcome, bps, actually realized under the rule's action (≈ 0 for a
    /// veto that removed the position; ≈ counterfactual when allowed).
    pub realized_bps: i64,
    /// Max favorable excursion, bps, of the underlying (for MFE captured/killed).
    pub mfe_bps: i64,
}

impl ConvexityEvent {
    /// Test/golden-vector constructor.
    pub fn test(
        rule: RuleId,
        suppressed: bool,
        counterfactual_bps: i64,
        realized_bps: i64,
        mfe_bps: i64,
    ) -> Self {
        ConvexityEvent {
            rule,
            suppressed,
            counterfactual_bps,
            realized_bps,
            mfe_bps,
        }
    }

    /// Full-tuple ordering key used to make top-k selection deterministic even
    /// under counterfactual ties.
    fn sort_key(&self) -> (i64, i64, i64, bool) {
        // Descending counterfactual first (negate), then realized, then mfe,
        // then allowed-before-suppressed.
        (
            -self.counterfactual_bps,
            -self.realized_bps,
            -self.mfe_bps,
            self.suppressed,
        )
    }
}

/// Per-rule two-sided convexity ledger (§49).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleLedger {
    /// The rule this ledger scores.
    pub rule: RuleId,
    /// Total events observed for the rule.
    pub n: u32,
    /// Events where the rule suppressed exposure.
    pub suppressed_n: u32,
    /// Losses avoided: `Σ max(0, −counterfactual)` over suppressed events (bps).
    /// This is also the "net SOL saved" side.
    pub losses_avoided_bps: i128,
    /// Net SOL forgone: `Σ max(0, counterfactual)` over suppressed events (bps).
    pub net_forgone_bps: i128,
    /// Runners missed: suppressed events whose counterfactual `≥ runner_threshold`.
    pub runners_missed: u32,
    /// Sum of counterfactual bps over those missed runners.
    pub runners_missed_bps: i128,
    /// MFE killed: `Σ mfe` over suppressed events (favorable excursion discarded).
    pub mfe_killed_bps: i128,
    /// MFE captured: `Σ max(0, realized)` over allowed events.
    pub mfe_captured_bps: i128,
    /// Right tail preserved: `Σ counterfactual` over top-10% events that were
    /// ALLOWED (convexity kept).
    pub right_tail_preserved_bps: i128,
    /// Right tail destroyed: `Σ counterfactual` over top-10% events that were
    /// SUPPRESSED (convexity killed).
    pub right_tail_destroyed_bps: i128,
    /// Top-1% participation: allowed / total in the top-1% by counterfactual.
    pub top1_participated: u32,
    /// Total events in the top-1%.
    pub top1_total: u32,
    /// Top-5% participation count.
    pub top5_participated: u32,
    /// Total events in the top-5%.
    pub top5_total: u32,
    /// Top-10% participation count.
    pub top10_participated: u32,
    /// Total events in the top-10%.
    pub top10_total: u32,
}

impl RuleLedger {
    /// Net convexity contribution: losses avoided minus net forgone (bps). A
    /// positive value means the rule saved more downside than it cost in upside.
    pub fn net_convexity_bps(&self) -> i128 {
        self.losses_avoided_bps - self.net_forgone_bps
    }
}

/// Top-`pct`% event count of `n`: `ceil(n·pct/100)`, at least 1 when `n > 0`.
fn top_count(n: usize, pct: u64) -> usize {
    if n == 0 {
        return 0;
    }
    ((n as u64 * pct).div_ceil(100) as usize).clamp(1, n)
}

/// Build the unified per-rule convexity-preservation ledger (§49).
///
/// Events are grouped by [`RuleId`]; within each rule they are ranked by
/// counterfactual outcome (deterministic full-tuple tie-break) to compute
/// top-1/5/10% participation and right-tail preserved-vs-destroyed. A firing
/// rule's downside saved (`losses_avoided`) and upside cost (`net_forgone`,
/// `runners_missed`, `mfe_killed`) are folded two-sidedly so no rule is ever
/// scored on avoided losses alone. `runner_threshold_bps` sets how large a
/// missed winner must be to count as a "runner". Output is ordered by
/// [`RuleId`]; deterministic. Empty input -> empty vector.
pub fn build_ledger(events: &[ConvexityEvent], runner_threshold_bps: i64) -> Vec<RuleLedger> {
    // Group events by rule (deterministic key order).
    let mut groups: BTreeMap<RuleId, Vec<ConvexityEvent>> = BTreeMap::new();
    for e in events {
        groups.entry(e.rule).or_default().push(*e);
    }

    let mut out: Vec<RuleLedger> = Vec::with_capacity(groups.len());
    for (rule, mut evs) in groups {
        let n = evs.len();

        // Two-sided fold over all events.
        let mut suppressed_n: u32 = 0;
        let mut losses_avoided: i128 = 0;
        let mut net_forgone: i128 = 0;
        let mut runners_missed: u32 = 0;
        let mut runners_missed_bps: i128 = 0;
        let mut mfe_killed: i128 = 0;
        let mut mfe_captured: i128 = 0;
        for e in &evs {
            if e.suppressed {
                suppressed_n += 1;
                if e.counterfactual_bps < 0 {
                    losses_avoided += (-(e.counterfactual_bps as i128)).max(0);
                } else {
                    net_forgone += e.counterfactual_bps as i128;
                }
                if e.counterfactual_bps >= runner_threshold_bps {
                    runners_missed += 1;
                    runners_missed_bps += e.counterfactual_bps as i128;
                }
                mfe_killed += e.mfe_bps as i128;
            } else {
                mfe_captured += e.realized_bps.max(0) as i128;
            }
        }

        // Rank by counterfactual for tail participation (deterministic).
        evs.sort_by_key(|a| a.sort_key());

        let t1 = top_count(n, 1);
        let t5 = top_count(n, 5);
        let t10 = top_count(n, 10);

        let count_participated =
            |k: usize| -> u32 { evs.iter().take(k).filter(|e| !e.suppressed).count() as u32 };
        let top1_participated = count_participated(t1);
        let top5_participated = count_participated(t5);
        let top10_participated = count_participated(t10);

        // Right tail (top-10%) preserved vs destroyed by counterfactual sum.
        let mut preserved: i128 = 0;
        let mut destroyed: i128 = 0;
        for e in evs.iter().take(t10) {
            if e.suppressed {
                destroyed += e.counterfactual_bps as i128;
            } else {
                preserved += e.counterfactual_bps as i128;
            }
        }

        out.push(RuleLedger {
            rule,
            n: n as u32,
            suppressed_n,
            losses_avoided_bps: losses_avoided,
            net_forgone_bps: net_forgone,
            runners_missed,
            runners_missed_bps,
            mfe_killed_bps: mfe_killed,
            mfe_captured_bps: mfe_captured,
            right_tail_preserved_bps: preserved,
            right_tail_destroyed_bps: destroyed,
            top1_participated,
            top1_total: t1 as u32,
            top5_participated,
            top5_total: t5 as u32,
            top10_participated,
            top10_total: t10 as u32,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: u64) -> RuleId {
        RuleId::new(RuleKind::Veto, id)
    }

    #[test]
    fn two_sided_veto_accounting() {
        // Veto 1: suppressed a −5000 loser (loss avoided) and a +8000 winner
        // (runner missed / forgone).
        let evs = vec![
            ConvexityEvent::test(r(1), true, -5_000, 0, 100),
            ConvexityEvent::test(r(1), true, 8_000, 0, 9_000),
        ];
        let led = build_ledger(&evs, 5_000);
        assert_eq!(led.len(), 1);
        let l = led[0];
        assert_eq!(l.n, 2);
        assert_eq!(l.suppressed_n, 2);
        assert_eq!(l.losses_avoided_bps, 5_000);
        assert_eq!(l.net_forgone_bps, 8_000);
        assert_eq!(l.runners_missed, 1); // +8000 >= 5000
        assert_eq!(l.runners_missed_bps, 8_000);
        assert_eq!(l.mfe_killed_bps, 9_100);
        assert_eq!(l.net_convexity_bps(), 5_000 - 8_000);
    }

    #[test]
    fn allowed_events_capture_mfe_and_preserve_tail() {
        // Rule allowed a big winner (participated) -> right tail preserved.
        let evs = vec![
            ConvexityEvent::test(r(2), false, 12_000, 11_000, 15_000),
            ConvexityEvent::test(r(2), true, -3_000, 0, 200),
        ];
        let led = build_ledger(&evs, 5_000);
        let l = led[0];
        assert_eq!(l.mfe_captured_bps, 11_000);
        assert_eq!(l.losses_avoided_bps, 3_000);
        // top-10% of 2 events = 1 event = the +12000 (allowed) -> preserved.
        assert_eq!(l.top10_total, 1);
        assert_eq!(l.top10_participated, 1);
        assert_eq!(l.right_tail_preserved_bps, 12_000);
        assert_eq!(l.right_tail_destroyed_bps, 0);
    }

    #[test]
    fn suppressed_top_event_destroys_right_tail() {
        // The single biggest counterfactual was suppressed -> destroyed.
        let evs = vec![
            ConvexityEvent::test(r(3), true, 20_000, 0, 25_000),
            ConvexityEvent::test(r(3), false, 1_000, 900, 1_500),
        ];
        let led = build_ledger(&evs, 5_000);
        let l = led[0];
        assert_eq!(l.top10_total, 1);
        assert_eq!(l.top10_participated, 0);
        assert_eq!(l.right_tail_destroyed_bps, 20_000);
        assert_eq!(l.right_tail_preserved_bps, 0);
    }

    #[test]
    fn multiple_rules_ordered_by_id() {
        let evs = vec![
            ConvexityEvent::test(RuleId::new(RuleKind::ExitPolicy, 9), true, -1_000, 0, 0),
            ConvexityEvent::test(RuleId::new(RuleKind::Veto, 1), true, -2_000, 0, 0),
        ];
        let led = build_ledger(&evs, 5_000);
        assert_eq!(led.len(), 2);
        // Veto < ExitPolicy in RuleKind discriminant order.
        assert_eq!(led[0].rule.kind, RuleKind::Veto);
        assert_eq!(led[1].rule.kind, RuleKind::ExitPolicy);
    }

    #[test]
    fn deterministic_repeat() {
        let evs = vec![
            ConvexityEvent::test(r(1), true, 5_000, 0, 6_000),
            ConvexityEvent::test(r(1), false, 5_000, 4_000, 6_000),
            ConvexityEvent::test(r(1), true, -1_000, 0, 100),
        ];
        let a = build_ledger(&evs, 3_000);
        let b = build_ledger(&evs, 3_000);
        assert_eq!(a, b);
    }

    #[test]
    fn empty_is_empty() {
        assert!(build_ledger(&[], 1_000).is_empty());
    }

    #[test]
    fn top_count_rounds_up() {
        assert_eq!(top_count(0, 10), 0);
        assert_eq!(top_count(5, 10), 1); // ceil(0.5) -> 1
        assert_eq!(top_count(100, 1), 1);
        assert_eq!(top_count(100, 10), 10);
        assert_eq!(top_count(3, 100), 3);
    }
}
