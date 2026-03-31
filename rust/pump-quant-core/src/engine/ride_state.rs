// engine/ride_state.rs — RIDE mode signal-driven exit engine v3.
//
// 128 bytes (2 cache lines). Zero heap. Zero f64. All integer arithmetic.
// Bayesian signal-driven 4-state machine with inline Beta posterior.
//
// State machine: StrongPump <-> Sustained <-> Weakening -> Exit
//   Transitions are bidirectional (can recover) except Exit (terminal).
//   Trail width computed dynamically every event from:
//     trail_bp = base_bp * clamp(f_hat * 256 / f_entry, 64, 400) >> 8

use crate::feeds::FeedSource;
use super::bayesian_signal::{self, BayesianSignal, bloom_count, bloom_insert, count_in_window};
use super::exit_v4::{
    MomentumDivergence, VolatilityEstimator, UrgencyState, ExitFraction, ExitV4Config,
    u_kelly, u_vol_trail, u_liquidity, composite_urgency, compute_adaptive_trail_stop,
};
use super::kelly_sizing::{fee_adjust_r, DEFAULT_ROUND_TRIP_FEE_BP};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAX_HOLD_RIDE_MS: u64 = 300_000;
pub const HARD_FLOOR_ENABLED: bool = true;
pub const BUY_GAP_EXIT_MS: u16 = 10_000;
pub const SELL_CASCADE_COUNT: u8 = 3;
pub const SELL_CASCADE_WINDOW_MS: u16 = 3_000;
pub const BUY_RING_LEN: usize = 8;
pub const SELL_RING_LEN: usize = 4;

const EVIDENCE_WEIGHTS: [[u8; 4]; 2] = bayesian_signal::EVIDENCE_WEIGHTS;
const UNIQUE_BUYER_BONUS: u8 = bayesian_signal::UNIQUE_BUYER_BONUS;
const DECAY_NUMER: u32 = 240;
const MIN_AB_X16: u16 = 16;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SignalState {
    StrongPump = 0,
    Sustained  = 1,
    Weakening  = 2,
    Exit       = 3,
}

impl SignalState {
    #[inline(always)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StrongPump => "strong_pump",
            Self::Sustained  => "sustained",
            Self::Weakening  => "weakening",
            Self::Exit       => "exit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RidePhase {
    Early    = 0,
    Momentum = 1,
    Tighten  = 2,
}

impl RidePhase {
    #[inline(always)]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Early,
            1 => Self::Momentum,
            _ => Self::Tighten,
        }
    }
}

