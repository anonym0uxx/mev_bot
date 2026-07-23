//! §105 (CRITERION 105) LPI extraction-risk / wash-history covariate — a
//! **REPORT / hazard-scaffold plane** readout ONLY.
//!
//! This ledger accumulates a per-mint, time-decayed extraction-risk covariate (a
//! `manip_history_fp`-style bps figure) from the flow-authenticity screen's
//! wash-strength readings. It is deliberately **not** read by any sizing, gating,
//! or promotion decision: it never enters the decision journal, so accumulating
//! into it is byte-identical on the golden path (the digest is unchanged). Its
//! sole consumer is the report/hazard-scaffold plane — the same role the
//! `pump_quant_strategy::scalp_position::manip_history_fp` covariate plays.
//!
//! ## Determinism & bounds (§22 / §99)
//!
//! * Integer-only. Decay reuses the audited half-life leaf
//!   `pump_quant_strategy::safety_integrity::lpi_decayed_covariate` (halving once
//!   per elapsed half-life over the engine's logical-tick clock).
//! * State is bounded to [`EXTRACTION_LEDGER_CAP`] mints; at capacity the
//!   least-recently-updated mint is evicted deterministically.
//! * Each per-mint covariate is clamped to [`MANIP_HISTORY_CLAMP`] (10_000 bps),
//!   matching the `manip_history_fp` contract.

use std::collections::BTreeMap;

use pump_quant_strategy::safety_integrity::lpi_decayed_covariate;

/// Maximum mints tracked before the least-recently-updated is evicted (§99 bounded
/// state) — matches the lane/structure track caps.
const EXTRACTION_LEDGER_CAP: usize = 4_096;

/// The covariate is a `manip_history_fp`-style bps figure, clamped to 10_000 (the
/// same ceiling `scalp_position::manip_history_fp` enforces).
const MANIP_HISTORY_CLAMP: u64 = 10_000;

/// Per-mint accumulated extraction-risk state: the decayed covariate and the
/// logical tick it was last refreshed at (the decay clock reference).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RiskAccum {
    /// Accumulated, last-decayed covariate value (bps, ≤ [`MANIP_HISTORY_CLAMP`]).
    covariate: u64,
    /// Logical tick at which `covariate` was last refreshed.
    last_tick: u64,
}

/// A bounded per-mint decayed extraction-risk covariate ledger (report-plane only).
#[derive(Clone, Debug, Default)]
pub struct ExtractionRiskLedger {
    mints: BTreeMap<[u8; 32], RiskAccum>,
}

/// Decay `covariate` from `last_tick` to `now` (integer half-life decay). A zero
/// covariate stays zero (short-circuit — decay of nothing is nothing).
#[inline]
fn decay_to(covariate: u64, last_tick: u64, now: u64) -> u64 {
    if covariate == 0 {
        return 0;
    }
    let age = now.saturating_sub(last_tick);
    lpi_decayed_covariate(u128::from(covariate), age)
}

impl ExtractionRiskLedger {
    /// A fresh, empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a fresh extraction-risk contribution `base` (bps — e.g. the flow
    /// screen's wash strength `10_000 − authenticity`) for `mint` at logical tick
    /// `now`: the mint's prior covariate is first decayed to `now`, then `base` is
    /// added, then the sum is clamped to [`MANIP_HISTORY_CLAMP`]. Deterministic and
    /// bounded. REPORT-plane only — this value must never feed a live decision.
    pub fn observe(&mut self, mint: [u8; 32], base: u64, now: u64) {
        if !self.mints.contains_key(&mint) && self.mints.len() >= EXTRACTION_LEDGER_CAP {
            // Evict the least-recently-updated mint (deterministic; ties broken by
            // the BTreeMap key order via `min_by_key`'s stable first-min).
            if let Some((&weakest, _)) = self.mints.iter().min_by_key(|(_, a)| a.last_tick) {
                self.mints.remove(&weakest);
            }
        }
        let e = self.mints.entry(mint).or_default();
        let decayed = decay_to(e.covariate, e.last_tick, now);
        e.covariate = decayed.saturating_add(base).min(MANIP_HISTORY_CLAMP);
        e.last_tick = now;
    }

