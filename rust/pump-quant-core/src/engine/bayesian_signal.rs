//! Bayesian posterior half-Kelly signal engine — integer-only, zero-heap.
//!
//! Replaces signal_engine.rs composite score with a Bayesian Beta(α,β) tracker.
//! Every function is `#[inline(always)]` and designed for <10ns hot-path execution.
//!
//! Output: f̂*(t) in permille — the estimated half-Kelly fraction, updated live.
//! Thresholds are derived from the entry conviction's f*_entry, not magic numbers.
//! The system speaks one language everywhere: Kelly fractions.

use crate::feeds::FeedSource;
use super::ride_state::SignalState;

// ───────────────────────────── Evidence Weight Constants ──────────────────

/// Evidence weight multipliers indexed by `[event_type][source_index]`.
///   event_type 0 = buy, 1 = sell.
///   source_index: PumpPortal=0, Helius=1, ShredStream=2, CoreCast=3.
///
/// CoreCast sells get 2.5× weight (can verify creator identity).
/// ShredStream gets significant boost — pre-confirmation data from Jito WL
/// represents genuine market activity ~80-200ms before any websocket feed.
/// Higher signal quality justifies stronger evidence weighting.
///
/// ```text
///                    PumpPortal  Helius  ShredStream  CoreCast
///  buy  row (0):     [10,       10,     15,          10      ]
///  sell row (1):     [10,       10,     20,          25      ]
/// ```
pub const EVIDENCE_WEIGHTS: [[u8; 4]; 2] = [
    [10, 10, 15, 10], // buy  (ShredStream: 1.5× base — pre-confirmation buy = strong signal)
    [10, 10, 20, 25], // sell (ShredStream: 2.0× base — pre-confirmation sell = early warning)
];

/// Creator sell — worth 5× a normal sell (insider information).
pub const CREATOR_SELL_WEIGHT: u8 = 50;

/// Whale sell (>2 SOL) — worth 3× a normal sell.
pub const WHALE_SELL_WEIGHT: u8 = 30;

/// Bonus α_x16 increment for a unique new buyer wallet.
/// Applied as additional alpha_x16 increment after normal update.
pub const UNIQUE_BUYER_BONUS: u8 = 5;

/// Prior pseudo-observation count by conviction tier.
/// Strengthened from [6, 9, 13] to reduce premature exits.
/// With weaker priors, 57% of trades were exiting as momentum_decay_flat in <100ms
/// because the model reached Exit state before any confirming evidence arrived.
/// LOW (tier=0):  8 total → weak prior, needs ~4-5 events to shift
/// MED (tier=1):  12 total → moderate prior, needs ~7-8 events to shift
/// HIGH (tier=2): 18 total → strong prior, needs ~10-12 events to override
const PRIOR_STRENGTH: [u16; 3] = [8, 12, 18];

/// Decay multiplier numerator: 240/256 ≈ 0.9375 per tick.
/// Half-life: ln(2) / ln(256/240) ≈ 10.4 ticks × 500ms ≈ 5.2 seconds.
const DECAY_NUMER: u32 = 240;
/// Decay denominator as right-shift (divide by 256).
const DECAY_DENOM_SHIFT: u32 = 8;
/// Minimum α/β in x16: 1.0 in natural units. Prevents div-by-zero.
const MIN_AB_X16: u16 = 16;

// ───────────────────────────── FeedSource → index ────────────────────────

/// Map FeedSource to LUT column index. Matches real enum variant order.
#[inline(always)]
fn feed_source_idx(source: FeedSource) -> usize {
    match source {
        FeedSource::PumpPortal  => 0,
        FeedSource::Helius      => 1,
        FeedSource::ShredStream => 2,
        FeedSource::CoreCast    => 3,
    }
}

// ───────────────────────────── BayesianSignal Struct ─────────────────────

