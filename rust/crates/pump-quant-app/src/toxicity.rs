//! VPIN-X — exact-sign order-flow **toxicity gate** (§21.7 burst/toxicity family).
//!
//! Canonical VPIN (Easley–López de Prado–O'Hara, RFS 2012) measures flow toxicity
//! as the mean absolute buy/sell imbalance per equal-volume bucket. In crypto it is
//! the **#2 short-horizon predictor** (Easley et al., SSRN 4814346; crypto baseline
//! toxicity 0.45–0.47 vs 0.22 in equities). This engine has the luxury the
//! literature lacks: the decoded swap stream carries **exact** trade signs, so the
//! error-prone BVC signing step (the part Andersen–Bondarenko discredited)
//! disappears and the measured object is the true signed imbalance.
//!
//! Memecoin adaptations (research doc "CONNECTIVITY LEDGER" / VPIN spec):
//! * **Volume-clocked buckets sized from the mint's own rolling volume** (the
//!   scalper-scale transplant of ELO's "1/50 of daily volume"), floored/capped in
//!   lamports so dust spam can't manufacture buckets and one whale can't pin the
//!   ring. Bucket cap is frozen at open; trades split across bucket boundaries.
//! * **Sell-share decomposition.** Memecoin launch waves are legitimately near-100%
//!   buy, so raw VPIN saturates on healthy tapes; only **sell-dominant** toxicity
//!   escalates to a veto/exit. Buy-dominant saturation is mild caution only.
//! * **Fail-safe absence.** VPIN below `min_buckets` completed buckets, or stale
//!   past `stale_ticks`, is ABSENT: identity multiplier, no veto, no relief. The
//!   gate can only ever *reduce* size — never add.
//!
//! Deterministic, integer, tick-clocked, bounded (§22/§99): fixed 16-bucket ring +
//! a 30-cell tick-volume ring per tracked mint; O(1) per swap amortized.

/// Completed buckets retained (the VPIN window). ELO's n=50 spans a day; 16
/// buckets of ~1/16 of the rolling volume window each span the scalping horizon.
pub const VPIN_N_BUCKETS: usize = 16;

/// Tick-volume ring geometry: 30 cells × 10 ticks = a 300-tick rolling volume
/// window (the same scale as the position max-hold), from which bucket caps derive.
const TICKVOL_CELLS: usize = 30;
const TICKS_PER_CELL: u64 = 10;

/// Named tuning for the toxicity gate (§102). All operator-overridable via config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VpinParams {
    /// Bucket-cap floor, lamports (≈ one retail clip; dust spam can't fill buckets).
    pub v_min_lamports: u64,
    /// Bucket-cap ceiling, lamports (one bucket ≤ ~25% of a full curve's SOL).
    pub v_max_lamports: u64,
    /// Completed buckets required before VPIN is trusted.
    pub min_buckets: usize,
    /// Ticks without a completed bucket after which VPIN is stale (absent).
    pub stale_ticks: u64,
}

/// One completed equal-volume bucket.
#[derive(Clone, Copy, Debug, Default)]
struct Bucket {
    v_buy: u64,
    v_sell: u64,
    cap: u64,
}

/// Per-mint VPIN accumulator: a fixed ring of completed buckets, the filling
/// bucket, and the rolling tick-volume window that sizes the next cap.
#[derive(Clone, Debug)]
pub struct VpinState {
    ring: [Bucket; VPIN_N_BUCKETS],
    len: usize,
    head: usize,
    cur: Bucket,
    tickvol: [u64; TICKVOL_CELLS],
    tickvol_cell_tick: u64,
    last_complete_tick: u64,
    has_completed: bool,
}

impl VpinState {
    /// A fresh accumulator. The first bucket cap is the floor (no volume history).
    #[must_use]
    pub fn new(p: &VpinParams) -> Self {
        Self {
            ring: [Bucket::default(); VPIN_N_BUCKETS],
            len: 0,
            head: 0,
            cur: Bucket {
                v_buy: 0,
                v_sell: 0,
                cap: p.v_min_lamports.max(1),
            },
            tickvol: [0; TICKVOL_CELLS],
            tickvol_cell_tick: 0,
            last_complete_tick: 0,
            has_completed: false,
        }
    }

    /// Advance the tick-volume ring to `tick`, zeroing skipped cells.
    fn advance_tickvol(&mut self, tick: u64) {
        let cell_tick = tick / TICKS_PER_CELL;
        if cell_tick > self.tickvol_cell_tick {
            let skipped = (cell_tick - self.tickvol_cell_tick).min(TICKVOL_CELLS as u64);
            for i in 1..=skipped {
                let idx = ((self.tickvol_cell_tick + i) % TICKVOL_CELLS as u64) as usize;
                self.tickvol[idx] = 0;
            }
            self.tickvol_cell_tick = cell_tick;
        }
    }