    /// The mint's decayed extraction-risk covariate at logical tick `now`, as a
    /// `manip_history_fp`-style `u32` bps figure (0 for an untracked mint). Pure
    /// read — does not mutate the decay clock.
    #[must_use]
    pub fn manip_history_fp(&self, mint: &[u8; 32], now: u64) -> u32 {
        match self.mints.get(mint) {
            Some(e) => {
                u32::try_from(decay_to(e.covariate, e.last_tick, now).min(MANIP_HISTORY_CLAMP))
                    .unwrap_or(u32::MAX)
            }
            None => 0,
        }
    }

    /// Number of mints currently tracked (bounded by [`EXTRACTION_LEDGER_CAP`]).
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.mints.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_strategy::safety_integrity::LPI_COVARIATE_HALF_LIFE_SECS;

    fn mint(tag: u8) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0] = tag;
        b
    }

    #[test]
    fn untracked_mint_reads_zero() {
        let led = ExtractionRiskLedger::new();
        assert_eq!(led.manip_history_fp(&mint(1), 0), 0);
        assert_eq!(led.tracked(), 0);
    }

    #[test]
    fn accumulation_sums_within_a_half_life() {
        // Two contributions at the same tick accumulate (no decay between them).
        let mut led = ExtractionRiskLedger::new();
        led.observe(mint(1), 3_000, 0);
        led.observe(mint(1), 2_000, 0);
        assert_eq!(led.manip_history_fp(&mint(1), 0), 5_000);
        assert_eq!(led.tracked(), 1);
    }

    #[test]
    fn accumulation_clamps_at_ceiling() {
        let mut led = ExtractionRiskLedger::new();
        led.observe(mint(1), 8_000, 0);
        led.observe(mint(1), 8_000, 0);
        // 16_000 → clamped to 10_000 (manip_history_fp ceiling).
        assert_eq!(led.manip_history_fp(&mint(1), 0), 10_000);
    }

    #[test]
    fn covariate_decays_over_half_lives() {
        let mut led = ExtractionRiskLedger::new();
        led.observe(mint(1), 8_000, 0);
        // One half-life later: halved.
        let hl = LPI_COVARIATE_HALF_LIFE_SECS;
        assert_eq!(led.manip_history_fp(&mint(1), hl), 4_000);
        // Two half-lives later: quartered.
        assert_eq!(led.manip_history_fp(&mint(1), 2 * hl), 2_000);
    }

    #[test]
    fn observe_decays_prior_before_adding() {
        // A fresh contribution after a half-life sits on top of the *decayed* prior.
        let mut led = ExtractionRiskLedger::new();
        led.observe(mint(1), 8_000, 0);
        let hl = LPI_COVARIATE_HALF_LIFE_SECS;
        led.observe(mint(1), 1_000, hl); // prior 8_000 → 4_000, + 1_000 = 5_000
        assert_eq!(led.manip_history_fp(&mint(1), hl), 5_000);
    }

    #[test]
    fn zero_contribution_never_creates_risk() {
        let mut led = ExtractionRiskLedger::new();
        led.observe(mint(1), 0, 0);
        assert_eq!(led.manip_history_fp(&mint(1), 0), 0);
    }

    #[test]
    fn ledger_is_capacity_bounded() {
        let mut led = ExtractionRiskLedger::new();
        for i in 0..(EXTRACTION_LEDGER_CAP + 50) {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&(i as u64).to_le_bytes());
            led.observe(b, 1_000, i as u64);
        }
        assert!(led.tracked() <= EXTRACTION_LEDGER_CAP);
    }
}