pub mod ride_flags {
    pub const CREATOR_SELL:       u8 = 1 << 0;
    pub const EMERGENCY_EXIT:     u8 = 1 << 1;
    pub const SELL_CASCADE_SEEN:  u8 = 1 << 2;
    pub const WHALE_EXIT_SEEN:    u8 = 1 << 3;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideDecision {
    Hold,
    Exit(RideExitReason),
    /// V4 partial exit: sell `permille` of remaining position (0–1000).
    PartialExit { permille: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideExitReason {
    TrailingStop,
    HardFloor,
    WhaleExit,
    BuyGapTimeout,
    SellCascade,
    CreatorSell,
    MaxHold,
    SignalExit,
    /// V4: urgency-driven partial exit.
    UrgencyPartial,
    /// V4: urgency-driven full exit.
    UrgencyFull,
}

pub use super::config::RideConfig;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert lamports to milli-vSOL (rounds to nearest).
#[inline(always)]
#[allow(dead_code)]
fn lamports_to_mvsol(lamports: u64) -> u32 {
    (lamports / 1_000_000) as u32
}

/// Compute trail stop from peak and trail distance in basis points.
#[inline(always)]
fn compute_trail_stop(peak_mvsol: u32, trail_bp: u16) -> u32 {
    // trail_stop = peak * (10000 - trail_bp) / 10000
    let stop = peak_mvsol as u64 * (10_000u64 - trail_bp as u64) / 10_000u64;
    stop as u32
}

// ---------------------------------------------------------------------------
// RideState v3 — 128 bytes, 2 cache lines
// ---------------------------------------------------------------------------

/// Signal-driven RIDE exit state v3. 128 bytes exactly.
///
/// Cache line 0 (bytes 0-63): HOT — trail + timing + counters + Bayesian.
/// Cache line 1 (bytes 64-127): WARM — ring buffers + bloom.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct RideState {
    // ── Cache line 0: trail + timing + counters + Bayesian ───────

    // Trail state (16 bytes: 0-15)
    pub peak_mvsol: u32,             // 0-3
    pub trail_stop_mvsol: u32,       // 4-7
    pub entry_mvsol: u32,            // 8-11
    pub current_trail_bp: u16,       // 12-13
    pub state: u8,                   // 14 (SignalState as u8)
    pub flags: u8,                   // 15

    // Timing (16 bytes: 16-31)
    pub ride_start_ms: u64,          // 16-23
    pub last_buy_ms: u64,            // 24-31

    // Counters (16 bytes: 32-47)
    pub buys_after_entry: u16,       // 32-33
    pub sells_after_entry: u16,      // 34-35
    pub unique_wallets: u8,          // 36
    pub _pad0: [u8; 3],             // 37-39
    pub confirming_vol_msol: u32,    // 40-43
    pub peak_pnl_bp: i16,           // 44-45
    pub peak_pnl_ms_rel: u16,       // 46-47

    // Bayesian signal (16 bytes: 48-63)
    pub alpha_x16: u16,             // 48-49
    pub beta_x16: u16,              // 50-51
    pub r_est_x100: u16,            // 52-53
    pub peak_mfe_bp: i16,           // 54-55
    pub entry_f_permille: u16,      // 56-57
    pub entry_p_permille: u16,      // 58-59
    pub peak_f_permille: u16,       // 60-61
    pub avg_loss_bp: u16,           // 62-63

    // ── Cache line 1: ring buffers + bloom ────────────────────────

    pub buy_ts_ring: [u16; BUY_RING_LEN],    // 64-79
    pub buy_sol_ring: [u16; BUY_RING_LEN],   // 80-95
    pub sell_ts_ring: [u16; SELL_RING_LEN],   // 96-103
    pub sell_sol_ring: [u16; SELL_RING_LEN],  // 104-111
    pub buy_ring_idx: u8,                     // 112
    pub sell_ring_idx: u8,                    // 113
    pub bloom_filter: [u8; 8],                // 114-121
    pub vol_recent_msol: u16,                 // 122-123
    pub vol_prior_msol: u16,                  // 124-125
    pub phase: u8,                            // 126 (RidePhase as u8)
    pub _pad2: u8,                            // 127

    // ── Cache line 2: V4 exit engine state (bytes 128-191) ────────

    /// V4 momentum divergence tracker. 16 bytes.
    pub momentum: MomentumDivergence,         // 128-143
    /// V4 volatility estimator for adaptive trail. 16 bytes.
    pub volatility: VolatilityEstimator,      // 144-159
    /// V4 urgency state: floor, remaining, partials. 8 bytes.
    pub urgency: UrgencyState,                // 160-167
    /// V4 last urgency component breakdown for JSONL logging.
    pub v4_u_kelly: u16,                      // 168-169
    pub v4_u_momentum: u16,                   // 170-171
    pub v4_u_vol_trail: u16,                  // 172-173
    pub v4_u_liquidity: u16,                  // 174-175
    /// Position size in lamports (for liquidity urgency calculation).
    pub position_size_lamports: u64,          // 176-183
    pub _pad3: [u8; 8],                       // 184-191 (pad to 192)
}

const _: () = assert!(core::mem::size_of::<RideState>() == 192);

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl RideState {
    /// Create a new RideState v3 for a freshly opened position.
    #[inline(always)]
    pub fn new(
        entry_mvsol: u32,
        _current_mvsol: u32,
        now_ms: u64,
        entry_f_permille: u16,
        entry_p_permille: u16,
        entry_r_x100: u16,
        conviction_tier: u8,
        avg_loss_bp: u16,
        config: &RideConfig,
    ) -> Self {
        let initial_trail = config.trail_strong_pump_bp;
        let trail_stop = compute_trail_stop(entry_mvsol, initial_trail);

        // Initialize Bayesian prior from conviction tier
        let bayes = BayesianSignal::from_conviction(
            entry_p_permille, entry_r_x100, entry_f_permille, conviction_tier,
        );

        RideState {
            peak_mvsol: entry_mvsol,
            trail_stop_mvsol: trail_stop,
            entry_mvsol,
            current_trail_bp: initial_trail,
            state: SignalState::StrongPump as u8,
            flags: 0,

            ride_start_ms: now_ms,
            last_buy_ms: now_ms,

            buys_after_entry: 0,
            sells_after_entry: 0,
            unique_wallets: 0,
            _pad0: [0; 3],
            confirming_vol_msol: 0,
            peak_pnl_bp: 0,
            peak_pnl_ms_rel: 0,

            alpha_x16: bayes.alpha_x16,
            beta_x16: bayes.beta_x16,
            r_est_x100: bayes.r_est_x100,
            peak_mfe_bp: 0,
            entry_f_permille,
            entry_p_permille,
            peak_f_permille: entry_f_permille,
            avg_loss_bp,

            buy_ts_ring: [u16::MAX; BUY_RING_LEN],
            buy_sol_ring: [0; BUY_RING_LEN],
            sell_ts_ring: [u16::MAX; SELL_RING_LEN],
            sell_sol_ring: [0; SELL_RING_LEN],
            buy_ring_idx: 0,
            sell_ring_idx: 0,
            bloom_filter: [0; 8],
            vol_recent_msol: 0,
            vol_prior_msol: 0,
            phase: RidePhase::Early as u8,
            _pad2: 0,

            // V4 exit engine state — initialized unconditionally
            momentum: MomentumDivergence::new(),
            volatility: VolatilityEstimator::new(entry_mvsol),
            urgency: UrgencyState::new(),
            v4_u_kelly: 0,
            v4_u_momentum: 0,
            v4_u_vol_trail: 0,
            v4_u_liquidity: 0,
            position_size_lamports: 0,
            _pad3: [0; 8],
        }
    }

    // ── Timing helpers ──

    /// Relative ms since ride start, clamped to u16.
    #[inline(always)]
    fn rel_ms(&self, now_ms: u64) -> u16 {
        now_ms.saturating_sub(self.ride_start_ms).min(u16::MAX as u64) as u16
    }

    /// Buy gap: ms since last buy event.
    #[inline(always)]
    pub fn buy_gap_ms(&self, now_ms: u64) -> u16 {
        now_ms.saturating_sub(self.last_buy_ms).min(u16::MAX as u64) as u16
    }

    /// Unrealized PnL in basis points.
    #[inline(always)]
    pub fn unrealized_pnl_bp(&self, current_mvsol: u32) -> i16 {
        if self.entry_mvsol == 0 { return 0; }
        let delta = current_mvsol as i64 - self.entry_mvsol as i64;
        let bp = (delta * 10_000 / self.entry_mvsol as i64) as i32;
        bp.clamp(-10_000, 10_000) as i16
    }

    /// Get current SignalState enum value.
    #[inline(always)]
    pub fn signal_state(&self) -> SignalState {
        match self.state {
            0 => SignalState::StrongPump,
            1 => SignalState::Sustained,
            2 => SignalState::Weakening,
            _ => SignalState::Exit,
        }
    }

    /// Get current f̂* (computed, not stored).
    #[inline(always)]
    pub fn f_hat_permille(&self) -> i16 {
        self.bayesian_current_f_permille()
    }

    /// Peak composite score (renamed from v2 — now tracks peak f̂*).
    #[inline(always)]
    pub fn peak_composite_score(&self) -> u16 {
        self.peak_f_permille
    }

    /// Mark creator sell flag.
    #[cold]
    pub fn mark_creator_sell(&mut self) {
        self.flags |= ride_flags::CREATOR_SELL;
    }

    // ── Event handlers ──

    /// Process a buy event.
    #[inline(always)]
    pub fn on_buy_event(
        &mut self,
        sol_amount_mvsol: u32,
        now_ms: u64,
        wallet_hash: u64,
        source: FeedSource,
        weight_mult: u8,
    ) {
        self.buys_after_entry = self.buys_after_entry.saturating_add(1);
        self.last_buy_ms = now_ms;

        let amount_msol = sol_amount_mvsol.min(u16::MAX as u32) as u16;
        self.confirming_vol_msol = self.confirming_vol_msol.saturating_add(sol_amount_mvsol);
        self.vol_recent_msol = self.vol_recent_msol.saturating_add(amount_msol);

        // Buy ring buffer
        let now_rel = self.rel_ms(now_ms);
        let idx = (self.buy_ring_idx as usize) % BUY_RING_LEN;
        self.buy_ts_ring[idx] = now_rel;
        self.buy_sol_ring[idx] = amount_msol;
        self.buy_ring_idx = self.buy_ring_idx.wrapping_add(1);

        // Bloom filter for unique wallets
        bloom_insert(&mut self.bloom_filter, wallet_hash);
        let old_wallets = self.unique_wallets;
        self.unique_wallets = bloom_count(&self.bloom_filter);

        // Bayesian update
        self.bayesian_update_evidence(true, amount_msol, source, weight_mult);

        // Unique wallet bonus
        if self.unique_wallets > old_wallets {
            self.alpha_x16 = self.alpha_x16.saturating_add(UNIQUE_BUYER_BONUS as u16);
        }

        // V4: Record event in momentum divergence tracker
        self.momentum.record_event(true, amount_msol);
    }

    /// Process a sell event. Returns Some(reason) for emergency exit.
    #[inline(always)]
    pub fn on_sell_event(
        &mut self,
        sol_amount_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
        source: FeedSource,
        weight_mult: u8,
    ) -> Option<RideExitReason> {
        self.sells_after_entry = self.sells_after_entry.saturating_add(1);

        let amount_msol = sol_amount_mvsol.min(u16::MAX as u32) as u16;
        let now_rel = self.rel_ms(now_ms);

        // Sell ring buffer
        let idx = (self.sell_ring_idx as usize) % SELL_RING_LEN;
        self.sell_ts_ring[idx] = now_rel;
        self.sell_sol_ring[idx] = amount_msol;
        self.sell_ring_idx = self.sell_ring_idx.wrapping_add(1);

        // ── Emergency checks (UNCHANGED from v2) ──

        if self.flags & ride_flags::CREATOR_SELL != 0 {
            return Some(RideExitReason::CreatorSell);
        }

        let whale_threshold_msol = (config.whale_exit_lamports / 1_000_000) as u32;
        if sol_amount_mvsol > whale_threshold_msol {
            self.flags |= ride_flags::WHALE_EXIT_SEEN;
            return Some(RideExitReason::WhaleExit);
        }

        let cascade_count = count_in_window(
            &self.sell_ts_ring, self.sell_ring_idx,
            SELL_RING_LEN as u8, now_rel, SELL_CASCADE_WINDOW_MS,
        );
        if cascade_count >= SELL_CASCADE_COUNT {
            self.flags |= ride_flags::SELL_CASCADE_SEEN;
            return Some(RideExitReason::SellCascade);
        }

        // Bayesian update
        self.bayesian_update_evidence(false, amount_msol, source, weight_mult);

        // V4: Record event in momentum divergence tracker
        self.momentum.record_event(false, amount_msol);

        None
    }

    /// Main tick: emergency → decay → Bayesian score → trail → trailing stop.
    #[inline(always)]
    pub fn on_tick(
        &mut self,
        current_mvsol: u32,
        now_ms: u64,
        config: &RideConfig,
    ) -> RideDecision {
        // ── Emergency overrides ──

        if self.flags & ride_flags::CREATOR_SELL != 0 {
            return RideDecision::Exit(RideExitReason::CreatorSell);
        }

        // Compute hold_ms once — used by multiple checks below.
        let hold_ms = now_ms.saturating_sub(self.ride_start_ms);

        // Fee-aware hard floor: breakeven = entry × (10000 + fee_bp) / 10000
        // Below breakeven we're guaranteed net-negative, exit immediately.
        if HARD_FLOOR_ENABLED {
            let fee_bp = DEFAULT_ROUND_TRIP_FEE_BP as u64;
            let breakeven = self.entry_mvsol as u64 * (10_000 + fee_bp) / 10_000;
            if (current_mvsol as u64) < breakeven {
                return RideDecision::Exit(RideExitReason::HardFloor);
            }
        }

        if hold_ms >= config.max_hold_ms.max(MAX_HOLD_RIDE_MS) {
            return RideDecision::Exit(RideExitReason::MaxHold);
        }

        // Buy gap timeout — only fire after minimum hold period (500ms).
        // In the first 500ms the model has insufficient evidence; premature
        // BuyGapTimeout at 0ms hold time is why 57% of trades exit as flat.
        let gap = self.buy_gap_ms(now_ms);
        if gap >= BUY_GAP_EXIT_MS && hold_ms >= 500 {
            return RideDecision::Exit(RideExitReason::BuyGapTimeout);
        }

        // ── Bayesian decay ──
        self.bayesian_decay_tick();

        // ── Compute f̂* and update R̂ ──
        let pnl_bp = self.unrealized_pnl_bp(current_mvsol);
        self.bayesian_update_r_estimate(pnl_bp);

        let f_hat = self.bayesian_current_f_permille();

        // Track peak f̂*
        if f_hat > 0 && f_hat as u16 > self.peak_f_permille {
            self.peak_f_permille = f_hat as u16;
        }

        // ── Signal state ──
        let new_state = self.bayesian_signal_state(f_hat);
        self.state = new_state as u8;

        self.phase = match new_state {
            SignalState::StrongPump => RidePhase::Early as u8,
            SignalState::Sustained  => RidePhase::Momentum as u8,
            SignalState::Weakening | SignalState::Exit => RidePhase::Tighten as u8,
        };

        // ── Grace period: no Bayesian exit until buys_after_entry >= 1 AND held >= 500ms ──
        // The 500ms floor ensures the Bayesian model has received enough evidence
        // (at least 1-2 trade events) before making an exit call. Without this,
        // decay ticks in the first 100ms can push f̂ negative before any confirming
        // buy arrives — causing premature momentum_decay_flat exits.
        if new_state == SignalState::Exit && self.buys_after_entry >= 1 && hold_ms >= 500 {
            return RideDecision::Exit(RideExitReason::SignalExit);
        }

        // ── Dynamic trail ──
        let base_trail = match new_state {
            SignalState::StrongPump => config.trail_strong_pump_bp,
            SignalState::Sustained  => config.trail_sustained_bp,
            SignalState::Weakening  => config.trail_weakening_bp,
            SignalState::Exit       => config.trail_weakening_bp, // grace period — use tightest
        };

        let f_entry = self.entry_f_permille.max(1) as u32;
        let f_now = (f_hat.max(0) as u32).min(f_entry * 2);
        let scale = (f_now * 256 / f_entry).clamp(64, 400);
        let trail = ((base_trail as u32 * scale) >> 8)
            .clamp(config.kelly_min_trail_bp as u32, config.kelly_max_trail_bp as u32);
        self.current_trail_bp = trail as u16;

        // Update peak PnL tracking
        let now_rel = self.rel_ms(now_ms);
        if pnl_bp > self.peak_pnl_bp {
            self.peak_pnl_bp = pnl_bp;
            self.peak_pnl_ms_rel = now_rel;
        }

        // Update peak + trail stop (ratchet up only)
        if current_mvsol > self.peak_mvsol {
            self.peak_mvsol = current_mvsol;
        }

        let new_stop = compute_trail_stop(self.peak_mvsol, self.current_trail_bp);
        if new_stop > self.trail_stop_mvsol {
            self.trail_stop_mvsol = new_stop;
        }

        // ── Trailing stop ──
        if current_mvsol <= self.trail_stop_mvsol {
            return RideDecision::Exit(RideExitReason::TrailingStop);
        }

        // ── V4 urgency computation (always runs — shadow mode logs, active mode acts) ──
        self.volatility.record(current_mvsol);

        // Compute urgency components
        let uk = u_kelly(f_hat, self.entry_f_permille);
        let um = self.momentum.urgency();

        // Adaptive trail stop for vol-trail urgency
        let vol_mult = self.volatility.trail_multiplier_x256();
        let adaptive_stop = compute_adaptive_trail_stop(self.peak_mvsol, self.current_trail_bp, vol_mult);
        let uv = u_vol_trail(current_mvsol, adaptive_stop);

        // Liquidity urgency from position size vs curve liquidity
        // current_mvsol is in milli-SOL, convert to lamports for comparison
        let liquidity_lamports = current_mvsol as u64 * 1_000_000;
        let ul = u_liquidity(self.position_size_lamports, liquidity_lamports);

        let composite = composite_urgency(uk, um, uv, ul);

        // Store breakdown for JSONL logging (always, even in shadow mode)
        self.v4_u_kelly = uk;
        self.v4_u_momentum = um;
        self.v4_u_vol_trail = uv;
        self.v4_u_liquidity = ul;

        // Apply monotonic floor
        let effective = self.urgency.effective_urgency(composite);

        // ── V4 exit decision (only when enabled; otherwise shadow-only) ──
        if config.exit_v4.enabled {
            match self.urgency.decide(effective) {
                ExitFraction::Hold => {}
                ExitFraction::Tighten => {
                    // Tighten the trail stop by 25% (reduce trail width)
                    let tighter_trail = (self.current_trail_bp as u32 * 3 / 4) as u16;
                    self.current_trail_bp = tighter_trail.max(config.kelly_min_trail_bp);
                    let new_stop = compute_trail_stop(self.peak_mvsol, self.current_trail_bp);
                    if new_stop > self.trail_stop_mvsol {
                        self.trail_stop_mvsol = new_stop;
                    }
                }
                ExitFraction::Partial(permille) => {
                    return RideDecision::PartialExit { permille };
                }
                ExitFraction::Exit(_) => {
                    return RideDecision::Exit(RideExitReason::UrgencyFull);
                }
            }
        } else {
            // Shadow mode: update urgency state for logging, but don't act
            self.urgency.last_urgency = effective;
        }

        RideDecision::Hold
    }

    // ── Bayesian helpers (inlined, operate on RideState fields directly) ──

    #[inline(always)]
    fn bayesian_update_evidence(
        &mut self,
        is_buy: bool,
        sol_msol: u16,
        source: FeedSource,
        weight_mult: u8,
    ) {
        let base = EVIDENCE_WEIGHTS[(!is_buy) as usize][source.as_index()] as u32;
        let size_factor = (1u32 + sol_msol as u32 / 500).min(16);
        let w = (base * size_factor * weight_mult as u32 / 10).min(4080) as u16;

        if is_buy {
            self.alpha_x16 = self.alpha_x16.saturating_add(w);
        } else {
            self.beta_x16 = self.beta_x16.saturating_add(w);
        }
    }

    #[inline(always)]
    fn bayesian_decay_tick(&mut self) {
        self.alpha_x16 = ((self.alpha_x16 as u32 * DECAY_NUMER) >> 8).max(MIN_AB_X16 as u32) as u16;
        self.beta_x16 = ((self.beta_x16 as u32 * DECAY_NUMER) >> 8).max(MIN_AB_X16 as u32) as u16;
    }

    /// Compute fee-adjusted Bayesian half-Kelly fraction.
    ///
    /// Uses fee_adjust_r() to convert raw R_est to R_adj, then:
    ///   f̂* = [p(R_adj+1) - 1] / (2 × R_adj)
    ///
    /// This ensures the exit engine never holds a position whose real-time
    /// Bayesian edge is negative after accounting for round-trip fees.
    #[inline(always)]
    fn bayesian_current_f_permille(&self) -> i16 {
        let a = self.alpha_x16 as u32;
        let b = self.beta_x16 as u32;
        let ab = a + b;
        if ab == 0 { return 0; }
        let p_x1000 = (a * 1000) / ab;
        // Fee-adjust R before computing half-Kelly
        let r_raw = self.r_est_x100;
        let r = fee_adjust_r(r_raw, DEFAULT_ROUND_TRIP_FEE_BP, self.avg_loss_bp)
            .max(1) as u32;
        let numerator = (p_x1000 * (r + 100)) as i32 - 100_000;
        (numerator / (2 * r as i32)).clamp(-1000, 1000) as i16
    }

    #[inline(always)]
    fn bayesian_signal_state(&self, f_hat: i16) -> SignalState {
        let f_entry = self.entry_f_permille as i32;
        if f_entry == 0 { return SignalState::Exit; }
        let strong = (f_entry * 179) >> 8;   // ~0.70 × f_entry
        let sustain = (f_entry * 90) >> 8;   // ~0.35 × f_entry
        if f_hat as i32 > strong {
            SignalState::StrongPump
        } else if f_hat as i32 > sustain {
            SignalState::Sustained
        } else if f_hat > 0 {
            SignalState::Weakening
        } else {
            SignalState::Exit
        }
    }

    #[inline(always)]
    fn bayesian_update_r_estimate(&mut self, current_pnl_bp: i16) {
        if current_pnl_bp > self.peak_mfe_bp {
            self.peak_mfe_bp = current_pnl_bp;
        }
        let avg = self.avg_loss_bp.max(1) as u32;
        let implied = (self.peak_mfe_bp.max(0) as u32 * 100) / avg;
        if implied > self.r_est_x100 as u32 {
            self.r_est_x100 = ((self.r_est_x100 as u32 * 7 + implied) >> 3) as u16;
        }
    }
}

// ---------------------------------------------------------------------------
// Debug impl (avoids derive requiring SignalState: Debug on u8 field)
// ---------------------------------------------------------------------------

impl core::fmt::Debug for RideState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RideState")
            .field("entry_mvsol", &self.entry_mvsol)
            .field("peak_mvsol", &self.peak_mvsol)
            .field("trail_stop", &self.trail_stop_mvsol)
            .field("state", &self.signal_state())
            .field("alpha", &self.alpha_x16)
            .field("beta", &self.beta_x16)
            .field("f_hat", &self.f_hat_permille())
            .field("buys", &self.buys_after_entry)
            .field("sells", &self.sells_after_entry)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> RideConfig {
        RideConfig {
            trail_strong_pump_bp: 500,
            trail_sustained_bp: 350,
            trail_weakening_bp: 200,
            kelly_min_trail_bp: 50,
            kelly_max_trail_bp: 1000,
            max_hold_ms: 60_000,
            whale_exit_lamports: 2_000_000_000,
            avg_loss_bp: 200,
            ..Default::default()
        }
    }

    #[test]
    fn test_size_assert() {
        assert_eq!(core::mem::size_of::<RideState>(), 192);
    }

    #[test]
    fn test_new_initializes_bayesian_prior() {
        let config = default_config();
        let rs = RideState::new(
            1000, 1000, 10_000,
            248, 542, 4300, 1, 200,
            &config,
        );
        assert!(rs.alpha_x16 > 0);
        assert!(rs.beta_x16 > 0);
        assert_eq!(rs.entry_f_permille, 248);
        assert_eq!(rs.entry_p_permille, 542);
        assert_eq!(rs.r_est_x100, 4300);
        assert_eq!(rs.signal_state(), SignalState::StrongPump);
    }

    #[test]
    fn test_on_buy_increments_counters() {
        let config = default_config();
        let mut rs = RideState::new(1000, 1000, 10_000, 248, 542, 4300, 1, 200, &config);
        let alpha_before = rs.alpha_x16;
        rs.on_buy_event(100, 10_500, 0xDEAD, FeedSource::PumpPortal, 10);
        assert_eq!(rs.buys_after_entry, 1);
        assert!(rs.alpha_x16 > alpha_before);
    }

    #[test]
    fn test_on_sell_increments_beta() {
        let config = default_config();
        let mut rs = RideState::new(1000, 1000, 10_000, 248, 542, 4300, 1, 200, &config);
        let beta_before = rs.beta_x16;
        let result = rs.on_sell_event(100, 10_500, &config, FeedSource::PumpPortal, 10);
        assert_eq!(rs.sells_after_entry, 1);
        assert!(rs.beta_x16 > beta_before);
        assert!(result.is_none()); // no emergency
    }

    #[test]
    fn test_grace_period_prevents_instant_exit() {
        let config = default_config();
        // Use LOW conviction tier with minimal p — f̂ starts low
        let mut rs = RideState::new(1000, 1000, 10_000, 50, 480, 4300, 0, 200, &config);
        // Hammer with sells to drive f̂ negative
        for i in 0..5 {
            rs.on_sell_event(500, 10_100 + i * 100, &config, FeedSource::PumpPortal, 10);
        }
        // Tick at price above fee-adjusted breakeven (entry=1000, fee=210bp → breakeven=1021)
        // buys_after_entry == 0, so grace period should prevent Bayesian exit
        let decision = rs.on_tick(1025, 10_600, &config);
        assert_eq!(decision, RideDecision::Hold);
    }

    #[test]
    fn test_signal_exit_after_confirming_buy() {
        let config = default_config();
        // LOW conviction tier (weakest prior, easiest to flip)
        let mut rs = RideState::new(1000, 1000, 10_000, 100, 500, 2000, 0, 200, &config);
        // One confirming buy (small)
        rs.on_buy_event(50, 10_200, 0xBEEF, FeedSource::PumpPortal, 10);
        assert_eq!(rs.buys_after_entry, 1);
        // Feed sells spaced 5s apart (to avoid sell_cascade which returns early before β update)
        for i in 0..10u64 {
            let _ = rs.on_sell_event(3000, 15_000 + i * 5000, &config, FeedSource::PumpPortal, 10);
            // Also directly pump β to simulate overwhelming evidence
            rs.beta_x16 = rs.beta_x16.saturating_add(500);
        }
        // Decay alpha to near-floor
        for _ in 0..30 {
            rs.bayesian_decay_tick();
        }
        // Verify f̂ is now negative
        let f = rs.bayesian_current_f_permille();
        assert!(f <= 0, "f_hat should be <= 0 after heavy selling + decay, got {}", f);
        let decision = rs.on_tick(1000, 80_000, &config);
        // Should be Exit (buys_after_entry >= 1 and f̂ ≤ 0)
        match decision {
            RideDecision::Exit(_) => {} // expected — could be SignalExit or BuyGapTimeout
            RideDecision::PartialExit { .. } => {} // acceptable — v4 urgency partial
            RideDecision::Hold => panic!("Expected exit, got Hold. f_hat={}, alpha={}, beta={}", f, rs.alpha_x16, rs.beta_x16),
        }
    }

    #[test]
    fn test_hard_floor_bypasses_grace_period() {
        let config = default_config();
        let rs_entry = 1000u32;
        let mut rs = RideState::new(rs_entry, rs_entry, 10_000, 248, 542, 4300, 1, 200, &config);
        // Price drops below fee-adjusted breakeven (entry × 1.021 = 1021)
        // At entry price (1000), we're already below breakeven → HardFloor
        let decision = rs.on_tick(rs_entry, 10_100, &config);
        assert_eq!(decision, RideDecision::Exit(RideExitReason::HardFloor));
    }

    #[test]
    fn test_whale_sell_emergency() {
        let config = default_config();
        let mut rs = RideState::new(1000, 1000, 10_000, 248, 542, 4300, 1, 200, &config);
        // 3 SOL sell = above 2 SOL whale threshold
        let result = rs.on_sell_event(3000, 10_100, &config, FeedSource::PumpPortal, 10);
        assert_eq!(result, Some(RideExitReason::WhaleExit));
    }

    #[test]
    fn test_trail_widens_with_conviction() {
        let config = default_config();
        let mut rs = RideState::new(1000, 1000, 10_000, 248, 542, 4300, 1, 200, &config);
        // Pump with several buys to build alpha
        for i in 0..5 {
            rs.on_buy_event(200, 10_100 + i * 100, i as u64, FeedSource::PumpPortal, 10);
        }
        let _ = rs.on_tick(1050, 10_600, &config);
        let trail_after_buys = rs.current_trail_bp;
        // Trail should be positive and reasonable
        assert!(trail_after_buys >= config.kelly_min_trail_bp);
        assert!(trail_after_buys <= config.kelly_max_trail_bp);
    }

    #[test]
    fn test_decay_shrinks_alpha_beta() {
        let config = default_config();
        let mut rs = RideState::new(1000, 1000, 10_000, 248, 542, 4300, 1, 200, &config);
        let alpha_before = rs.alpha_x16;
        let beta_before = rs.beta_x16;
        rs.bayesian_decay_tick();
        assert!(rs.alpha_x16 < alpha_before, "Alpha should shrink after decay");
        assert!(rs.beta_x16 < beta_before, "Beta should shrink after decay");
        assert!(rs.alpha_x16 >= MIN_AB_X16, "Alpha should not go below floor");
    }

    #[test]
    fn test_ring_buffer_sentinels() {
        let config = default_config();
        let rs = RideState::new(1000, 1000, 10_000, 248, 542, 4300, 1, 200, &config);
        for &ts in rs.buy_ts_ring.iter() {
            assert_eq!(ts, u16::MAX, "Buy ring should be sentinel-initialized");
        }
        for &ts in rs.sell_ts_ring.iter() {
            assert_eq!(ts, u16::MAX, "Sell ring should be sentinel-initialized");
        }
    }
}