/// Bayesian posterior tracker for pump-alive conviction.
/// Updated on every buy/sell event and every 500ms decay tick.
/// 12 bytes total — inlined into RideState v3 (no separate allocation).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct BayesianSignal {
    /// Beta distribution α × 16 (4-bit fractional precision).
    /// Range: [16, 65535]. Minimum = 1.0 (16 >> 4).
    pub alpha_x16: u16,

    /// Beta distribution β × 16.
    /// Range: [16, 65535]. Minimum = 1.0.
    pub beta_x16: u16,

    /// Current reward ratio estimate × 100.
    /// Initialized from EntryConviction.r_x100. Updated upward only.
    pub r_est_x100: u16,

    /// Peak MFE (maximum favorable excursion) in basis points from entry.
    /// Used for R̂(t) upward-only update.
    pub peak_mfe_bp: i16,

    /// Kelly f* at entry in permille (copied from EntryConviction).
    /// Immutable after init. Used as denominator for signal thresholds.
    pub entry_f_permille: u16,

    /// Entry win probability in permille (copied from EntryConviction).
    /// Used to calibrate the initial Beta prior.
    pub entry_p_permille: u16,
}

// 12 bytes, no padding.
const _: () = assert!(core::mem::size_of::<BayesianSignal>() == 12);

impl BayesianSignal {
    /// Initialize from EntryConviction at position open.
    ///
    /// Sets Beta(α₀, β₀) such that α₀/(α₀+β₀) ≈ p_entry
    /// and α₀ + β₀ = PRIOR_STRENGTH[tier].
    ///
    /// All values stored ×16 for 4-bit fractional precision.
    #[inline(always)]
    pub fn from_conviction(
        p_permille: u16,
        r_x100: u16,
        f_permille: u16,
        conviction_tier: u8,
    ) -> Self {
        let tier = (conviction_tier as usize).min(2);
        let total = PRIOR_STRENGTH[tier];

        // α₀ = round(p × total / 1000), clamped to [1, total-1]
        let alpha_raw = ((p_permille as u32 * total as u32 + 500) / 1000)
            .max(1)
            .min(total as u32 - 1) as u16;
        let beta_raw = total - alpha_raw;

        Self {
            alpha_x16: alpha_raw << 4,
            beta_x16: beta_raw << 4,
            r_est_x100: r_x100,
            peak_mfe_bp: 0,
            entry_f_permille: f_permille,
            entry_p_permille: p_permille,
        }
    }

    /// Update Beta posterior with a trade event.
    ///
    /// `is_buy`:      true → α evidence, false → β evidence
    /// `sol_msol`:    trade size in milli-SOL (1 SOL = 1000)
    /// `source`:      which feed reported this event (for weight lookup)
    /// `weight_mult`: caller-supplied multiplier:
    ///                  10 = normal trade (1.0×)
    ///                  CREATOR_SELL_WEIGHT (50) = creator dumping (5.0×)
    ///                  WHALE_SELL_WEIGHT (30) = whale sell (3.0×)
    ///
    /// Weight formula:
    ///   base = EVIDENCE_WEIGHTS[is_sell][source]
    ///   size_factor = clamp(1 + sol_msol / 500, 1, 16)
    ///   w = base × size_factor × weight_mult / 10
    ///
    /// Performance: 4 integer ops + 1 saturating_add + 1 branch. <10ns.
    #[inline(always)]
    pub fn update_evidence(
        &mut self,
        is_buy: bool,
        sol_msol: u16,
        source: FeedSource,
        weight_mult: u8,
    ) {
        // Look up base weight: buy=row0, sell=row1
        let base = EVIDENCE_WEIGHTS[(!is_buy) as usize][feed_source_idx(source)] as u32;

        // Size scaling: 1 + sol_msol/500, capped at 16.
        let size_factor = (1u32 + sol_msol as u32 / 500).min(16);

        // Total weight (in x16 units approximately):
        let w = (base * size_factor * weight_mult as u32 / 10).min(4080) as u16;

        if is_buy {
            self.alpha_x16 = self.alpha_x16.saturating_add(w);
        } else {
            self.beta_x16 = self.beta_x16.saturating_add(w);
        }
    }

    /// Exponential forgetting. Called every 500ms tick.
    ///
    /// Multiplies both α and β by 240/256 ≈ 0.9375 per tick.
    /// Half-life ≈ 5.2 seconds. Clamps to MIN_AB_X16 to prevent div-by-zero.
    ///
    /// Performance: 2 multiplies + 2 right-shifts + 2 max. No branches. <5ns.
    #[inline(always)]
    pub fn decay_tick(&mut self) {
        self.alpha_x16 = ((self.alpha_x16 as u32 * DECAY_NUMER) >> DECAY_DENOM_SHIFT)
            .max(MIN_AB_X16 as u32) as u16;
        self.beta_x16 = ((self.beta_x16 as u32 * DECAY_NUMER) >> DECAY_DENOM_SHIFT)
            .max(MIN_AB_X16 as u32) as u16;
    }

