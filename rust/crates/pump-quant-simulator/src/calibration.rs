//! Versioned, memory-bounded `CalibrationStore` and deterministic model
//! application over recorded fills.
//!
//! Responsibility: hold the calibrated execution model (fees, impairment, impact,
//! terminal-loss rule, mode) keyed by execution condition, and replay recorded
//! fills through it deterministically (constitution §38: "Calibrate ... stored in a
//! versioned CalibrationStore"; §39 execution-calibration budget). The store is
//! memory-bounded: a hard cap on the number of keys and on retained versions per
//! key, with deterministic FIFO eviction of the oldest version — no unbounded
//! growth in a long paper run.

use crate::fill::{simulate_fill, CostModel, ExitImpairment, FillMode, FillResult, MarketState};
use crate::terminal_loss::TerminalLossPolicy;
use std::collections::BTreeMap;

/// Execution condition a calibration is keyed by. Ordered so store iteration is
/// deterministic (§22: stable iteration ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CalibrationKey {
    /// Submission route identifier (e.g. Jito vs. an alternative).
    pub route_id: u16,
    /// Tip band bucket the trade fell into.
    pub tip_band: u8,
}

/// A single calibrated model version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalibrationParams {
    /// Itemized fee/tip model.
    pub costs: CostModel,
    /// Exit-impairment model applied in Mode C.
    pub imp: ExitImpairment,
    /// Price-impact scale for the venue.
    pub impact_k_bps: u32,
    /// Predeclared terminal-loss rule for unexitable positions.
    pub terminal: TerminalLossPolicy,
    /// Mode this calibration drives when applied.
    pub mode: FillMode,
}

/// A recorded fill input: the raw, replayable market observation, tagged with the
/// execution condition it should be calibrated under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedFill {
    /// Condition key selecting which calibration applies.
    pub key: CalibrationKey,
    /// Recorded SOL notional committed, in lamports.
    pub notional_lamports: u64,
    /// Recorded signed price move over the hold, in bps.
    pub move_bps: i32,
    /// Recorded SOL-side depth, in lamports.
    pub depth_lamports: u64,
}

/// Error returned when an insert would violate a memory bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalStoreError {
    /// Inserting a new key would exceed the configured key capacity.
    KeyCapacityExceeded,
}

/// Memory-bounded, versioned calibration store.
#[derive(Debug, Clone)]
pub struct CalibrationStore {
    max_keys: usize,
    max_versions: usize,
    map: BTreeMap<CalibrationKey, Vec<CalibrationParams>>,
}

impl CalibrationStore {
    /// Create a store bounded to `max_keys` distinct keys and `max_versions`
    /// retained versions per key. Both bounds are clamped to at least `1`.
    #[must_use]
    pub fn new(max_keys: usize, max_versions: usize) -> Self {
        CalibrationStore {
            max_keys: max_keys.max(1),
            max_versions: max_versions.max(1),
            map: BTreeMap::new(),
        }
    }

    /// Append a calibration version for `key`.
    ///
    /// If `key` is new and the key capacity is full, returns
    /// [`CalStoreError::KeyCapacityExceeded`] (explicit failure — never silent
    /// overwrite of an unrelated key). When a key already holds `max_versions`
    /// versions, the oldest (index 0) is evicted FIFO before the append, keeping
    /// memory bounded and eviction deterministic.
    pub fn insert(
        &mut self,
        key: CalibrationKey,
        params: CalibrationParams,
    ) -> Result<(), CalStoreError> {
        if !self.map.contains_key(&key) && self.map.len() >= self.max_keys {
            return Err(CalStoreError::KeyCapacityExceeded);
        }
        let versions = self.map.entry(key).or_default();
        if versions.len() >= self.max_versions {
            versions.remove(0);
        }
        versions.push(params);
        Ok(())
    }

    /// Number of distinct keys currently held.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.map.len()
    }

    /// Number of retained versions for `key` (`0` if absent).
    #[must_use]
    pub fn version_count(&self, key: &CalibrationKey) -> usize {
        self.map.get(key).map_or(0, Vec::len)
    }

    /// The latest (most recently inserted, post-eviction) calibration for `key`.
    #[must_use]
    pub fn latest(&self, key: &CalibrationKey) -> Option<&CalibrationParams> {
        self.map.get(key).and_then(|v| v.last())
    }

    /// Apply the latest calibration for a single recorded fill.
    ///
    /// Returns `None` when no calibration exists for the fill's key — a missing
    /// calibration is surfaced, never silently defaulted. Deterministic: identical
    /// `recorded` + store state always yields the identical [`FillResult`].
    #[must_use]
    pub fn apply_recorded(&self, recorded: &RecordedFill) -> Option<FillResult> {
        let params = self.latest(&recorded.key)?;
        let market = MarketState {
            notional_lamports: recorded.notional_lamports,
            move_bps: recorded.move_bps,
            depth_lamports: recorded.depth_lamports,
            impact_k_bps: params.impact_k_bps,
        };
        Some(simulate_fill(
            &market,
            &params.costs,
            &params.imp,
            params.mode,
            &params.terminal,
        ))
    }

    /// Deterministic model application over a batch of recorded fills, preserving
    /// input order. Each element is `Some` iff its key has a calibration.
    #[must_use]
    pub fn apply_all(&self, fills: &[RecordedFill]) -> Vec<Option<FillResult>> {
        fills.iter().map(|f| self.apply_recorded(f)).collect()
    }
}