    /// Fold one decoded swap (`quote_lamports` on `buy`/sell side at `tick`).
    /// Trades split across bucket boundaries — one whale print may complete
    /// several buckets, which is volume-time working as intended.
    pub fn on_trade(&mut self, buy: bool, quote_lamports: u64, tick: u64, p: &VpinParams) {
        self.advance_tickvol(tick);
        let cell = (self.tickvol_cell_tick % TICKVOL_CELLS as u64) as usize;
        self.tickvol[cell] = self.tickvol[cell].saturating_add(quote_lamports);

        let mut q = quote_lamports;
        while q > 0 {
            let filled = self.cur.v_buy.saturating_add(self.cur.v_sell);
            let room = self.cur.cap.saturating_sub(filled);
            let take = q.min(room);
            if buy {
                self.cur.v_buy = self.cur.v_buy.saturating_add(take);
            } else {
                self.cur.v_sell = self.cur.v_sell.saturating_add(take);
            }
            q -= take;
            if take == room {
                self.complete_bucket(tick, p);
            }
            if take == 0 {
                // Defensive: a zero-room bucket (cap consumed exactly) completes above;
                // this branch is unreachable, but never loop on it.
                break;
            }
        }
    }

    /// Seal the filling bucket into the ring and open the next with a cap derived
    /// from the rolling volume window (frozen at open; floored/capped).
    fn complete_bucket(&mut self, tick: u64, p: &VpinParams) {
        let idx = (self.head + self.len) % VPIN_N_BUCKETS;
        if self.len == VPIN_N_BUCKETS {
            // Evict oldest.
            self.ring[self.head] = self.cur;
            self.head = (self.head + 1) % VPIN_N_BUCKETS;
        } else {
            self.ring[idx] = self.cur;
            self.len += 1;
        }
        self.last_complete_tick = tick;
        self.has_completed = true;
        let window_vol: u128 = self.tickvol.iter().map(|&v| u128::from(v)).sum();
        let cap = (window_vol / VPIN_N_BUCKETS as u128).min(u128::from(u64::MAX)) as u64;
        self.cur = Bucket {
            v_buy: 0,
            v_sell: 0,
            cap: cap.clamp(p.v_min_lamports.max(1), p.v_max_lamports.max(1)),
        };
    }

    /// The current toxicity reading: `Some((vpin_bp, sell_share_bp))` when valid
    /// (≥ `min_buckets` completed, not stale at `now_tick`), else `None` (absent).
    ///
    /// `vpin_bp = ceil(10_000 × Σ|v_sell−v_buy| / Σcap)` — ceiling division so
    /// truncation never under-states toxicity. `sell_share_bp` is the sell fraction
    /// of completed-bucket volume (the memecoin decomposition: only sell-dominant
    /// toxicity may veto).
    #[must_use]
    pub fn reading(&self, now_tick: u64, p: &VpinParams) -> Option<(u32, u32)> {
        if self.len < p.min_buckets.max(1)
            || !self.has_completed
            || now_tick.saturating_sub(self.last_complete_tick) > p.stale_ticks
        {
            return None;
        }
        let mut sum_abs: u128 = 0;
        let mut sum_cap: u128 = 0;
        let mut sum_buy: u128 = 0;
        let mut sum_sell: u128 = 0;
        for i in 0..self.len {
            let b = &self.ring[(self.head + i) % VPIN_N_BUCKETS];
            let (vb, vs) = (u128::from(b.v_buy), u128::from(b.v_sell));
            sum_abs += vb.abs_diff(vs);
            sum_cap += u128::from(b.cap);
            sum_buy += vb;
            sum_sell += vs;
        }
        if sum_cap == 0 {
            return None;
        }
        let vpin_bp = (sum_abs.saturating_mul(10_000).div_ceil(sum_cap)).min(10_000) as u32;
        let sell_share_bp = sum_sell
            .saturating_mul(10_000)
            .checked_div(sum_buy + sum_sell)
            .unwrap_or(0) as u32;
        Some((vpin_bp, sell_share_bp))
    }
}

/// Toxicity thresholds (§102 named; config-fed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VpinThresholds {
    /// Haircut onset (sell-dominant only).
    pub warn_bp: u32,
    /// Deep-haircut tier.
    pub toxic_bp: u32,
    /// Extreme tier: veto (with sell dominance) / exit escalation.
    pub veto_bp: u32,
    /// Sell-share above which the extreme tier vetoes / escalates.
    pub sell_dom_bp: u32,
}

