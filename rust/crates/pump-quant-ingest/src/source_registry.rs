//! Source classification and observation-source-mix labeling (leaf
//! `in_source_registry`).
//!
//! Responsibility: implement the constitution's source-authority /
//! source-lifecycle taxonomy as pure functions.
//!   - §14.5: repository Jito ShredStream code is classified TRANSITIONAL;
//!     Helius WS-era code is LEGACY pending the LaserStream gRPC adapter.
//!   - §15: source-authority levels (earliest signal / structured observation /
//!     canonical repair / reconciled execution) — never collapsed.
//!   - §16: observation-source-mix labels; multiple recorded feeds collapse to
//!     `DualOrMultiFeedRecorded`, never silently pooled to a single source.
//!
//! All pure, total, deterministic functions — no floats, no I/O (§22).

/// A source identity known to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceId {
    /// Jito ShredStream earliest-source shred feed (sunset-bound, §18.3).
    JitoShredStream,
    /// A successor earliest-source shred feed (e.g. DoubleZero / Helius Shred
    /// Delivery), §18.3.4.
    SuccessorShred,
    /// Legacy Helius WebSocket `logsSubscribe` feed (pre-LaserStream).
    HeliusWsLogs,
    /// Helius LaserStream gRPC mainnet — structured observation truth (§15 L2).
    HeliusLaserStream,
    /// Helius provider replay of LaserStream observations (§16 / §18.6).
    HeliusProviderReplay,
    /// Canonical Solana/Helius RPC used for repair (§15 L3).
    CanonicalRpc,
    /// Reconciled finalized execution truth for the system's own txs (§15 L4).
    ReconciledExecution,
}

/// Registry classification of a source (§14.5 / §15 / §18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceClass {
    /// Sunset-bound earliest source (announced shutdown), §14.5 / §18.3.
    Transitional,
    /// Ongoing earliest-source shred feed with no announced sunset, §18.3.4.
    Successor,
    /// Helius LaserStream gRPC mainnet — the structured-observation primary.
    StructuredPrimary,
    /// Helius WS-era feed retained only until LaserStream replaces it (§14.5).
    Legacy,
    /// Canonical RPC repair source (§15 L3).
    CanonicalRepair,
    /// Reconciled finalized execution truth (§15 L4).
    ReconciledTruth,
}

/// Observation-source-mix label (§16). Each dataset/result row must preserve
/// which source(s) produced it; timing claims across labels are non-equivalent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixLabel {
    /// `HELIUS_LASERSTREAM_LIVE`.
    HeliusLaserStreamLive,
    /// `HELIUS_PROVIDER_REPLAY`.
    HeliusProviderReplay,
    /// `JITO_TRANSITIONAL_LIVE`.
    JitoTransitionalLive,
    /// `SUCCESSOR_SHRED_LIVE`.
    SuccessorShredLive,
    /// `CANONICAL_RPC_REPAIR`.
    CanonicalRpcRepair,
    /// `RECONCILED_LIVE_EXECUTION`.
    ReconciledLiveExecution,
    /// `DUAL_OR_MULTI_FEED_RECORDED` — more than one distinct feed recorded.
    DualOrMultiFeedRecorded,
    /// `LIVE_SHADOW_RECORDED` — a shadow (paper) row recorded against live
    /// feeds: real signal timing, feature availability, and decision/build
    /// latency, but *simulated* landing counterfactuals (no on-chain fill).
    ///
    /// This is a recording-*mode* label, not a feed-source label: it has no
    /// originating [`SourceId`] (shadow mode can sit atop any live feed), so
    /// [`mix_label_for`] never yields it. It is attached to rows produced in
    /// shadow mode and combined via [`combine_mix_labels`], where it is
    /// *absorbing* (see that function) — its simulated-landing caveat caps what
    /// the whole row may claim (§16 timing-claim non-equivalence).
    LiveShadowRecorded,
}

