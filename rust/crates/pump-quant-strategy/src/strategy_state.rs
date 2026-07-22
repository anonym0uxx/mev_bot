//! # strategy_state — orthogonal multi-dimensional decision state (criterion 47)
//!
//! [`StrategyState`] holds four **orthogonal** decision dimensions — entry, size,
//! exit, hold — each an independently inspectable [`Dimension`] preserving its raw
//! input, derived value, completeness, freshness, confidence, and source
//! provenance (constitution §31). "No single collapsed score drives entry + size +
//! exit + hold" is enforced *by construction*: the only constructor takes four
//! separate dimensions, there is no `from_composite`, and mutating one dimension
//! provably leaves the other three unchanged.
//!
//! ## Constitution
//! §31: orthogonal, independently observable dimensions; composite scores never
//! erase the underlying dimensions and a single composite may never override a
//! hard per-dimension failure. §22: all values integer/fixed-point.

/// One orthogonal decision dimension with its full provenance trail.
///
/// Every dimension preserves raw and derived inputs plus quality metadata, so the
/// substrate is never collapsed into a single opaque number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dimension {
    /// Raw input value, fixed-point.
    pub raw_fp: i64,
    /// Derived (post-processing) value, fixed-point.
    pub derived_fp: i64,
    /// Completeness in bps (0..=10_000).
    pub completeness_bps: u32,
    /// Freshness / age in ns (lower is fresher).
    pub freshness_ns: u64,
    /// Confidence in bps (0..=10_000).
    pub confidence_bps: u32,
    /// Source-provenance id.
    pub source: u32,
}

impl Dimension {
    /// Construct a dimension from its raw inputs.
    pub fn new(
        raw_fp: i64,
        derived_fp: i64,
        completeness_bps: u32,
        freshness_ns: u64,
        confidence_bps: u32,
        source: u32,
    ) -> Self {
        Dimension {
            raw_fp,
            derived_fp,
            completeness_bps,
            freshness_ns,
            confidence_bps,
            source,
        }
    }
}

/// The four orthogonal decision dimensions, named for inspection/dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimensionKind {
    /// Entry attractiveness dimension.
    Entry,
    /// Position-size dimension.
    Size,
    /// Exit-pressure dimension.
    Exit,
    /// Hold-continuation dimension.
    Hold,
}

/// The multi-dimensional strategy decision state (criterion 47).
///
/// The four dimensions are stored separately and are never derived from one
/// another. There is intentionally no constructor that builds all four from a
/// single score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrategyState {
    entry: Dimension,
    size: Dimension,
    exit: Dimension,
    hold: Dimension,
}

impl StrategyState {
    /// Build the state from four independently-computed dimensions.
    ///
    /// Taking four separate arguments is the by-construction guarantee that no
    /// single collapsed score can drive all four decisions.
    pub fn new(entry: Dimension, size: Dimension, exit: Dimension, hold: Dimension) -> Self {
        StrategyState {
            entry,
            size,
            exit,
            hold,
        }
    }

    /// Inspect the entry dimension.
    #[inline]
    pub fn entry(&self) -> &Dimension {
        &self.entry
    }
    /// Inspect the size dimension.
    #[inline]
    pub fn size(&self) -> &Dimension {
        &self.size
    }
    /// Inspect the exit dimension.
    #[inline]
    pub fn exit(&self) -> &Dimension {
        &self.exit
    }
    /// Inspect the hold dimension.
    #[inline]
    pub fn hold(&self) -> &Dimension {
        &self.hold
    }

    /// Inspect a dimension by kind (uniform accessor for auditing).
    pub fn get(&self, kind: DimensionKind) -> &Dimension {
        match kind {
            DimensionKind::Entry => &self.entry,
            DimensionKind::Size => &self.size,
            DimensionKind::Exit => &self.exit,
            DimensionKind::Hold => &self.hold,
        }
    }

    /// Replace one dimension, returning the updated state and leaving the other
    /// three byte-identical. Used to prove orthogonality in tests.
    pub fn with(&self, kind: DimensionKind, dim: Dimension) -> Self {
        let mut s = *self;
        match kind {
            DimensionKind::Entry => s.entry = dim,
            DimensionKind::Size => s.size = dim,
            DimensionKind::Exit => s.exit = dim,
            DimensionKind::Hold => s.hold = dim,
        }
        s
    }

    /// The four dimensions in canonical order for inspection.
    pub fn dimensions(&self) -> [(DimensionKind, Dimension); 4] {
        [
            (DimensionKind::Entry, self.entry),
            (DimensionKind::Size, self.size),
            (DimensionKind::Exit, self.exit),
            (DimensionKind::Hold, self.hold),
        ]
    }
}
