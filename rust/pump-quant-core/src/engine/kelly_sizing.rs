//! Unified Kelly Position Sizing Engine
//!
//! Computes position size from wallet balance + entry features using a 2D LUT
//! with bilinear interpolation for win probability (p) and reward ratio (R),
//! then applies half-Kelly, correlation adjustment, and drawdown scaling.
//!
//! Called once per entry decision (~100-500/day). NOT on the hot path.
//! Output struct is integer-only for zero-cost reads by the hot-path exit engine.

use std::sync::atomic::{AtomicU64, Ordering};

// ─── Constants ──────────────────────────────────────────────────────────────

/// Minimum position size: 0.05 SOL
const MIN_SIZE_LAMPORTS: u64 = 50_000_000;
/// Maximum position size: 0.20 SOL
const MAX_SIZE_LAMPORTS: u64 = 200_000_000;

/// Default round-trip fee in basis points (1% buy + 1% sell + ~0.11% Jito).
/// Used to adjust Kelly R before sizing. Override via config.
pub const DEFAULT_ROUND_TRIP_FEE_BP: u16 = 210;

/// Default average loss in basis points (used to convert R ratio to absolute bp).
/// R_raw = avg_win_bp / avg_loss_bp, so avg_win_bp = R × avg_loss_bp.
pub const DEFAULT_AVG_LOSS_BP: u16 = 200;

/// 4×4 win-probability LUT (permille, i.e. p × 1000).
/// Rows: magnitude buckets [40-50, 50-60, 60-70, 70+]
/// Cols: entry_score buckets [50-60, 60-70, 70-80, 80+]
/// Precomputed from 392 historical trades.
const P_LUT: [[u16; 4]; 4] = [
    [440, 420, 450, 430], // mag 40-50
    [600, 560, 580, 550], // mag 50-60
    [640, 590, 620, 580], // mag 60-70
    [540, 510, 540, 520], // mag 70+
];

/// 4×4 reward-ratio LUT (R × 100).
/// Same bucket layout as P_LUT.
const R_LUT: [[u16; 4]; 4] = [
    [4300, 4800, 4000, 3500], // mag 40-50 (high R, low p — lottery tickets)
    [1100, 1400, 1200, 900],  // mag 50-60
    [800, 1200, 900, 700],    // mag 60-70
    [700, 1000, 750, 520],    // mag 70+
];

/// Magnitude bucket boundaries (lower bounds) × 100 for integer math.
/// Bucket i covers [MAG_BOUNDS_X100[i], MAG_BOUNDS_X100[i+1]).
/// Below MAG_BOUNDS_X100[0] → clamp to bucket 0. At/above last bound → bucket 3.
const MAG_BOUNDS_X100: [u32; 4] = [4000, 5000, 6000, 7000];
/// Score bucket boundaries × 100.
const SCORE_BOUNDS_X100: [u32; 4] = [5000, 6000, 7000, 8000];
/// Bucket width × 100 (uniform for both dimensions).
const BUCKET_WIDTH_X100: u32 = 1000;

/// Fixed-point fractional bits for bilinear interpolation.
/// 1024 = 1.0. Gives ~0.1% precision on u16 LUT values.
const FRAC_BITS: u32 = 10;
const FRAC_ONE: u32 = 1 << FRAC_BITS; // 1024

// ─── Entry Conviction Struct ────────────────────────────────────────────────

/// Entry conviction computed at decision time, stored on OpenPosition.
/// Integer-only for zero-cost reads on the hot-path exit engine.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct EntryConviction {
    /// Win probability × 1000 (0–1000)
    pub p_permille: u16,
    /// Win/loss ratio × 100
    pub r_x100: u16,
    /// Optimal Kelly fraction × 1000 (after half-Kelly + adjustments)
    pub f_permille: u16,
    /// Kelly-derived position size in lamports
    pub size_lamports: u64,
    /// Conviction tier: 0=LOW, 1=MED, 2=HIGH
    pub conviction_tier: u8,
    /// Padding for repr(C) alignment
    pub _pad: [u8; 5],
}

impl Default for EntryConviction {
    fn default() -> Self {
        Self {
            p_permille: 542,
            r_x100: 4300,
            f_permille: 248,
            size_lamports: 50_000_000,
            conviction_tier: 1,
            _pad: [0; 5],
        }
    }
}

// ─── Bucket Assignment ──────────────────────────────────────────────────────

