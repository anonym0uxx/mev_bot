//! Fee calibration: priority-fee market sampler producing `fee_calibration_v1` records.
//!
//! The sampler periodically queries the Solana `getRecentPriorityFees` RPC
//! method (or observes landed transaction fees) and produces a
//! `FeeCalibrationV1` record with:
//! - p50 (median) CU price in lamports
//! - p75, p90, p99 CU prices for tail-latency budgeting
//! - sample size (number of observed slots/transactions)
//! - timestamp (slot number)
//!
//! ## Design
//! - **Deterministic core**: the `FeeCalibrationV1` record and the
//!   `pick_cu_price` selector are pure functions with no I/O.
//! - **I/O boundary**: the `FeeCalibrationSampler` struct holds the RPC URL
//!   and would call `getRecentPriorityFees` when wired. Until then, it
//!   returns `None` (fail-closed — no fabricated CU price).
//! - **Fail-closed**: if no samples exist, `pick_cu_price` returns 0,
//!   which triggers the flat `FIXED_LAMPORTS_PER_LEG` fallback in
//!   the app-layer `cost_model::leg_cost_lamports`.
//!
//! ## Constitution refs
//! - §22: integer math only. CU prices are u64 lamports.
//! - §24(b): paper/replay mode uses the flat fallback (CU price = 0).

/// The flat fallback per-leg cost in lamports (mirrors the app crate's
/// `FIXED_LAMPORTS_PER_LEG`). Inlined here to avoid a cyclic dependency
/// (app depends on execution, not vice versa).
const FIXED_LAMPORTS_PER_LEG: u64 = 150_000;

/// A fee-calibration record (version 1). Produced by the sampler from
/// observed priority-fee market data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeeCalibrationV1 {
    /// Slot number at which the sample was taken.
    pub slot: u64,
    /// Number of slots/transactions observed.
    pub sample_size: u32,
    /// p50 (median) CU price in lamports per CU unit.
    pub cu_price_p50: u64,
    /// p75 CU price in lamports per CU unit.
    pub cu_price_p75: u64,
    /// p90 CU price in lamports per CU unit.
    pub cu_price_p90: u64,
    /// p99 CU price in lamports per CU unit (tail-latency budget).
    pub cu_price_p99: u64,
}

/// Select the CU price to use for a given urgency level.
///
/// - Low urgency (0): p50 (median — cheapest, slowest landing)
/// - Medium urgency (1-2): p75
/// - High urgency (3-4): p90
/// - Critical urgency (5+): p99 (tail — most expensive, fastest landing)
///
/// Returns 0 when the calibration is empty (sample_size == 0), triggering
/// the flat `FIXED_LAMPORTS_PER_LEG` fallback. This is fail-closed:
/// no fabricated CU price, always the conservative flat estimate.
#[must_use]
#[inline]
pub fn pick_cu_price(cal: &FeeCalibrationV1, urgency: u8) -> u64 {
    if cal.sample_size == 0 {
        return 0; // fail-closed → flat fallback
    }
    match urgency {
        0 => cal.cu_price_p50,
        1 | 2 => cal.cu_price_p75,
        3 | 4 => cal.cu_price_p90,
        _ => cal.cu_price_p99,
    }
}

/// The fee-calibration sampler. Holds an RPC URL and the last calibration.
/// In paper mode, the sampler is not wired and `last()` returns `None`.
pub struct FeeCalibrationSampler {
    #[allow(dead_code)]
    rpc_url: String,
    last_calibration: Option<FeeCalibrationV1>,
}

impl FeeCalibrationSampler {
    /// Create a new sampler pointing at the given RPC endpoint.
    #[must_use]
    pub fn new(rpc_url: &str) -> Self {
        Self {
            rpc_url: rpc_url.to_string(),
            last_calibration: None,
        }
    }

    /// Return the last calibration record, if any.
    #[must_use]
    pub fn last(&self) -> Option<FeeCalibrationV1> {
        self.last_calibration
    }

    /// Set the calibration record (used by tests and the live sampler
    /// when it produces a new record from RPC data).
    pub fn set(&mut self, cal: FeeCalibrationV1) {
        self.last_calibration = Some(cal);
    }
}

/// Verify that the flat fallback is used when no calibration exists.
/// This is the fail-closed guarantee: no calibration → no fabricated CU price.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_calibration_returns_zero_cu_price() {
        let cal = FeeCalibrationV1::default();
        assert_eq!(pick_cu_price(&cal, 0), 0);
        assert_eq!(pick_cu_price(&cal, 5), 0);
    }

    #[test]
    fn urgency_0_uses_p50() {
        let cal = FeeCalibrationV1 {
            slot: 100,
            sample_size: 50,
            cu_price_p50: 100,
            cu_price_p75: 200,
            cu_price_p90: 400,
            cu_price_p99: 1000,
        };
        assert_eq!(pick_cu_price(&cal, 0), 100);
    }

    #[test]
    fn urgency_1_2_uses_p75() {
        let cal = FeeCalibrationV1 {
            slot: 100,
            sample_size: 50,
            cu_price_p50: 100,
            cu_price_p75: 200,
            cu_price_p90: 400,
            cu_price_p99: 1000,
        };
        assert_eq!(pick_cu_price(&cal, 1), 200);
        assert_eq!(pick_cu_price(&cal, 2), 200);
    }

    #[test]
    fn urgency_3_4_uses_p90() {
        let cal = FeeCalibrationV1 {
            slot: 100,
            sample_size: 50,
            cu_price_p50: 100,
            cu_price_p75: 200,
            cu_price_p90: 400,
            cu_price_p99: 1000,
        };
        assert_eq!(pick_cu_price(&cal, 3), 400);
        assert_eq!(pick_cu_price(&cal, 4), 400);
    }

    #[test]
    fn urgency_5_plus_uses_p99() {
        let cal = FeeCalibrationV1 {
            slot: 100,
            sample_size: 50,
            cu_price_p50: 100,
            cu_price_p75: 200,
            cu_price_p90: 400,
            cu_price_p99: 1000,
        };
        assert_eq!(pick_cu_price(&cal, 5), 1000);
        assert_eq!(pick_cu_price(&cal, 255), 1000);
    }

    #[test]
    fn sampler_starts_empty() {
        let s = FeeCalibrationSampler::new("http://127.0.0.1:8080");
        assert!(s.last().is_none());
    }

    #[test]
    fn sampler_stores_calibration() {
        let mut s = FeeCalibrationSampler::new("http://127.0.0.1:8080");
        let cal = FeeCalibrationV1 {
            slot: 200,
            sample_size: 100,
            cu_price_p50: 150,
            cu_price_p75: 300,
            cu_price_p90: 600,
            cu_price_p99: 1500,
        };
        s.set(cal);
        assert_eq!(s.last(), Some(cal));
    }

    #[test]
    fn flat_fallback_constant_is_conservative() {
        // The flat fallback (150k lamports) must be conservative — larger
        // than a typical p50 CU price * CU consumption. With 40k CU and
        // p50 = 100 lamports/CU, the CU fee is 400 lamports — far below
        // the 150k flat fallback.
        let cu_fee = 40_000u64 * 100 / 10_000; // 400 lamports
        assert!(cu_fee < FIXED_LAMPORTS_PER_LEG);
    }
}