/// Subscription-filter breadth (number of distinct on-chain accounts/programs a
/// LaserStream-class filter matches) at or above which the subscription is
/// treated as **broad/costly** and may only be armed with an active cost
/// monitor (§72 fail-closed cost governance).
///
/// A per-mint or small-account-set filter sits well below this; a program-wide
/// firehose (which expands to "every account the program touches") is encoded by
/// the caller as a breadth at or above it. The value is a named constant, not a
/// magic literal, so the arm-gate threshold is auditable and change-controlled.
pub const BROAD_FILTER_BREADTH_THRESHOLD: u32 = 64;

/// Why a subscription was refused arming by [`may_arm`] (§72).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmRefusal {
    /// A broad/costly subscription (`breadth >= threshold`) was requested while
    /// no cost monitor is registered/active. Fail-closed: an unmetered firehose
    /// can never be armed. Carries the offending breadth and the threshold.
    CostMonitorRequired {
        /// The requested filter breadth.
        breadth: u32,
        /// The breadth threshold at/above which a cost monitor is mandatory.
        threshold: u32,
    },
}

/// Fail-closed cost-monitor arm-gate for LaserStream-class subscriptions (§72).
///
/// Refuses to arm a **broad/costly** subscription filter
/// (`filter_breadth >= `[`BROAD_FILTER_BREADTH_THRESHOLD`]) unless a cost monitor
/// is registered and active. This is the *gate* only — a pure boolean check that
/// runs anywhere (laptop or server); the live cost telemetry it presupposes is
/// Phase-B. A **narrow** filter always arms (its cost is inherently bounded), and
/// any filter arms when the monitor is active. Absence of a monitor can never let
/// an unmetered firehose through: unknown ≙ refused.
///
/// Pure, total, deterministic (§22): no floats, no I/O, no wall-clock.
pub fn may_arm(filter_breadth: u32, cost_monitor_active: bool) -> Result<(), ArmRefusal> {
    let broad = filter_breadth >= BROAD_FILTER_BREADTH_THRESHOLD;
    if broad && !cost_monitor_active {
        return Err(ArmRefusal::CostMonitorRequired {
            breadth: filter_breadth,
            threshold: BROAD_FILTER_BREADTH_THRESHOLD,
        });
    }
    Ok(())
}

/// Whether a source is an earliest-source shred feed (Jito or a successor).
/// These are the sources subject to the sunset rule.
fn is_earliest_shred(id: SourceId) -> bool {
    matches!(id, SourceId::JitoShredStream | SourceId::SuccessorShred)
}

/// Classify a source (§14.5).
///
/// For earliest-source shred feeds the classification depends on
/// `announced_sunset`: an announced shutdown makes the source
/// [`SourceClass::Transitional`]; without one it is a viable
/// [`SourceClass::Successor`]. Per §18.3.1 Jito's sunset is a *verified fact*,
/// so callers pass `announced_sunset = true` for `JitoShredStream`, which yields
/// `Transitional` as §14.5 mandates. `announced_sunset` is ignored for
/// non-earliest sources (their class is fixed).
pub fn classify_source(id: SourceId, announced_sunset: bool) -> SourceClass {
    if is_earliest_shred(id) {
        return if announced_sunset {
            SourceClass::Transitional
        } else {
            SourceClass::Successor
        };
    }
    match id {
        SourceId::HeliusWsLogs => SourceClass::Legacy,
        SourceId::HeliusLaserStream => SourceClass::StructuredPrimary,
        SourceId::HeliusProviderReplay => SourceClass::StructuredPrimary,
        SourceId::CanonicalRpc => SourceClass::CanonicalRepair,
        SourceId::ReconciledExecution => SourceClass::ReconciledTruth,
        // Earliest-shred sources handled above.
        SourceId::JitoShredStream | SourceId::SuccessorShred => unreachable!(),
    }
}

/// The single-source observation-mix label for a source (§16), or `None` for a
/// source that has no canonical live-mix label (the pre-LaserStream Helius WS
/// feed is legacy and is not used for dataset source-mix labeling).
pub fn mix_label_for(id: SourceId) -> Option<MixLabel> {
    match id {
        SourceId::JitoShredStream => Some(MixLabel::JitoTransitionalLive),
        SourceId::SuccessorShred => Some(MixLabel::SuccessorShredLive),
        SourceId::HeliusLaserStream => Some(MixLabel::HeliusLaserStreamLive),
        SourceId::HeliusProviderReplay => Some(MixLabel::HeliusProviderReplay),
        SourceId::CanonicalRpc => Some(MixLabel::CanonicalRpcRepair),
        SourceId::ReconciledExecution => Some(MixLabel::ReconciledLiveExecution),
        SourceId::HeliusWsLogs => None,
    }
}