/// Map a continuous score to (bucket_index, fractional_position).
/// Input: `value_x100` = score × 100 (e.g., 55.0 → 5500).
/// `bounds_x100`: bucket lower bounds × 100.
/// Returns (bucket 0..3, frac as fixed-point 0..FRAC_ONE where FRAC_ONE = 1.0).
/// Pure integer arithmetic — zero f64.
#[inline]
fn bucket_frac_int(value_x100: u32, bounds_x100: &[u32; 4]) -> (usize, u32) {
    if value_x100 <= bounds_x100[0] {
        return (0, 0);
    }
    // Distance above first bound, in x100 units
    let offset = value_x100 - bounds_x100[0];
    // Bucket index = offset / width (integer division)
    let bucket = (offset / BUCKET_WIDTH_X100) as usize;
    let bucket = bucket.min(3);
    // Fractional position within bucket: (offset % width) * FRAC_ONE / width
    let frac = if bucket >= 3 {
        let past_last = value_x100.saturating_sub(bounds_x100[3]);
        // Clamp frac to [0, FRAC_ONE]
        (past_last * FRAC_ONE / BUCKET_WIDTH_X100).min(FRAC_ONE)
    } else {
        let remainder = offset - (bucket as u32 * BUCKET_WIDTH_X100);
        remainder * FRAC_ONE / BUCKET_WIDTH_X100
    };
    (bucket, frac)
}

// ─── Bilinear Interpolation ─────────────────────────────────────────────────

/// Integer bilinear interpolation on a 4×4 u16 LUT.
/// `bm`, `bs`: bucket indices (0..3).
/// `fm`, `fs`: fractional positions as fixed-point (0..FRAC_ONE).
/// Returns interpolated value in the same units as the LUT.
/// Pure integer arithmetic — zero f64. Uses u32 intermediates (no overflow:
/// max value = 4300 × 1024 × 1024 = ~4.5B, fits in u32).
#[inline]
fn bilerp_int(lut: &[[u16; 4]; 4], bm: usize, bs: usize, fm: u32, fs: u32) -> u16 {
    let bm1 = (bm + 1).min(3);
    let bs1 = (bs + 1).min(3);

    let v00 = lut[bm][bs] as u32;
    let v10 = lut[bm1][bs] as u32;
    let v01 = lut[bm][bs1] as u32;
    let v11 = lut[bm1][bs1] as u32;

    let ifm = FRAC_ONE - fm;
    let ifs = FRAC_ONE - fs;

    // result = (ifm*ifs*v00 + fm*ifs*v10 + ifm*fs*v01 + fm*fs*v11) / FRAC_ONE^2
    // Each term fits in u32: max = 1024 * 1024 * 4300 = 4,505,600,000 < u32::MAX
    // Sum of 4 terms can overflow u32 (max ~18B), so use u64 for accumulator.
    let result = (ifm as u64 * ifs as u64 * v00 as u64
        + fm as u64 * ifs as u64 * v10 as u64
        + ifm as u64 * fs as u64 * v01 as u64
        + fm as u64 * fs as u64 * v11 as u64
        + (FRAC_ONE as u64 * FRAC_ONE as u64 / 2)) // rounding
        / (FRAC_ONE as u64 * FRAC_ONE as u64);

    result as u16
}

// ─── Fee-Adjusted Reward Ratio ──────────────────────────────────────────────

/// Adjust R for round-trip fees.
///
/// Raw R = avg_win_bp / avg_loss_bp (stored as r_x100 = R × 100).
/// Fee-adjusted:
///   R_adj = (avg_win_bp - fee_bp) / (avg_loss_bp + fee_bp)
///         = (r_x100 × avg_loss_bp / 100 - fee_bp) / (avg_loss_bp + fee_bp)
///
/// Returns r_adj_x100 (R_adj × 100). Returns 0 if edge is negative.
///
/// All integer arithmetic, no f64.
#[inline]
pub fn fee_adjust_r(r_x100: u16, fee_bp: u16, avg_loss_bp: u16) -> u16 {
    let avg_loss = avg_loss_bp.max(1) as u32;
    let fee = fee_bp as u32;

    // avg_win_bp = r_x100 * avg_loss / 100
    let avg_win_bp = (r_x100 as u32 * avg_loss + 50) / 100;

    // Net win must exceed fee
    if avg_win_bp <= fee {
        return 0;
    }

    let net_win = avg_win_bp - fee;         // avg_win_bp - fee_bp
    let net_loss = avg_loss + fee;           // avg_loss_bp + fee_bp

    // R_adj × 100 = net_win × 100 / net_loss
    let r_adj = (net_win * 100 + net_loss / 2) / net_loss; // rounded
    r_adj as u16
}

// ─── Kelly Fraction ─────────────────────────────────────────────────────────

/// Compute raw Kelly fraction in permille.
/// f* = p - (1-p)/R
/// Integer form: f_permille = p_permille - (1000 - p_permille) * 100 / r_x100
/// Returns 0 if there is no edge (f* ≤ 0).
#[inline]
fn kelly_permille(p_permille: u16, r_x100: u16) -> u16 {
    if r_x100 == 0 {
        return 0;
    }
    let loss_permille = 1000u32.saturating_sub(p_permille as u32);
    let penalty = loss_permille * 100 / r_x100 as u32;
    let p = p_permille as u32;
    if p <= penalty {
        return 0;
    }
    (p - penalty).min(1000) as u16
}