    /// Compute current half-Kelly fraction in permille from Bayesian posterior.
    ///
    /// Returns: signed i16. Positive = edge exists. Zero/negative = no edge → exit.
    ///
    /// Integer formula:
    ///   p_x1000 = alpha_x16 × 1000 / (alpha_x16 + beta_x16)
    ///   numerator = p_x1000 × (r_est_x100 + 100) - 100_000
    ///   f_half_permille = numerator / (2 × r_est_x100)
    ///
    /// Operation count: 3 multiplies + 1 divide + 1 subtract. <10ns.
    #[inline(always)]
    pub fn current_f_permille(&self) -> i16 {
        let a = self.alpha_x16 as u32;
        let b = self.beta_x16 as u32;
        let ab = a + b; // guaranteed ≥ 32 (2 × MIN_AB_X16)

        // p̂ × 1000
        let p_x1000 = (a * 1000) / ab;

        let r = self.r_est_x100.max(1) as u32;
        let r_plus_1_x100 = r + 100;

        // p_x1000 × r_plus_1_x100 max: 1000 × 65635 ≈ 65M, fits u32
        let numerator = (p_x1000 * r_plus_1_x100) as i32 - 100_000;

        // half-Kelly: / (2 × r)
        let f = numerator / (2 * r as i32);

        f.clamp(-1000, 1000) as i16
    }

    /// Map current f̂*(t) to a SignalState using Kelly-derived thresholds.
    ///
    /// Thresholds are fractions of entry_f_permille:
    ///   StrongPump:  f̂ > 0.70 × f_entry
    ///   Sustained:   f̂ > 0.35 × f_entry
    ///   Weakening:   f̂ > 0
    ///   Exit:        f̂ ≤ 0
    ///
    /// Integer approximations: 0.70 ≈ 179/256, 0.35 ≈ 90/256.
    ///
    /// Performance: 2 multiplies + 2 shifts + 3 comparisons. <3ns.
    #[inline(always)]
    pub fn signal_state(&self) -> SignalState {
        let f_hat = self.current_f_permille() as i32;
        let f_entry = self.entry_f_permille as i32;

        if f_entry == 0 {
            return SignalState::Exit;
        }

        // 0.70 × f_entry ≈ f_entry × 179 >> 8
        let strong_thresh = (f_entry * 179) >> 8;
        // 0.35 × f_entry ≈ f_entry × 90 >> 8
        let sustain_thresh = (f_entry * 90) >> 8;

        if f_hat > strong_thresh {
            SignalState::StrongPump
        } else if f_hat > sustain_thresh {
            SignalState::Sustained
        } else if f_hat > 0 {
            SignalState::Weakening
        } else {
            SignalState::Exit
        }
    }

    /// Update R̂(t) from realized PnL trajectory. Upward-only.
    ///
    /// Called when price updates in on_tick. Only revises R̂ upward because
    /// observing a higher MFE is evidence of larger available reward.
    ///
    /// EMA-8 smoothing: R̂ = (R̂ × 7 + implied_R) / 8
    ///
    /// `current_pnl_bp`: unrealized PnL in basis points from entry.
    /// `avg_loss_bp`:    configured average loss (e.g. 300bp from historical data).
    #[inline(always)]
    pub fn update_r_estimate(&mut self, current_pnl_bp: i16, avg_loss_bp: u16) {
        if current_pnl_bp > self.peak_mfe_bp {
            self.peak_mfe_bp = current_pnl_bp;
        }

        let avg = avg_loss_bp.max(1) as u32;
        let implied_r_x100 = (self.peak_mfe_bp.max(0) as u32 * 100) / avg;

        if implied_r_x100 > self.r_est_x100 as u32 {
            self.r_est_x100 =
                (((self.r_est_x100 as u32) * 7 + implied_r_x100) >> 3) as u16;
        }
    }
}

// ───────────────────────────── Bloom filter (moved from signal_engine) ───

