//! `walk_forward` — chronological walk-forward / no-look-ahead split validator
//! (constitution §16, §53).
//!
//! Responsibility: prove, deterministically and on the frozen-evaluator side,
//! that a set of walk-forward folds contains no look-ahead. Every test window
//! must strictly post-date its own training window, every window must be
//! well-formed, and the folds must march forward in time. The Python research
//! loop *orchestrates* the folds; this guard *proves* they cannot leak the
//! future into training. Any Python bug that shuffles or overlaps folds is
//! caught here rather than silently inflating backtest results.
//!
//! Integer-only (constitution §22): timestamps are `u64` nanoseconds; no floats.

/// One walk-forward fold: a training window ending at `train_end_ns`, then a
/// test window `[test_start_ns, test_end_ns]`. Times are `u64` nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    /// Inclusive end of the training window (nanoseconds).
    pub train_end_ns: u64,
    /// Start of the test/evaluation window (nanoseconds).
    pub test_start_ns: u64,
    /// End of the test/evaluation window (nanoseconds).
    pub test_end_ns: u64,
}

impl Fold {
    /// Test/golden-vector constructor.
    pub fn new(train_end_ns: u64, test_start_ns: u64, test_end_ns: u64) -> Self {
        Fold {
            train_end_ns,
            test_start_ns,
            test_end_ns,
        }
    }
}

/// A detected chronology violation, tagged with the offending fold index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leak {
    /// The test window does not strictly post-date training: look-ahead.
    /// Requires `test_start_ns > train_end_ns`.
    TestOverlapsTrain {
        /// 0-based index of the offending fold.
        fold: usize,
    },
    /// The test window is malformed (`test_start_ns > test_end_ns`).
    MalformedWindow {
        /// 0-based index of the offending fold.
        fold: usize,
    },
    /// Folds are not ordered forward in time (a fold's training end or test
    /// start moved backward relative to its predecessor).
    FoldsOutOfOrder {
        /// 0-based index of the fold that regressed.
        fold: usize,
    },
}

/// Assert that every fold is chronological and the folds march forward.
///
/// Responsibility (constitution §16, §53): returns `Ok(())` iff
///
/// * every fold has `test_start_ns > train_end_ns` (test strictly post-dates
///   train — the core no-look-ahead condition), and
/// * every fold has `test_start_ns ≤ test_end_ns` (well-formed window), and
/// * across folds both `train_end_ns` and `test_start_ns` are non-decreasing
///   (the walk marches forward, never revisiting an earlier era).
///
/// The first violation in fold order is returned as the corresponding [`Leak`].
/// An empty or single-fold slice trivially satisfies the ordering clause.
/// Deterministic; pure comparison, no arithmetic that can overflow.
pub fn assert_chronological(folds: &[Fold]) -> Result<(), Leak> {
    let mut prev: Option<Fold> = None;
    for (i, f) in folds.iter().enumerate() {
        // Per-fold well-formedness and no-look-ahead.
        if f.test_start_ns <= f.train_end_ns {
            return Err(Leak::TestOverlapsTrain { fold: i });
        }
        if f.test_start_ns > f.test_end_ns {
            return Err(Leak::MalformedWindow { fold: i });
        }
        // Forward-march across folds.
        if let Some(p) = prev {
            if f.train_end_ns < p.train_end_ns || f.test_start_ns < p.test_start_ns {
                return Err(Leak::FoldsOutOfOrder { fold: i });
            }
        }
        prev = Some(*f);
    }
    Ok(())
}