// ─── Conviction Tier ────────────────────────────────────────────────────────

/// Map adjusted f_permille to conviction tier.
/// 0=LOW, 1=MED, 2=HIGH
#[inline]
fn conviction_tier(f_permille: u16) -> u8 {
    if f_permille >= 550 {
        2 // HIGH
    } else if f_permille >= 450 {
        1 // MED
    } else {
        0 // LOW
    }
}

// ─── Core Entry Point ───────────────────────────────────────────────────────

/// Compute entry conviction from features and wallet state.
///
/// # Arguments
/// * `mag_score`  – magnitude score (f64, typically 0–100)
/// * `entry_score` – entry quality score (f64, typically 0–100)
/// * `wallet_balance_lamports` – effective bankroll in lamports
/// * `n_open` – number of currently open positions (0–255)
/// * `drawdown_pct` – current drawdown from HWM as integer percent (0–100)
///
/// # Sizing pipeline
/// 1. 2D LUT lookup with bilinear interpolation → p, R
/// 2. Raw Kelly: f* = p - (1-p)/R
/// 3. Half-Kelly: f_half = f* / 2
/// 4. Correlation adjustment (Thorp approx, ρ≈0.25):
///      f_adj = f_half × 256 / (256 + (n_open - 1) × 64)
/// 5. Drawdown scaling (if drawdown_pct > 10):
///      f_adj *= (100 - drawdown_pct) / 100
/// 6. size = f_adj × wallet_balance / 1_000
/// 7. Clamp to [MIN_SIZE_LAMPORTS, MAX_SIZE_LAMPORTS]
pub fn compute_conviction(
    mag_score: f64,
    entry_score: f64,
    wallet_balance_lamports: u64,
    n_open: u8,
    drawdown_pct: u8,
) -> EntryConviction {
    compute_conviction_with_fees(
        mag_score, entry_score, wallet_balance_lamports,
        n_open, drawdown_pct,
        DEFAULT_ROUND_TRIP_FEE_BP, DEFAULT_AVG_LOSS_BP,
    )
}

/// Fee-aware conviction computation. Adjusts R for round-trip trading friction
/// before computing Kelly fraction, so entries with insufficient edge after
/// fees are correctly rejected.
pub fn compute_conviction_with_fees(
    mag_score: f64,
    entry_score: f64,
    wallet_balance_lamports: u64,
    n_open: u8,
    drawdown_pct: u8,
    fee_bp: u16,
    avg_loss_bp: u16,
) -> EntryConviction {
    // Step 1: Convert f64 scores to integer ×100 (single f64→u32 conversion, then pure integer)
    let mag_x100 = (mag_score * 100.0).max(0.0).min(10000.0) as u32;
    let score_x100 = (entry_score * 100.0).max(0.0).min(10000.0) as u32;

    // Step 1b: Bucket assignment with integer interpolation fractions
    let (bm, fm) = bucket_frac_int(mag_x100, &MAG_BOUNDS_X100);
    let (bs, fs) = bucket_frac_int(score_x100, &SCORE_BOUNDS_X100);

    // Step 2: Integer bilinear interpolation on both LUTs
    let p_permille = bilerp_int(&P_LUT, bm, bs, fm, fs);
    let r_x100_raw = bilerp_int(&R_LUT, bm, bs, fm, fs);

    // Step 2b: Fee-adjust R before Kelly computation
    let r_x100 = if fee_bp > 0 {
        fee_adjust_r(r_x100_raw, fee_bp, avg_loss_bp)
    } else {
        r_x100_raw
    };

    // Step 3: Raw Kelly fraction (now fee-aware)
    let f_raw = kelly_permille(p_permille, r_x100);

    // Step 4: Half-Kelly
    let f_half = f_raw / 2;

    // Step 5: Correlation adjustment — Thorp approximation with ρ=0.25
    // f_adj = f_half * 256 / (256 + max(0, n_open - 1) * 64)
    let k = n_open.saturating_sub(1) as u32;
    let denom = 256u32 + k * 64;
    let f_adj_corr = (f_half as u32 * 256 + denom / 2) / denom; // rounded

    // Step 6: Drawdown scaling
    let f_adj = if drawdown_pct > 10 {
        let scale = (100u32).saturating_sub(drawdown_pct as u32);
        (f_adj_corr * scale + 50) / 100 // rounded
    } else {
        f_adj_corr
    };

    let f_final = (f_adj as u16).min(1000);

    // Step 7: Position sizing
    // size = f_final * wallet_balance / 1000
    let size_raw = if wallet_balance_lamports > 0 && f_final > 0 {
        (wallet_balance_lamports as u128 * f_final as u128 / 1000) as u64
    } else {
        0
    };

    // Step 8: Clamp
    let size_lamports = if size_raw == 0 {
        0
    } else {
        size_raw.clamp(MIN_SIZE_LAMPORTS, MAX_SIZE_LAMPORTS)
    };

    let tier = conviction_tier(f_final);

    EntryConviction {
        p_permille,
        r_x100,
        f_permille: f_final,
        size_lamports,
        conviction_tier: tier,
        _pad: [0u8; 5],
    }
}