/// Approximate unique wallet count from 64-bit bloom filter.
///
/// Uses popcount × 45 / 64 ≈ popcount × ln(2) for 2-hash bloom filter.
/// Maximum-likelihood estimate of distinct insertions.
#[inline(always)]
pub fn bloom_count(bloom: &[u8; 8]) -> u8 {
    let bits = u64::from_le_bytes(*bloom);
    let pop = bits.count_ones() as u16;
    ((pop * 45) >> 6) as u8
}

/// Insert a wallet hash into a 64-bit bloom filter (2 hash functions).
///
/// h1 = bits [0:5] of wallet_hash  (& 0x3F)
/// h2 = bits [16:21] of wallet_hash (>> 16 & 0x3F)
#[inline(always)]
pub fn bloom_insert(bloom: &mut [u8; 8], wallet_hash: u64) {
    let h1 = (wallet_hash & 0x3F) as u32;
    let h2 = ((wallet_hash >> 16) & 0x3F) as u32;

    let bits = u64::from_le_bytes(*bloom);
    let updated = bits | (1u64 << h1) | (1u64 << h2);
    *bloom = updated.to_le_bytes();
}

// ───────────────────────────── Ring buffer helpers (kept from signal_engine) ─

/// Count events in last `window_ms` from a ring buffer of relative timestamps.
///
/// Ring stores timestamps as u16 ms offsets from entry.
/// `ring_len` is the number of valid entries (≤ ring.len()).
/// Handles ring wrap correctly.
///
/// For small fixed-size rings (≤ 20 elements), the compiler unrolls this loop.
#[inline(always)]
pub fn count_in_window(
    ring: &[u16],
    ring_idx: u8,
    ring_len: u8,
    now_rel_ms: u16,
    window_ms: u16,
) -> u8 {
    let len = ring_len.min(ring.len() as u8) as usize;
    let cap = ring.len();

    let threshold = now_rel_ms.saturating_sub(window_ms);

    let mut count: u8 = 0;
    let mut i: usize = 0;
    while i < len {
        let idx = (ring_idx as usize + cap - 1 - i) % cap;
        let ts = unsafe { *ring.get_unchecked(idx) };
        count += (ts >= threshold && ts <= now_rel_ms) as u8;
        i += 1;
    }

    count
}

/// Compute sell pressure ratio (0–255).
///
/// ratio = sell_rate × 255 / (buy_rate + sell_rate).
/// Returns 0 if no events. Returns 255 if all sells.
#[inline(always)]
pub fn sell_pressure_ratio(buy_rate: u8, sell_rate: u8) -> u8 {
    let total = buy_rate as u16 + sell_rate as u16;
    if total == 0 {
        return 0;
    }
    ((sell_rate as u16 * 255) / total) as u8
}