/// The graded entry-size multiplier (bps of 10_000, ≤ 10_000 always) from a VPIN
/// reading. `None` reading ⇒ identity (absence grants no relief and no penalty).
/// A distributed dump in progress (extreme + sell-dominant) returns 0 — the one
/// narrow binary veto tier the evidence supports.
#[must_use]
pub fn vpin_size_mult_bp(reading: Option<(u32, u32)>, t: &VpinThresholds) -> u32 {
    let Some((vpin, sell)) = reading else {
        return 10_000;
    };
    let sell_dom = sell >= t.sell_dom_bp;
    let sell_lean = sell >= 5_000;
    if vpin >= t.veto_bp {
        if sell_dom {
            0 // veto: distributed dump in progress
        } else {
            7_000 // buy-side blow-off: caution, never veto (launch waves are ~all-buy)
        }
    } else if vpin >= t.toxic_bp {
        if sell_lean {
            5_000
        } else {
            8_500
        }
    } else if vpin >= t.warn_bp {
        if sell_lean {
            8_000
        } else {
            10_000
        }
    } else {
        10_000
    }
}

/// Whether a held position should be force-exited on toxicity: the extreme
/// sell-dominant tier — a distributed multi-swap dump that the single-print
/// rug-precursor cannot see.
#[must_use]
pub fn vpin_exit_escalates(reading: Option<(u32, u32)>, t: &VpinThresholds) -> bool {
    matches!(reading, Some((v, s)) if v >= t.veto_bp && s >= t.sell_dom_bp)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed bucket caps (floor == ceiling) so bucket counts are exact in tests.
    const P: VpinParams = VpinParams {
        v_min_lamports: 1_000,
        v_max_lamports: 1_000,
        min_buckets: 8,
        stale_ticks: 150,
    };
    const T: VpinThresholds = VpinThresholds {
        warn_bp: 6_500,
        toxic_bp: 8_000,
        veto_bp: 9_000,
        sell_dom_bp: 6_000,
    };

    #[test]
    fn sparse_tape_is_absent_and_identity() {
        let mut v = VpinState::new(&P);
        v.on_trade(true, 500, 1, &P); // half a bucket — nothing completes
        assert_eq!(v.reading(1, &P), None);
        assert_eq!(vpin_size_mult_bp(None, &T), 10_000, "absence = identity");
    }

    #[test]
    fn balanced_flow_reads_low_and_sell_dump_reads_toxic() {
        let mut v = VpinState::new(&P);
        // Alternating balanced buckets: 8 × (500 buy + 500 sell) = low VPIN.
        for i in 0..8u64 {
            v.on_trade(true, 500, i, &P);
            v.on_trade(false, 500, i, &P);
        }
        let (vpin, sell) = v.reading(8, &P).expect("8 buckets completed");
        assert!(vpin <= 2_000, "balanced flow is low-toxicity (got {vpin})");
        assert_eq!(sell, 5_000, "balanced sell share");

        // A distributed dump: one whale sell completes a full ring of all-sell
        // buckets (16 × 1_000-cap), displacing the balanced history entirely.
        v.on_trade(false, 16_000, 9, &P);
        let (vpin2, sell2) = v.reading(9, &P).unwrap();
        assert!(vpin2 >= T.veto_bp, "dump saturates VPIN (got {vpin2})");
        assert!(sell2 >= T.sell_dom_bp, "sell-dominant (got {sell2})");
        assert_eq!(vpin_size_mult_bp(Some((vpin2, sell2)), &T), 0, "veto tier");
        assert!(vpin_exit_escalates(Some((vpin2, sell2)), &T));
    }

    #[test]
    fn buy_wave_never_vetoes() {
        let mut v = VpinState::new(&P);
        v.on_trade(true, 16_000, 1, &P); // an all-buy launch wave: 16 full buckets
        let r = v.reading(1, &P).unwrap();
        assert!(r.0 >= T.veto_bp, "all-buy saturates VPIN");
        assert!(r.1 < T.sell_dom_bp);
        let m = vpin_size_mult_bp(Some(r), &T);
        assert!(
            m > 0,
            "buy-dominant saturation is caution ({m}bp), never a veto"
        );
        assert!(!vpin_exit_escalates(Some(r), &T));
    }

    #[test]
    fn staleness_makes_reading_absent() {
        let mut v = VpinState::new(&P);
        v.on_trade(true, 16_000, 1, &P);
        assert!(v.reading(1, &P).is_some());
        assert_eq!(v.reading(1 + P.stale_ticks + 1, &P), None, "stale ⇒ absent");
    }

    #[test]
    fn deterministic_same_stream_same_reading() {
        let run = || {
            let mut v = VpinState::new(&P);
            for (i, (b, q)) in [(true, 700u64), (false, 300), (true, 2_000), (false, 1_500)]
                .iter()
                .enumerate()
            {
                v.on_trade(*b, *q, i as u64, &P);
            }
            v.reading(4, &P)
        };
        assert_eq!(run(), run());
    }
}