// ─── Paper Bankroll ─────────────────────────────────────────────────────────

/// Atomic paper-trading bankroll tracker.
/// Thread-safe for concurrent reads; `apply_pnl` uses CAS loop.
pub struct PaperBankroll {
    balance: AtomicU64,
    initial: u64,
}

impl PaperBankroll {
    /// Create a new paper bankroll with the given initial balance (lamports).
    pub fn new(initial_lamports: u64) -> Self {
        Self {
            balance: AtomicU64::new(initial_lamports),
            initial: initial_lamports,
        }
    }

    /// Current balance in lamports.
    #[inline]
    pub fn balance(&self) -> u64 {
        self.balance.load(Ordering::Relaxed)
    }

    /// Initial balance (for drawdown calculation).
    #[inline]
    pub fn initial(&self) -> u64 {
        self.initial
    }

    /// Apply a PnL delta. `net_pnl_lamports` can be negative (loss).
    /// Uses CAS loop for lock-free thread safety.
    /// Balance saturates at 0 on the low end.
    pub fn apply_pnl(&self, net_pnl_lamports: i64) {
        loop {
            let current = self.balance.load(Ordering::Relaxed);
            let new_val = if net_pnl_lamports >= 0 {
                current.saturating_add(net_pnl_lamports as u64)
            } else {
                current.saturating_sub((-net_pnl_lamports) as u64)
            };
            if self
                .balance
                .compare_exchange_weak(current, new_val, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    /// Current drawdown as integer percent (0–100) relative to initial balance.
    /// Returns 0 if current balance >= initial.
    pub fn drawdown_pct(&self) -> u8 {
        let bal = self.balance();
        if bal >= self.initial {
            return 0;
        }
        let dd = self.initial - bal;
        let pct = (dd as u128 * 100 / self.initial as u128) as u64;
        pct.min(100) as u8
    }
}

// ─── Bankroll Source Abstraction ────────────────────────────────────────────

/// Abstraction over paper vs. live bankroll sources.
pub enum BankrollSource {
    /// Paper trading: fully simulated balance.
    Paper(PaperBankroll),
    /// Live trading: cached balance from RPC, updated externally.
    Live { cached_balance: AtomicU64 },
}

impl BankrollSource {
    /// Current balance in lamports.
    pub fn balance(&self) -> u64 {
        match self {
            BankrollSource::Paper(pb) => pb.balance(),
            BankrollSource::Live { cached_balance } => cached_balance.load(Ordering::Relaxed),
        }
    }

    /// Apply PnL to paper bankroll. No-op for live.
    pub fn apply_paper_pnl(&self, pnl: i64) {
        if let BankrollSource::Paper(pb) = self {
            pb.apply_pnl(pnl);
        }
    }

    /// Drawdown percent (0–100). For live, returns 0 (tracked externally).
    pub fn drawdown_pct(&self) -> u8 {
        match self {
            BankrollSource::Paper(pb) => pb.drawdown_pct(),
            BankrollSource::Live { .. } => 0,
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── LUT bucket sanity ───────────────────────────────────────────────

    #[test]
    fn test_lut_buckets_produce_sensible_values() {
        let mag_centers = [45.0, 55.0, 65.0, 75.0];
        let score_centers = [55.0, 65.0, 75.0, 85.0];

        for (mi, &mag) in mag_centers.iter().enumerate() {
            for (si, &score) in score_centers.iter().enumerate() {
                let c = compute_conviction(mag, score, 1_000_000_000, 0, 0);

                let expected_p = P_LUT[mi][si];
                let expected_r_raw = R_LUT[mi][si];
                // Test against fee-adjusted R (compute_conviction now applies fee_adjust_r)
                let expected_r_adj = fee_adjust_r(expected_r_raw, DEFAULT_ROUND_TRIP_FEE_BP, DEFAULT_AVG_LOSS_BP);

                assert!(
                    (c.p_permille as i32 - expected_p as i32).unsigned_abs() <= 150,
                    "p mismatch at mag={mag}, score={score}: got {} expected ~{expected_p}",
                    c.p_permille
                );
                assert!(
                    (c.r_x100 as i32 - expected_r_adj as i32).unsigned_abs() <= 1000,
                    "R mismatch at mag={mag}, score={score}: got {} expected ~{expected_r_adj} (raw {expected_r_raw})",
                    c.r_x100
                );
                assert!(c.f_permille > 0, "f should be positive at mag={mag}, score={score}");
                assert!(c.conviction_tier <= 2);
            }
        }
    }

    #[test]
    fn test_kelly_permille_known_values() {
        // mag 60-70: p=640, R_x100=800 → f = 640 - 360*100/800 = 640-45 = 595
        assert_eq!(kelly_permille(640, 800), 595);

        // mag 40-50: p=440, R_x100=4300 → f = 440 - 560*100/4300 = 440-13 = 427
        assert_eq!(kelly_permille(440, 4300), 427);

        // No edge
        assert_eq!(kelly_permille(200, 100), 0);

        // Zero R
        assert_eq!(kelly_permille(500, 0), 0);
    }

    // ── Size scales with wallet balance ─────────────────────────────────

    #[test]
    fn test_size_scales_with_wallet_balance() {
        let c1 = compute_conviction(55.0, 55.0, 1_000_000_000, 0, 0);
        let c2 = compute_conviction(55.0, 55.0, 2_000_000_000, 0, 0);
        let c3 = compute_conviction(55.0, 55.0, 4_000_000_000, 0, 0);

        assert_eq!(c1.f_permille, c2.f_permille);
        assert_eq!(c2.f_permille, c3.f_permille);

        assert!(c1.size_lamports <= c2.size_lamports);
        assert!(c2.size_lamports <= c3.size_lamports);
    }

    #[test]
    fn test_size_scales_with_smaller_wallets() {
        let c_small = compute_conviction(55.0, 55.0, 200_000_000, 0, 0);
        let c_medium = compute_conviction(55.0, 55.0, 500_000_000, 0, 0);

        assert!(
            c_small.size_lamports < c_medium.size_lamports,
            "larger wallet should produce larger size: {} vs {}",
            c_small.size_lamports,
            c_medium.size_lamports
        );
    }

    // ── Size decreases with more concurrent positions ───────────────────

    #[test]
    fn test_size_decreases_with_more_open_positions() {
        let wallet = 2_000_000_000u64;
        let c0 = compute_conviction(55.0, 55.0, wallet, 0, 0);
        let c1 = compute_conviction(55.0, 55.0, wallet, 1, 0);
        let c2 = compute_conviction(55.0, 55.0, wallet, 2, 0);
        let c3 = compute_conviction(55.0, 55.0, wallet, 3, 0);
        let c5 = compute_conviction(55.0, 55.0, wallet, 5, 0);

        // n_open=0,1 → same (k=0 for both)
        assert_eq!(c0.f_permille, c1.f_permille);

        assert!(c1.f_permille >= c2.f_permille, "2 open: {} vs {}", c1.f_permille, c2.f_permille);
        assert!(c2.f_permille >= c3.f_permille, "3 open: {} vs {}", c2.f_permille, c3.f_permille);
        assert!(c3.f_permille >= c5.f_permille, "5 open: {} vs {}", c3.f_permille, c5.f_permille);
        assert!(c1.size_lamports >= c5.size_lamports);
    }

    // ── Drawdown scaling reduces size ───────────────────────────────────

    #[test]
    fn test_drawdown_scaling_reduces_size() {
        let wallet = 2_000_000_000u64;
        let c_no_dd = compute_conviction(55.0, 55.0, wallet, 0, 0);
        let c_5_dd = compute_conviction(55.0, 55.0, wallet, 0, 5);
        let c_15_dd = compute_conviction(55.0, 55.0, wallet, 0, 15);
        let c_30_dd = compute_conviction(55.0, 55.0, wallet, 0, 30);
        let c_50_dd = compute_conviction(55.0, 55.0, wallet, 0, 50);

        // ≤10% → no scaling
        assert_eq!(c_no_dd.f_permille, c_5_dd.f_permille);
        assert_eq!(c_no_dd.size_lamports, c_5_dd.size_lamports);

        assert!(c_no_dd.f_permille > c_15_dd.f_permille, "15% dd: {} vs {}", c_no_dd.f_permille, c_15_dd.f_permille);
        assert!(c_15_dd.f_permille > c_30_dd.f_permille, "30% dd: {} vs {}", c_15_dd.f_permille, c_30_dd.f_permille);
        assert!(c_30_dd.f_permille > c_50_dd.f_permille, "50% dd: {} vs {}", c_30_dd.f_permille, c_50_dd.f_permille);
    }

    #[test]
    fn test_drawdown_at_100_pct_produces_zero() {
        let c = compute_conviction(55.0, 55.0, 2_000_000_000, 0, 100);
        assert_eq!(c.f_permille, 0);
        assert_eq!(c.size_lamports, 0);
    }

    // ── Paper bankroll tracking ─────────────────────────────────────────

    #[test]
    fn test_paper_bankroll_basic() {
        let pb = PaperBankroll::new(1_000_000_000);
        assert_eq!(pb.balance(), 1_000_000_000);
        assert_eq!(pb.drawdown_pct(), 0);

        pb.apply_pnl(100_000_000);
        assert_eq!(pb.balance(), 1_100_000_000);
        assert_eq!(pb.drawdown_pct(), 0);

        pb.apply_pnl(-300_000_000);
        assert_eq!(pb.balance(), 800_000_000);
        assert_eq!(pb.drawdown_pct(), 20);
    }

    #[test]
    fn test_paper_bankroll_saturates_at_zero() {
        let pb = PaperBankroll::new(100_000_000);
        pb.apply_pnl(-200_000_000);
        assert_eq!(pb.balance(), 0);
        assert_eq!(pb.drawdown_pct(), 100);
    }

    #[test]
    fn test_paper_bankroll_drawdown_partial() {
        let pb = PaperBankroll::new(2_000_000_000);
        pb.apply_pnl(-500_000_000);
        assert_eq!(pb.balance(), 1_500_000_000);
        assert_eq!(pb.drawdown_pct(), 25);
    }

    // ── Size clamping ───────────────────────────────────────────────────

    #[test]
    fn test_size_clamped_to_min() {
        let c = compute_conviction(55.0, 55.0, 100_000_000, 0, 0);
        assert_eq!(c.size_lamports, MIN_SIZE_LAMPORTS, "should clamp to min: {}", c.size_lamports);
    }

    #[test]
    fn test_size_clamped_to_max() {
        let c = compute_conviction(55.0, 55.0, 10_000_000_000, 0, 0);
        assert_eq!(c.size_lamports, MAX_SIZE_LAMPORTS, "should clamp to max: {}", c.size_lamports);
    }

    #[test]
    fn test_zero_wallet_produces_zero_size() {
        let c = compute_conviction(55.0, 55.0, 0, 0, 0);
        assert_eq!(c.size_lamports, 0);
    }

    // ── Conviction tier assignment ──────────────────────────────────────

    #[test]
    fn test_conviction_tiers() {
        assert_eq!(conviction_tier(0), 0);
        assert_eq!(conviction_tier(100), 0);
        assert_eq!(conviction_tier(449), 0);
        assert_eq!(conviction_tier(450), 1);
        assert_eq!(conviction_tier(500), 1);
        assert_eq!(conviction_tier(549), 1);
        assert_eq!(conviction_tier(550), 2);
        assert_eq!(conviction_tier(700), 2);
        assert_eq!(conviction_tier(1000), 2);
    }

    // ── Bilinear interpolation ──────────────────────────────────────────

    #[test]
    fn test_bilerp_at_cell_origin() {
        assert_eq!(bilerp_int(&P_LUT, 0, 0, 0, 0), 440);
        assert_eq!(bilerp_int(&R_LUT, 0, 0, 0, 0), 4300);
    }

    #[test]
    fn test_bilerp_at_last_cell() {
        assert_eq!(bilerp_int(&P_LUT, 3, 3, FRAC_ONE, FRAC_ONE), 520);
        assert_eq!(bilerp_int(&R_LUT, 3, 3, FRAC_ONE, FRAC_ONE), 520);
    }

    #[test]
    fn test_bilerp_midpoint() {
        // Midpoint between cells (0,0) and (1,0): (440 + 600) / 2 = 520
        let result = bilerp_int(&P_LUT, 0, 0, FRAC_ONE / 2, 0);
        assert!((result as i32 - 520).abs() <= 1, "midpoint p: got {result}");
    }

    #[test]
    fn test_bucket_frac_int_basics() {
        // Below first bound → bucket 0, frac 0
        let (b, f) = bucket_frac_int(3000, &MAG_BOUNDS_X100);
        assert_eq!(b, 0);
        assert_eq!(f, 0);

        // At first bound → bucket 0, frac 0
        let (b, f) = bucket_frac_int(4000, &MAG_BOUNDS_X100);
        assert_eq!(b, 0);
        assert_eq!(f, 0);

        // Midpoint of first bucket (45.0 → 4500)
        let (b, f) = bucket_frac_int(4500, &MAG_BOUNDS_X100);
        assert_eq!(b, 0);
        assert_eq!(f, FRAC_ONE / 2); // 512

        // At second bound (50.0 → 5000)
        let (b, f) = bucket_frac_int(5000, &MAG_BOUNDS_X100);
        assert_eq!(b, 1);
        assert_eq!(f, 0);

        // Beyond last bound (80.0 → 8000)
        let (b, f) = bucket_frac_int(8000, &MAG_BOUNDS_X100);
        assert_eq!(b, 3);
        assert_eq!(f, FRAC_ONE); // clamped to 1.0
    }

    // ── Bankroll source abstraction ─────────────────────────────────────

    #[test]
    fn test_bankroll_source_paper() {
        let src = BankrollSource::Paper(PaperBankroll::new(1_000_000_000));
        assert_eq!(src.balance(), 1_000_000_000);
        src.apply_paper_pnl(-200_000_000);
        assert_eq!(src.balance(), 800_000_000);
        assert_eq!(src.drawdown_pct(), 20);
    }

    #[test]
    fn test_bankroll_source_live() {
        let src = BankrollSource::Live {
            cached_balance: AtomicU64::new(5_000_000_000),
        };
        assert_eq!(src.balance(), 5_000_000_000);
        src.apply_paper_pnl(-1_000_000_000); // no-op for Live
        assert_eq!(src.balance(), 5_000_000_000);
        assert_eq!(src.drawdown_pct(), 0);
    }

    // ── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn test_extreme_mag_and_score_clamped() {
        let c_low = compute_conviction(0.0, 0.0, 1_000_000_000, 0, 0);
        let c_high = compute_conviction(100.0, 100.0, 1_000_000_000, 0, 0);
        assert!(c_low.p_permille > 0);
        assert!(c_high.p_permille > 0);
    }

    #[test]
    fn test_many_open_positions_still_works() {
        let c = compute_conviction(55.0, 55.0, 1_000_000_000, 255, 0);
        assert!(c.f_permille < 50, "255 open should crush f: {}", c.f_permille);
    }

    // ── Integration: full pipeline numeric check ────────────────────────

    #[test]
    fn test_full_pipeline_numeric() {
        // mag=55, score=55, 1 SOL, no open, no drawdown
        let c = compute_conviction(55.0, 55.0, 1_000_000_000, 0, 0);

        // p near P_LUT[1][0] = 600
        assert!(c.p_permille >= 550 && c.p_permille <= 650, "p: {}", c.p_permille);

        // R_raw near R_LUT[1][0]=1100. Fee-adjusted: (1100×200/100 - 210)/(200+210) ≈ 485
        assert!(c.r_x100 >= 400 && c.r_x100 <= 600, "R(fee-adj): {}", c.r_x100);

        // f_raw ≈ 600 - 400*100/485 ≈ 518; f_half ≈ 259
        assert!(c.f_permille >= 220 && c.f_permille <= 300, "f: {}", c.f_permille);

        // size = 282 * 1B / 1000 = 282M → clamped to max 200M
        assert_eq!(c.size_lamports, MAX_SIZE_LAMPORTS);

        // tier: f ~282 < 450 → LOW
        assert_eq!(c.conviction_tier, 0);
    }

    #[test]
    fn test_repr_c_size() {
        // Verify EntryConviction has predictable size for FFI/storage.
        // u16+u16+u16 = 6 bytes, then 2 pad to align u64,
        // u64 = 8 bytes, u8 = 1 byte, [u8;5] = 5 bytes → total 22,
        // but repr(C) aligns to u64 boundary → 24 bytes.
        let size = std::mem::size_of::<EntryConviction>();
        assert!(size <= 24, "EntryConviction too large: {size}");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MOMENTUM ENGINE KELLY SIZING
// Simple Kelly formula for momentum engine position sizing.
// Used by MomentumEngine when kelly_sizing_enabled=true and bootstrap complete.
// ═══════════════════════════════════════════════════════════════════════════

/// Compute Kelly-optimal position size in lamports for the momentum engine.
///
/// Formula: f* = (p×b - q) / b  where b = avg_win_sol / avg_loss_sol
/// Size = wallet_balance × kelly_fraction × f*
///
/// Returns `None` if inputs are degenerate (negative EV, invalid inputs).
/// Caller must clamp to [min_probe_size_sol, max_probe_size_sol].
pub fn compute_momentum_kelly_size(
    wallet_balance_lamports: u64,
    win_rate: f64,
    avg_win_sol: f64,
    avg_loss_sol: f64,
    kelly_fraction: f64,
) -> Option<u64> {
    if wallet_balance_lamports == 0 { return None; }
    if !(0.0 < win_rate && win_rate < 1.0) { return None; }
    if avg_win_sol <= 0.0 || avg_loss_sol <= 0.0 { return None; }
    if !(0.0 < kelly_fraction && kelly_fraction <= 1.0) { return None; }

    let p = win_rate;
    let q = 1.0 - p;
    let b = avg_win_sol / avg_loss_sol;
    let kelly_f = (p * b - q) / b;

    if kelly_f <= 0.0 { return None; }

    let size_sol = (wallet_balance_lamports as f64 / 1e9) * kelly_fraction * kelly_f;
    if size_sol <= 0.0 { return None; }

    Some((size_sol * 1_000_000_000.0) as u64)
}

/// Minimal trade record for Kelly input computation.
#[derive(Debug, Clone)]
pub struct MomentumPaperTrade {
    pub net_pnl_sol: f64,
}

/// Parse recent trades and extract Kelly inputs (win_rate, avg_win_sol, avg_loss_sol).
/// Returns None if fewer than 10 trades or no wins/losses in sample.
pub fn compute_momentum_kelly_inputs(
    trades: &[MomentumPaperTrade],
    lookback: usize,
) -> Option<(f64, f64, f64)> {
    if trades.len() < 10 { return None; }

    let recent: Vec<&MomentumPaperTrade> = trades.iter().rev().take(lookback).collect();

    let wins: Vec<f64> = recent.iter()
        .filter_map(|t| if t.net_pnl_sol > 0.0 { Some(t.net_pnl_sol) } else { None })
        .collect();
    let losses: Vec<f64> = recent.iter()
        .filter_map(|t| if t.net_pnl_sol < 0.0 { Some(t.net_pnl_sol.abs()) } else { None })
        .collect();

    if wins.is_empty() || losses.is_empty() { return None; }

    let win_rate = wins.len() as f64 / recent.len() as f64;
    let avg_win = wins.iter().sum::<f64>() / wins.len() as f64;
    let avg_loss = losses.iter().sum::<f64>() / losses.len() as f64;

    Some((win_rate, avg_win, avg_loss))
}

#[cfg(test)]
mod momentum_kelly_tests {
    use super::*;

    #[test]
    fn test_momentum_kelly_positive_ev() {
        // 60% WR, avg win 0.03 SOL, avg loss 0.01 SOL, quarter-Kelly on 1 SOL wallet
        let result = compute_momentum_kelly_size(1_000_000_000, 0.60, 0.03, 0.01, 0.25);
        assert!(result.is_some());
        let size_sol = result.unwrap() as f64 / 1e9;
        assert!(size_sol > 0.05 && size_sol < 0.25, "Expected ~0.117 SOL, got {:.4}", size_sol);
    }

    #[test]
    fn test_momentum_kelly_negative_ev_returns_none() {
        let result = compute_momentum_kelly_size(1_000_000_000, 0.30, 0.01, 0.03, 0.25);
        assert!(result.is_none(), "Negative EV should return None");
    }

    #[test]
    fn test_momentum_kelly_zero_balance() {
        assert!(compute_momentum_kelly_size(0, 0.60, 0.03, 0.01, 0.25).is_none());
    }

    #[test]
    fn test_momentum_kelly_invalid_win_rate() {
        assert!(compute_momentum_kelly_size(1_000_000_000, 0.0, 0.03, 0.01, 0.25).is_none());
        assert!(compute_momentum_kelly_size(1_000_000_000, 1.0, 0.03, 0.01, 0.25).is_none());
    }

    #[test]
    fn test_momentum_kelly_scales_with_balance() {
        let s1 = compute_momentum_kelly_size(1_000_000_000, 0.60, 0.03, 0.01, 0.25).unwrap();
        let s2 = compute_momentum_kelly_size(2_000_000_000, 0.60, 0.03, 0.01, 0.25).unwrap();
        let ratio = s2 as f64 / s1 as f64;
        assert!((ratio - 2.0).abs() < 0.01, "Expected 2× scaling, got {:.3}", ratio);
    }

    #[test]
    fn test_momentum_kelly_inputs_insufficient() {
        let trades = vec![MomentumPaperTrade { net_pnl_sol: 0.01 }; 5];
        assert!(compute_momentum_kelly_inputs(&trades, 50).is_none());
    }

    #[test]
    fn test_momentum_kelly_inputs_sufficient() {
        let mut trades = Vec::new();
        for i in 0..30 {
            trades.push(MomentumPaperTrade {
                net_pnl_sol: if i % 3 == 0 { -0.005 } else { 0.02 },
            });
        }
        let result = compute_momentum_kelly_inputs(&trades, 30);
        assert!(result.is_some());
        let (wr, avg_win, avg_loss) = result.unwrap();
        assert!(wr > 0.0 && wr < 1.0);
        assert!(avg_win > 0.0);
        assert!(avg_loss > 0.0);
    }

    #[test]
    fn test_momentum_kelly_breakeven_none() {
        // 50% WR, equal win/loss → f* = 0
        let result = compute_momentum_kelly_size(1_000_000_000, 0.50, 0.01, 0.01, 0.25);
        assert!(result.is_none(), "Breakeven Kelly should return None");
    }
}