// ───────────────────────────── Tests ─────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Size assertion ──────────────────────────────────────────────

    #[test]
    fn test_struct_size() {
        assert_eq!(core::mem::size_of::<BayesianSignal>(), 12);
    }

    // ── Test 1: Fresh initialization → StrongPump ───────────────────

    #[test]
    fn test_fresh_init_strong_pump() {
        let sig = BayesianSignal::from_conviction(560, 1100, 248, 1);

        // MED tier: total=12, alpha_raw = (560*12+500)/1000 = 7220/1000 = 7
        assert_eq!(sig.alpha_x16, 112);  // 7 << 4
        assert_eq!(sig.beta_x16, 80);    // 5 << 4

        let f = sig.current_f_permille();
        // a=112, b=80, ab=192
        // p_x1000 = 112000/192 = 583
        // numerator = 583*1100 - 417*100 = 641300 - 41700 = 599600
        // f = 599600/2200 = 272
        assert_eq!(f, 272);

        let state = sig.signal_state();
        // strong_thresh = 248*179>>8 = 44392>>8 = 173
        // 272 > 173 → StrongPump
        assert_eq!(state, SignalState::StrongPump);
    }

    // ── Test 2: Five sells drive to Sustained ───────────────────────

    #[test]
    fn test_five_sells_sustained() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);

        // 5 sells × 1.0 SOL (1000 msol), PumpPortal, weight_mult=10
        for _ in 0..5 {
            sig.update_evidence(false, 1000, FeedSource::PumpPortal, 10);
        }

        // base=10, size_factor=1+1000/500=3, w=10*3*10/10=30 per sell
        // beta_x16 = 80 + 5*30 = 230
        assert_eq!(sig.alpha_x16, 112);
        assert_eq!(sig.beta_x16, 230);

        let f = sig.current_f_permille();
        // a=112, b=230, ab=342
        // p_x1000 = 112000/342 = 327
        // f = (327*1100 - 673*100) / 2200 = (359700 - 67300) / 2200 = 292400/2200 = 132
        assert_eq!(f, 132);

        let state = sig.signal_state();
        // strong_thresh=173, sustain_thresh=86
        // 173 > 132 > 86 → Sustained
        assert_eq!(state, SignalState::Sustained);
    }

    // ── Test 3: Heavy selling drives to Exit ────────────────────────

    #[test]
    fn test_heavy_selling_to_exit() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);

        // 5 sells × 1.0 SOL → beta = 64 + 150 = 214
        for _ in 0..5 {
            sig.update_evidence(false, 1000, FeedSource::PumpPortal, 10);
        }
        // 10 sells × 0.5 SOL → w=20 each, beta += 200 → 414
        for _ in 0..10 {
            sig.update_evidence(false, 500, FeedSource::PumpPortal, 10);
        }
        // 5 sells × 1.0 SOL → w=30 each, beta += 150 → 564
        for _ in 0..5 {
            sig.update_evidence(false, 1000, FeedSource::PumpPortal, 10);
        }
        // 20 sells × 2.0 SOL → w=50 each, beta += 1000 → 1564
        for _ in 0..20 {
            sig.update_evidence(false, 2000, FeedSource::PumpPortal, 10);
        }

        assert_eq!(sig.alpha_x16, 112);
        assert_eq!(sig.beta_x16, 1580);

        let f = sig.current_f_permille();
        // a=112, b=1580, ab=1692
        // p_x1000 = 112000/1692 = 66
        // numer = 66*1100 - 934*100 = -20800
        // Rust i32 division truncates toward zero: -20800/2200 = -9
        assert_eq!(f, -9);

        assert_eq!(sig.signal_state(), SignalState::Exit);
    }

    // ── Test 4: Creator sell → massive β spike → Weakening ──────────

    #[test]
    fn test_creator_sell_weakening() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);

        // 1 creator sell, 2.0 SOL, CoreCast, weight_mult=50
        sig.update_evidence(false, 2000, FeedSource::CoreCast, CREATOR_SELL_WEIGHT);

        // base = EVIDENCE_WEIGHTS[1][3] = 25 (CoreCast is index 3)
        // size_factor = 1 + 2000/500 = 5
        // w = 25*5*50/10 = 625
        assert_eq!(sig.alpha_x16, 112);
        assert_eq!(sig.beta_x16, 80 + 625);

        let f = sig.current_f_permille();
        // a=112, b=705, ab=817
        // p_x1000 = 112000/817 = 137
        // f = (137*1100 - 863*100) / 2200 = (150700 - 86300) / 2200 = 64400/2200 = 29
        assert_eq!(f, 29);

        // sustain_thresh=86, 86>29>0 → Weakening
        assert_eq!(sig.signal_state(), SignalState::Weakening);
    }

    // ── Test 5: Healthy pump — 8 buys, 1 sell → StrongPump ─────────

    #[test]
    fn test_healthy_pump_strong() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);

        // 8 buys × 0.5 SOL, PumpPortal, weight_mult=10
        // base=10, size_factor=1+500/500=2, w=20
        for _ in 0..8 {
            sig.update_evidence(true, 500, FeedSource::PumpPortal, 10);
        }

        // 1 sell × 0.3 SOL, PumpPortal, weight_mult=10
        // base=10, size_factor=1+300/500=1, w=10
        sig.update_evidence(false, 300, FeedSource::PumpPortal, 10);

        assert_eq!(sig.alpha_x16, 112 + 8 * 20);  // 272
        assert_eq!(sig.beta_x16, 80 + 10);         // 90

        let f = sig.current_f_permille();
        // a=272, b=90, ab=362
        // p_x1000 = 272000/362 = 751
        // f = (751*1100 - 249*100) / 2200 = (826100 - 24900) / 2200 = 801200/2200 = 364
        assert_eq!(f, 364);

        assert_eq!(sig.signal_state(), SignalState::StrongPump);
    }

    // ── Test 6: Decay preserves ratio ───────────────────────────────

    #[test]
    fn test_decay_preserves_ratio() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);

        // 10 decay ticks
        for _ in 0..10 {
            sig.decay_tick();
        }

        // After 10 ticks: α≈56, β≈39 (with stronger priors, starts higher)
        // The exact values depend on integer truncation each step.
        // Verify ratio is approximately preserved and state stays StrongPump.
        let f = sig.current_f_permille();
        // p̂ ≈ 56/95 = 0.589 → f ≈ 275
        // Allow a small range for integer truncation drift.
        assert!(
            f >= 260 && f <= 290,
            "Decay should preserve ratio approximately, got f={f}"
        );
        assert_eq!(sig.signal_state(), SignalState::StrongPump);
    }

    // ── Test 7: LOW tier — weaker prior, faster transitions ─────────

    #[test]
    fn test_low_tier_faster_transitions() {
        let sig = BayesianSignal::from_conviction(560, 1100, 248, 0);

        // LOW: total=8, alpha_raw=(560*8+500)/1000=4980/1000=4
        assert_eq!(sig.alpha_x16, 64);   // 4 << 4
        assert_eq!(sig.beta_x16, 64);    // 4 << 4

        let f = sig.current_f_permille();
        // a=64, b=64, ab=128
        // p_x1000 = 500
        // f = (500*1100 - 500*100) / 2200 = (550000 - 50000) / 2200 = 500000/2200 = 227
        assert_eq!(f, 227);
        assert_eq!(sig.signal_state(), SignalState::StrongPump);

        // 3 sells × 1 SOL → w=30 each → beta += 90
        let mut sig2 = sig;
        for _ in 0..3 {
            sig2.update_evidence(false, 1000, FeedSource::PumpPortal, 10);
        }

        assert_eq!(sig2.beta_x16, 64 + 90); // 154

        let f2 = sig2.current_f_permille();
        // a=64, b=154, ab=218
        // p_x1000 = 64000/218 = 293
        // f = (293*1100 - 707*100) / 2200 = (322300 - 70700) / 2200 = 251600/2200 = 114
        assert_eq!(f2, 114);

        // strong_thresh=173, sustain_thresh=86
        // 173 > 114 > 86 → Sustained
        assert_eq!(sig2.signal_state(), SignalState::Sustained);
        // LOW tier drops to Sustained after just 3 sells (vs 5 for MED).
    }

    // ── HIGH tier initialization ────────────────────────────────────

    #[test]
    fn test_high_tier_init() {
        let sig = BayesianSignal::from_conviction(560, 1100, 248, 2);

        // HIGH: total=18, alpha_raw=(560*18+500)/1000=10580/1000=10
        assert_eq!(sig.alpha_x16, 160); // 10 << 4
        assert_eq!(sig.beta_x16, 128);  // 8 << 4

        let f = sig.current_f_permille();
        // a=160, b=128, ab=288
        // p_x1000 = 160000/288 = 555
        // f = (555*1100 - 445*100) / 2200 = (610500 - 44500) / 2200 = 566000/2200 = 257
        assert_eq!(f, 257);
        assert_eq!(sig.signal_state(), SignalState::StrongPump);
    }

    // ── update_r_estimate ───────────────────────────────────────────

    #[test]
    fn test_r_estimate_upward_only() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);

        assert_eq!(sig.r_est_x100, 1100);
        assert_eq!(sig.peak_mfe_bp, 0);

        // PnL goes to +500bp, avg_loss=300bp
        sig.update_r_estimate(500, 300);
        assert_eq!(sig.peak_mfe_bp, 500);
        // implied_r_x100 = 500*100/300 = 166 — less than 1100, no update
        assert_eq!(sig.r_est_x100, 1100);

        // PnL goes to +5000bp
        sig.update_r_estimate(5000, 300);
        assert_eq!(sig.peak_mfe_bp, 5000);
        // implied_r_x100 = 5000*100/300 = 1666 > 1100
        // r_est = (1100*7 + 1666) >> 3 = (7700+1666)/8 = 9366/8 = 1170
        assert_eq!(sig.r_est_x100, 1170);

        // Negative PnL doesn't change peak, but EMA still fires
        // because implied_r (1666) is still > current r_est (1170).
        // r_est = (1170*7+1666)>>3 = 9856>>3 = 1232
        sig.update_r_estimate(-200, 300);
        assert_eq!(sig.peak_mfe_bp, 5000);
        assert_eq!(sig.r_est_x100, 1232);
    }

    // ── Bloom filter ────────────────────────────────────────────────

    #[test]
    fn test_bloom_empty() {
        let bloom = [0u8; 8];
        assert_eq!(bloom_count(&bloom), 0);
    }

    #[test]
    fn test_bloom_full() {
        let bloom = [0xFFu8; 8];
        assert_eq!(bloom_count(&bloom), 45);
    }

    #[test]
    fn test_bloom_insert_and_count() {
        let mut bloom = [0u8; 8];
        bloom_insert(&mut bloom, 0x0010_0005);
        let c = bloom_count(&bloom);
        assert!(c >= 1 && c <= 2, "got {c}");
    }

    // ── count_in_window ─────────────────────────────────────────────

    #[test]
    fn test_count_in_window_basic() {
        let ring: [u16; 8] = [100, 200, 300, 400, 500, 600, 700, 800];
        let count = count_in_window(&ring, 0, 8, 850, 300);
        assert_eq!(count, 3);
    }

    #[test]
    fn test_count_in_window_empty() {
        let ring: [u16; 8] = [0; 8];
        let count = count_in_window(&ring, 0, 0, 1000, 500);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_in_window_wrap() {
        let ring: [u16; 8] = [700, 800, 900, 100, 200, 300, 400, 500];
        let count = count_in_window(&ring, 3, 8, 900, 250);
        assert_eq!(count, 3);
    }

    // ── sell_pressure_ratio ─────────────────────────────────────────

    #[test]
    fn test_sell_pressure_no_events() {
        assert_eq!(sell_pressure_ratio(0, 0), 0);
    }

    #[test]
    fn test_sell_pressure_all_buys() {
        assert_eq!(sell_pressure_ratio(10, 0), 0);
    }

    #[test]
    fn test_sell_pressure_all_sells() {
        assert_eq!(sell_pressure_ratio(0, 10), 255);
    }

    #[test]
    fn test_sell_pressure_balanced() {
        assert_eq!(sell_pressure_ratio(5, 5), 127);
    }

    // ── Edge cases ──────────────────────────────────────────────────

    #[test]
    fn test_zero_entry_f_permille_exits() {
        let sig = BayesianSignal::from_conviction(560, 1100, 0, 1);
        assert_eq!(sig.signal_state(), SignalState::Exit);
    }

    #[test]
    fn test_saturating_evidence() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);
        for _ in 0..1000 {
            sig.update_evidence(false, 5000, FeedSource::CoreCast, CREATOR_SELL_WEIGHT);
        }
        assert_eq!(sig.beta_x16, u16::MAX);
        assert!(sig.current_f_permille() < 0);
        assert_eq!(sig.signal_state(), SignalState::Exit);
    }

    #[test]
    fn test_decay_clamps_to_minimum() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 0);
        for _ in 0..500 {
            sig.decay_tick();
        }
        assert_eq!(sig.alpha_x16, MIN_AB_X16);
        assert_eq!(sig.beta_x16, MIN_AB_X16);
        let f = sig.current_f_permille();
        // Equal α/β → p=0.5, f = (500*1200-100000)/2200 = 227
        assert_eq!(f, 227);
    }

    #[test]
    fn test_shredstream_buy_boost() {
        let mut sig = BayesianSignal::from_conviction(560, 1100, 248, 1);
        sig.update_evidence(true, 1000, FeedSource::ShredStream, 10);
        // ShredStream buy: base=15, size_factor=1+1000/500=3, w=15*3*10/10=45
        assert_eq!(sig.alpha_x16, 112 + 45);

        let mut sig2 = BayesianSignal::from_conviction(560, 1100, 248, 1);
        sig2.update_evidence(true, 1000, FeedSource::PumpPortal, 10);
        // PumpPortal buy: base=10, size_factor=3, w=10*3*10/10=30
        assert_eq!(sig2.alpha_x16, 112 + 30);

        assert!(sig.alpha_x16 > sig2.alpha_x16);
    }
}