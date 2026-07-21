//! `regression_gate` — regression-battery pass/fail aggregation gate
//! (constitution §36, §56.8).
//!
//! Responsibility: a promotion is blocked if *any* regression check fails. This
//! is the thin, deterministic all-must-pass aggregator that sits beside the
//! champion/challenger verdict; the battery's *orchestration* (which checks to
//! run, on what fixtures) stays in the supervisor, but the go/no-go reduction is
//! an evaluator leaf so a single silent failure can never be waved through.
//!
//! Integer-only (constitution §22): ids are opaque `u64`; no floats.

/// Stable identifier for one regression check. Ordering drives deterministic
/// failure-report order only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegressionId(pub u64);

/// The outcome of one regression check in the battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegressionResult {
    /// Which check this is.
    pub id: RegressionId,
    /// True iff the check passed.
    pub passed: bool,
}

impl RegressionResult {
    /// Test/golden-vector constructor.
    pub fn new(id: u64, passed: bool) -> Self {
        RegressionResult {
            id: RegressionId(id),
            passed,
        }
    }
}

/// Aggregate verdict of the regression battery (constitution §36).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateOutcome {
    /// Every check passed — promotion may proceed on this axis.
    Pass,
    /// At least one check failed — promotion blocked. Failing ids are listed in
    /// the order they appear in the input battery.
    Blocked {
        /// Ids of every failing check, in input order.
        failing: Vec<RegressionId>,
    },
}

impl GateOutcome {
    /// True iff the battery passed.
    pub fn passed(&self) -> bool {
        matches!(self, GateOutcome::Pass)
    }
}

/// Aggregate a regression battery into a single go/no-go verdict.
///
/// Responsibility (constitution §36): [`GateOutcome::Pass`] iff **every**
/// result passed; otherwise [`GateOutcome::Blocked`] listing all failing ids in
/// input order. An empty battery passes vacuously — there is no regression to
/// block on — which callers must pair with their own "battery is non-empty"
/// precondition where that matters. Deterministic; a single fold over the slice.
pub fn regression_gate(results: &[RegressionResult]) -> GateOutcome {
    let failing: Vec<RegressionId> = results.iter().filter(|r| !r.passed).map(|r| r.id).collect();
    if failing.is_empty() {
        GateOutcome::Pass
    } else {
        GateOutcome::Blocked { failing }
    }
}