/// Combine the per-source labels of everything that produced a record into a
/// single dataset mix label (§16).
///
/// ## Precedence lattice (§16 timing-claim non-equivalence)
/// The combine follows a *safe-direction* lattice: the combined label always
/// reports the **weakest (most caveated)** claim the contributing rows can
/// jointly support, never the strongest. From highest precedence down:
///
/// 1. [`MixLabel::LiveShadowRecorded`] is **absorbing** — if *any* contributing
///    row is shadow-recorded, the whole record is `LiveShadowRecorded`. A row
///    that carries simulated landing counterfactuals can never be presented as
///    a pure live-feed dataset, so shadow provenance taints the entire record;
///    collapsing it to `DualOrMultiFeedRecorded` would *over*claim live
///    fidelity, which §16 forbids.
/// 2. two or more distinct (non-shadow) labels →
///    [`MixLabel::DualOrMultiFeedRecorded`] (feed disagreement is preserved as
///    a multi-feed label, never silently collapsed to one provider's
///    interpretation).
/// 3. exactly one distinct label → that label preserved.
/// 4. empty input → `None`.
pub fn combine_mix_labels(labels: &[MixLabel]) -> Option<MixLabel> {
    if labels.is_empty() {
        return None;
    }
    // §16 shadow-absorbing rule: a shadow-recorded contribution caps the whole
    // record's claim regardless of how many live feeds also contributed.
    if labels.contains(&MixLabel::LiveShadowRecorded) {
        return Some(MixLabel::LiveShadowRecorded);
    }
    let mut distinct: Vec<MixLabel> = Vec::new();
    for &l in labels {
        if !distinct.contains(&l) {
            distinct.push(l);
        }
    }
    match distinct.len() {
        1 => Some(distinct[0]),
        _ => Some(MixLabel::DualOrMultiFeedRecorded),
    }
}

#[cfg(test)]
mod arm_gate_tests {
    use super::*;

    #[test]
    fn broad_without_monitor_refuses() {
        // A firehose-class breadth with no cost monitor is refused (fail-closed).
        let breadth = BROAD_FILTER_BREADTH_THRESHOLD;
        assert_eq!(
            may_arm(breadth, false),
            Err(ArmRefusal::CostMonitorRequired {
                breadth,
                threshold: BROAD_FILTER_BREADTH_THRESHOLD,
            })
        );
        // Well above the threshold is refused just the same.
        assert!(may_arm(BROAD_FILTER_BREADTH_THRESHOLD + 10_000, false).is_err());
    }

    #[test]
    fn broad_with_monitor_arms() {
        // The same broad filter arms once a cost monitor is active.
        assert_eq!(may_arm(BROAD_FILTER_BREADTH_THRESHOLD, true), Ok(()));
        assert_eq!(may_arm(u32::MAX, true), Ok(()));
    }

    #[test]
    fn narrow_always_arms() {
        // Below the threshold, arming never depends on the monitor.
        let narrow = BROAD_FILTER_BREADTH_THRESHOLD - 1;
        assert_eq!(may_arm(narrow, false), Ok(()));
        assert_eq!(may_arm(narrow, true), Ok(()));
        assert_eq!(may_arm(0, false), Ok(()));
        assert_eq!(may_arm(1, false), Ok(()));
    }

    #[test]
    fn threshold_boundary_is_inclusive_broad() {
        // At exactly the threshold the filter is broad (refused without a
        // monitor); one below is narrow (always arms).
        assert!(may_arm(BROAD_FILTER_BREADTH_THRESHOLD, false).is_err());
        assert_eq!(may_arm(BROAD_FILTER_BREADTH_THRESHOLD - 1, false), Ok(()));
    }
}
