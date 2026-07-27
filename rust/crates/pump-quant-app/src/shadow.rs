//! The §48 exit-policy shadow tournament — pre-registered challengers racing
//! the incumbent on the SAME admits, no capital, report-only verdicts.
//!
//! # Design (research-fixed)
//! K = 8 challengers form a 2×2×2 factorial over the three exit levers the §48
//! research found decisive: trailing width (tight/wide), principal-recovery
//! arm (early/late TP1) and conditional hold (short/long). Every other
//! parameter is inherited from the incumbent's [`LifecycleParams`], so each
//! challenger differs from the live policy in exactly the pre-registered
//! dimensions — no post-hoc parameter fishing (§56.1).
//!
//! Each challenger runs a FULL [`ScalpLifecycle`] in shadow: it is opened on
//! the same admits, fed the same swaps and ticks, and books the same integer
//! fill arithmetic as the live position — but no capital moves and nothing is
//! journaled as a decision (§54: shadow books are research, not authority).
//!
//! # Scoring — paired SPRT in milli-nats
//! When the LIVE position for a mint closes, each challenger's outcome for the
//! same mint-entry is paired against it: `d = challenger_net − incumbent_net`.
//! A challenger whose shadow position is still open at that moment is
//! force-closed at the latest price — that IS its policy outcome: a slower
//! exit is marked at live-close time, not granted free extra runway.
//!
//! The pair stream drives a per-challenger SPRT of H1 "the challenger wins 60%
//! of pairs" against H0 "coin flip": win `+182` milli-nats (`ln 1.2`), loss
//! `−223` (`ln 0.8`), tie `0`. Boundaries at α = β = 5%: lower `ln(0.05/0.95)
//! = −2944` drops the challenger; upper `ln(0.95/0.05) + ln 8 = +5023` — the
//! `ln 8` term is the Bonferroni deflation for running 8 pre-registered
//! challengers at once — flags it ADOPTABLE. No flag before
//! [`PAIRS_MIN`] pairs; at [`PAIRS_TRUNCATE`] pairs without a decision the
//! ledger resets (params kept) so a stale race cannot squat forever.
//!
//! Two economic guards must ALSO hold before ADOPTABLE (a statistically
//! significant but immaterial or riskier challenger stays Racing):
//! * **materiality** — `10·Σd ≥ n·|median incumbent net|`: the mean pair edge
//!   must be at least 10% of the incumbent's typical trade;
//! * **drawdown veto** — the challenger's shadow-equity max drawdown may not
//!   exceed the incumbent's by more than [`DD_ALLOWANCE_BPS`] of the mean
//!   deployed size (net-SOL expectancy under survival constraints, §48 — a
//!   challenger that wins by riding deeper troughs is not an upgrade).
//!
//! **Adoption is report-only** (§56.2 envelope): [`ExitTournament::standings`]
//! exposes the verdict; the engine/operator flips config. Nothing here ever
//! self-adopts.
//!
//! # Discipline (binding)
//! Deterministic, integer-only, tick-clocked, no RNG (§22). Bounded (§99):
//! 8 fixed challengers, each book capped at the live concurrency, pair
//! ledgers and mark caches capped by named constants with oldest-first
//! eviction. Runs off the hot path — per-swap work is 8 bounded map probes.

use crate::position::{ExitReason, LifecycleParams, ScalpLifecycle};
use std::collections::{BTreeMap, VecDeque};

// ============================================================================
// Named constants (§102 — each with its rationale).
// ============================================================================

/// Pre-registered challengers: the full 2×2×2 factorial over the three legacy
/// exit levers (indices 0..8) PLUS two Batch-2a single-axis challengers appended
/// at 8 and 9 — the §24(d) exit-into-strength arm and the §24 volatility-scaled
/// stop arm. Each appended challenger inherits every incumbent parameter and
/// toggles ONLY its one new axis, so its pair edge attributes cleanly to that law
/// (§56.1, no post-hoc fishing).
const K_CHALLENGERS: usize = 10;
/// Size of the legacy 2×2×2 factorial block (challengers 0..8).
const K_FACTORIAL: usize = 8;
/// Challenger index of the §24(d) exit-into-strength arm.
const IDX_INTO_STRENGTH: usize = 8;
/// Challenger index of the §24 volatility-scaled-stop arm.
const IDX_VOL_STOP: usize = 9;
/// The §24(d) challenger's climax-strength threshold (bps of 10_000 over baseline
/// arrival): 2× baseline = a genuine plateaued climax, not routine flow.
const INTO_STRENGTH_CLIMAX_BP: u32 = 20_000;
/// The §24 vol-stop challenger's scale: 0.5× of the realized-vol bps added to the
/// base stop/trail width, inside the envelope.
const VOL_STOP_SCALE_BP: u32 = 5_000;

/// Tight trailing width, bps: harvest earlier, give back less off the peak.
const TRAIL_TIGHT_BPS: u32 = 1_500;
/// Wide trailing width, bps: let winners breathe at the cost of give-back.
const TRAIL_WIDE_BPS: u32 = 3_000;
/// Early principal-recovery arm, mult bps (1.25×): de-risk sooner.
const TP1_ARM_EARLY_BPS: u32 = 12_500;
/// Late principal-recovery arm, mult bps (1.5×): more room before the trim.
const TP1_ARM_LATE_BPS: u32 = 15_000;
/// Short conditional hold, ticks: cut stalls fast.
const HOLD_SHORT_TICKS: u64 = 180;
/// Long conditional hold, ticks: give slow grinds time to resolve.
const HOLD_LONG_TICKS: u64 = 420;

/// Shadow-book concurrency cap per challenger — mirrors the engine's live
/// scalp-book cap so a challenger can never take a trade the incumbent's
/// concurrency would have refused (§99, same-admit pairing).
const SHADOW_CONCURRENCY_CAP: usize = 64;

/// SPRT win increment, milli-nats: `ln(0.6/0.5) = +0.18232` — the
/// log-likelihood a pair win contributes under H1 p=0.6 vs H0 p=0.5.
const SPRT_WIN_MILLINATS: i64 = 182;
/// SPRT loss increment, milli-nats: `ln(0.4/0.5) = −0.22314`.
const SPRT_LOSS_MILLINATS: i64 = -223;
/// SPRT lower boundary, milli-nats: `ln(β/(1−α))` at α = β = 0.05 — cross it
/// and the challenger is dropped.
const SPRT_LOWER_MILLINATS: i64 = -2_944;
/// SPRT upper boundary, milli-nats: `ln((1−β)/α) = +2944` PLUS `ln 8 = +2079`
/// Bonferroni deflation for the 8 simultaneous challengers.
const SPRT_UPPER_MILLINATS: i64 = 5_023;
/// Minimum pairs before ANY flag may bind (learning horizon, §56.11 spirit).
const PAIRS_MIN: u32 = 50;
/// Pair-count truncation: at 400 undecided pairs the SPRT ledger resets
/// (params kept) — an SPRT that has not decided by then is running on a
/// near-zero effect and must restart rather than drift.
const PAIRS_TRUNCATE: u32 = 400;

/// Materiality multiplier: `10·Σd ≥ n·|median incumbent net|` ⇔ the mean pair
/// edge is at least 10% of the incumbent's typical per-trade net.
const MATERIALITY_NUM: i128 = 10;

/// Drawdown allowance, bps of the mean deployed size, added to the incumbent's
/// max drawdown when vetoing a challenger on survival grounds.
const DD_ALLOWANCE_BPS: i128 = 2_000;

/// Cap on closed-but-unpaired challenger outcomes per challenger (§99). Live
/// closes normally drain these immediately; the cap only matters if the
/// engine never reports a live close for a mint. Oldest evicted.
const PENDING_CLOSED_CAP: usize = 256;

/// Cap on the last-seen price cache used for force-close marks (§99).
/// Entries are removed when their mint's pair completes; the cap bounds the
/// pathological case. Oldest evicted; an evicted mint force-closes at a zero
/// mark (conservative against the challenger, never in its favor).
const LAST_PRICE_CAP: usize = 512;

/// Incumbent per-trade net ring for the materiality median (§99). Sized to
/// the SPRT truncation window — the guard sees the same history the test does.
const INCUMBENT_NET_CAP: usize = 400;

// ============================================================================
// Public verdict types.
// ============================================================================

/// A challenger's tournament verdict. `Dropped` and `Adoptable` are absorbing:
/// an operator-facing flag that flip-flopped would be unactionable (§56.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TournamentVerdict {
    /// Still racing: no boundary crossed (or a boundary crossed but an
    /// economic guard withheld the flag).
    Racing,
    /// SPRT concluded the challenger is not better — its shadow book is
    /// closed and it stops taking admits.
    Dropped,
    /// SPRT + materiality + drawdown all passed: worth adopting. REPORT-ONLY —
    /// the operator flips config through the §56.2 envelope.
    Adoptable,
}

/// One row of [`ExitTournament::standings`]: the challenger's pre-registered
/// parameter summary and its current SPRT state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChallengerStanding {
    /// Challenger index (0..8, the factorial encoding).
    pub idx: u8,
    /// Its trailing base width, bps.
    pub trail_base_bps: u32,
    /// Its TP1 principal-recovery arm, mult bps.
    pub tp1_bps: u32,
    /// Its conditional max hold, ticks.
    pub max_hold_ticks: u64,
    /// Scored pairs in the current SPRT ledger.
    pub pairs: u32,
    /// Accumulated SPRT log-likelihood ratio, milli-nats.
    pub sprt_millinats: i64,
    /// Current verdict.
    pub verdict: TournamentVerdict,
}

// ============================================================================
// Internal challenger state.
// ============================================================================

/// One challenger: its params, its full shadow lifecycle, and its pair ledger.
#[derive(Debug)]
struct Challenger {
    /// The pre-registered parameter set (incumbent + factorial overrides).
    params: LifecycleParams,
    /// The full shadow exit lifecycle (no capital, no journal).
    book: ScalpLifecycle,
    /// Partial-tranche net accumulated per still-open mint.
    partial: BTreeMap<[u8; 32], i128>,
    /// Closed-but-unpaired nets: mint → (net, insertion seq for eviction).
    pending: BTreeMap<[u8; 32], (i128, u64)>,
    /// Pairs scored in the current SPRT ledger.
    pairs: u32,
    /// Accumulated SPRT LLR, milli-nats.
    llr_millinats: i64,
    /// `Σ d` over the current ledger (materiality guard numerator).
    sum_d: i128,
    /// Current verdict.
    verdict: TournamentVerdict,
    /// Shadow equity: running sum of realized nets across closed trades.
    equity: i128,
    /// Running equity peak.
    equity_peak: i128,
    /// Max peak-to-trough equity drawdown, lamports (survival guard).
    max_drawdown: i128,
}

impl Challenger {
    /// Fold one closed shadow trade into the equity/drawdown track.
    fn fold_equity(&mut self, net: i128) {
        self.equity = self.equity.saturating_add(net);
        if self.equity > self.equity_peak {
            self.equity_peak = self.equity;
        }
        let dd = self.equity_peak.saturating_sub(self.equity);
        if dd > self.max_drawdown {
            self.max_drawdown = dd;
        }
    }

    /// Book a closed-but-unpaired outcome, evicting the oldest past the cap.
    fn push_pending(&mut self, mint: [u8; 32], net: i128, seq: u64) {
        if self.pending.len() >= PENDING_CLOSED_CAP && !self.pending.contains_key(&mint) {
            if let Some(oldest) = self
                .pending
                .iter()
                .min_by_key(|(_, (_, s))| *s)
                .map(|(m, _)| *m)
            {
                self.pending.remove(&oldest);
            }
        }
        self.pending.insert(mint, (net, seq));
    }

    /// Score one pair `d = challenger − incumbent` through the SPRT and its
    /// economic guards. `med_abs` is `|median incumbent net|`; `dd_limit` the
    /// drawdown ceiling. Only a Racing challenger scores; verdict transitions
    /// are absorbing.
    fn score_pair(&mut self, d: i128, med_abs: i128, dd_limit: i128) {
        if self.verdict != TournamentVerdict::Racing {
            return;
        }
        self.pairs = self.pairs.saturating_add(1);
        self.sum_d = self.sum_d.saturating_add(d);
        let step = match d.cmp(&0) {
            std::cmp::Ordering::Greater => SPRT_WIN_MILLINATS,
            std::cmp::Ordering::Less => SPRT_LOSS_MILLINATS,
            std::cmp::Ordering::Equal => 0,
        };
        self.llr_millinats = self.llr_millinats.saturating_add(step);

        if self.pairs >= PAIRS_MIN {
            if self.llr_millinats <= SPRT_LOWER_MILLINATS {
                self.verdict = TournamentVerdict::Dropped;
                return;
            }
            if self.llr_millinats >= SPRT_UPPER_MILLINATS {
                let material = self.sum_d.saturating_mul(MATERIALITY_NUM)
                    >= i128::from(self.pairs).saturating_mul(med_abs);
                let survives = self.max_drawdown <= dd_limit;
                if material && survives {
                    self.verdict = TournamentVerdict::Adoptable;
                    return;
                }
                // Boundary crossed but a guard withheld the flag: keep racing.
            }
        }
        if self.pairs >= PAIRS_TRUNCATE {
            // Truncation: reset the SPRT ledger, keep the params (and the
            // lifetime equity track — survival history is not amnestied).
            self.pairs = 0;
            self.llr_millinats = 0;
            self.sum_d = 0;
        }
    }
}

// ============================================================================
// The tournament.
// ============================================================================

/// The §48 exit-policy shadow tournament: 8 pre-registered challengers racing
/// the incumbent exit policy on the live admit/swap/tick stream. See the
/// module docs for the full design and scoring contract.
#[derive(Debug)]
pub struct ExitTournament {
    /// The 8 challengers (factorial index encoding: bit0 trail, bit1 TP1,
    /// bit2 hold).
    challengers: [Challenger; K_CHALLENGERS],
    /// Incumbent per-trade nets (materiality median), cap
    /// [`INCUMBENT_NET_CAP`].
    incumbent_nets: VecDeque<i128>,
    /// Incumbent shadow-equity track for the drawdown veto.
    inc_equity: i128,
    /// Incumbent equity peak.
    inc_equity_peak: i128,
    /// Incumbent max drawdown, lamports.
    inc_max_drawdown: i128,
    /// Total deployed size across opens (mean-size for the dd allowance).
    total_size_lamports: u128,
    /// Number of opens behind `total_size_lamports`.
    opens: u64,
    /// Last-seen price per mint (entry, then every swap) for force-close
    /// marks: mint → (price_fp, seq). Cap [`LAST_PRICE_CAP`].
    last_price: BTreeMap<[u8; 32], (u64, u64)>,
    /// Monotonic sequence for deterministic oldest-first evictions.
    seq: u64,
}

impl ExitTournament {
    /// Build the tournament: 8 challengers derived from `incumbent` by the
    /// pre-registered 2×2×2 factorial (trail tight/wide × TP1 early/late ×
    /// hold short/long); every other parameter is inherited unchanged.
    #[must_use]
    pub fn new(incumbent: LifecycleParams) -> Self {
        let challengers = std::array::from_fn(|i| {
            let mut params = incumbent;
            if i < K_FACTORIAL {
                // Legacy 2×2×2 factorial over trail width × TP1 arm × hold length.
                params.trail_base_bps = if i & 0b001 == 0 {
                    TRAIL_TIGHT_BPS
                } else {
                    TRAIL_WIDE_BPS
                };
                params.tp1_bps = if i & 0b010 == 0 {
                    TP1_ARM_EARLY_BPS
                } else {
                    TP1_ARM_LATE_BPS
                };
                params.max_hold_ticks = if i & 0b100 == 0 {
                    HOLD_SHORT_TICKS
                } else {
                    HOLD_LONG_TICKS
                };
            } else if i == IDX_INTO_STRENGTH {
                // §24(d) exit-into-strength arm: incumbent params + the new axis.
                params.into_strength_exit_enable = true;
                params.into_strength_climax_bp = INTO_STRENGTH_CLIMAX_BP;
            } else if i == IDX_VOL_STOP {
                // §24 volatility-scaled-stop arm: incumbent params + the new axis.
                params.vol_stop_enable = true;
                params.vol_stop_scale_bp = VOL_STOP_SCALE_BP;
            }
            Challenger {
                params,
                book: ScalpLifecycle::new(params, SHADOW_CONCURRENCY_CAP),
                partial: BTreeMap::new(),
                pending: BTreeMap::new(),
                pairs: 0,
                llr_millinats: 0,
                sum_d: 0,
                verdict: TournamentVerdict::Racing,
                equity: 0,
                equity_peak: 0,
                max_drawdown: 0,
            }
        });
        Self {
            challengers,
            incumbent_nets: VecDeque::with_capacity(INCUMBENT_NET_CAP),
            inc_equity: 0,
            inc_equity_peak: 0,
            inc_max_drawdown: 0,
            total_size_lamports: 0,
            opens: 0,
            last_price: BTreeMap::new(),
            seq: 0,
        }
    }

    /// Whether no shadow position is open in any challenger's book.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.challengers.iter().all(|c| c.book.is_empty())
    }

    /// Mirror a live admit into every still-racing challenger's shadow book
    /// (same mint, same entry, same size, same cost, same tick — the paired
    /// design's identical-admit requirement).
    pub fn open(
        &mut self,
        mint: [u8; 32],
        entry_price_fp: u64,
        size_lamports: u64,
        entry_cost_lamports: u64,
        tick: u64,
        vol_bps: u32,
    ) {
        self.note_price(mint, entry_price_fp);
        self.total_size_lamports = self
            .total_size_lamports
            .saturating_add(u128::from(size_lamports));
        self.opens = self.opens.saturating_add(1);
        for c in &mut self.challengers {
            if c.verdict == TournamentVerdict::Dropped {
                continue;
            }
            c.book.open(
                mint,
                entry_price_fp,
                size_lamports,
                entry_cost_lamports,
                tick,
            );
            // §24 LAW 6: arm the recent-window volatility on the shadow book so the
            // vol-stop challenger scales its stop from the same measurement the live
            // path used (no derived TP ladder in shadow — that is a live-only law).
            c.book.arm_context(&mint, vol_bps, None);
        }
    }

    /// Mirror one decoded swap into every challenger's lifecycle, folding any
    /// exits (partial tranches accumulate; closes await pairing).
    pub fn on_trade(
        &mut self,
        mint: &[u8; 32],
        price_fp: u64,
        signed_quote: i128,
        tick: u64,
        liq_lamports: u64,
    ) {
        self.note_price(*mint, price_fp);
        let seq = self.seq;
        for c in &mut self.challengers {
            if let Some(e) = c.book.on_trade(mint, price_fp, signed_quote, tick, liq_lamports) {
                let acc = c.partial.entry(*mint).or_insert(0);
                *acc = acc.saturating_add(e.net_lamports);
                if e.closed {
                    let total = c.partial.remove(mint).unwrap_or(0);
                    c.fold_equity(total);
                    c.push_pending(*mint, total, seq);
                }
            }
        }
    }

    /// Mirror one logical tick: challengers' conditional time-stops fire here
    /// (this is where the hold-ticks factorial dimension bites).
    pub fn on_tick(&mut self, tick: u64, latest_price_fp: &dyn Fn(&[u8; 32]) -> Option<u64>) {
        let seq = self.seq;
        for c in &mut self.challengers {
            for e in c.book.on_tick(tick, latest_price_fp) {
                let total = c
                    .partial
                    .remove(&e.mint)
                    .unwrap_or(0)
                    .saturating_add(e.net_lamports);
                c.fold_equity(total);
                c.push_pending(e.mint, total, seq);
            }
        }
    }

    /// The engine reports the LIVE position for `mint` closed at
    /// `net_lamports`. Pairs and scores every challenger's outcome for the
    /// same mint-entry: a challenger that closed earlier pairs its banked
    /// net; one still holding is force-closed at the latest price (falling
    /// back to the last seen swap/entry mark) — a slower exit is marked at
    /// live-close time, that IS its policy outcome. A challenger that never
    /// held the mint scores no pair (same-admit pairing only).
    pub fn incumbent_closed(
        &mut self,
        mint: &[u8; 32],
        net_lamports: i128,
        latest_price_fp: &dyn Fn(&[u8; 32]) -> Option<u64>,
    ) {
        // Incumbent book-keeping first: the guards below see this trade too.
        if self.incumbent_nets.len() >= INCUMBENT_NET_CAP {
            self.incumbent_nets.pop_front();
        }
        self.incumbent_nets.push_back(net_lamports);
        self.inc_equity = self.inc_equity.saturating_add(net_lamports);
        if self.inc_equity > self.inc_equity_peak {
            self.inc_equity_peak = self.inc_equity;
        }
        let inc_dd = self.inc_equity_peak.saturating_sub(self.inc_equity);
        if inc_dd > self.inc_max_drawdown {
            self.inc_max_drawdown = inc_dd;
        }

        let med_abs = median_abs(&self.incumbent_nets);
        let dd_limit = self
            .inc_max_drawdown
            .saturating_add(self.mean_size_allowance());
        let fallback = self.last_price.get(mint).map(|(p, _)| *p);

        for c in &mut self.challengers {
            if c.verdict == TournamentVerdict::Dropped {
                continue;
            }
            let chal_net: Option<i128> = if let Some((net, _)) = c.pending.remove(mint) {
                Some(net)
            } else if c.book.has(mint) {
                // Force-close at the freshest available mark; a mint with no
                // mark at all (evicted cache) closes at zero — conservative
                // against the challenger, never in its favor.
                let px = latest_price_fp(mint).or(fallback).unwrap_or(0);
                c.book.close_at(mint, px, ExitReason::ForceClose).map(|e| {
                    let total = c
                        .partial
                        .remove(mint)
                        .unwrap_or(0)
                        .saturating_add(e.net_lamports);
                    c.fold_equity(total);
                    total
                })
            } else {
                None
            };
            if let Some(chal) = chal_net {
                let d = chal.saturating_sub(net_lamports);
                c.score_pair(d, med_abs, dd_limit);
                if c.verdict == TournamentVerdict::Dropped {
                    // A dropped challenger's book is retired immediately.
                    let _ = c.book.force_close_all(latest_price_fp);
                    c.partial.clear();
                    c.pending.clear();
                }
            }
        }
        self.last_price.remove(mint);
    }

    /// Tournament standings: one row per challenger with its pre-registered
    /// parameter summary and current SPRT state. Report-only (§56.2) — the
    /// operator acts on `Adoptable`, this module never self-adopts.
    #[must_use]
    pub fn standings(&self) -> Vec<ChallengerStanding> {
        self.challengers
            .iter()
            .enumerate()
            .map(|(i, c)| ChallengerStanding {
                idx: i as u8,
                trail_base_bps: c.params.trail_base_bps,
                tp1_bps: c.params.tp1_bps,
                max_hold_ticks: c.params.max_hold_ticks,
                pairs: c.pairs,
                sprt_millinats: c.llr_millinats,
                verdict: c.verdict,
            })
            .collect()
    }

    /// Record the freshest mark for `mint`, evicting the oldest entry past
    /// [`LAST_PRICE_CAP`].
    fn note_price(&mut self, mint: [u8; 32], price_fp: u64) {
        let seq = self.seq;
        self.seq = self.seq.saturating_add(1);
        if self.last_price.len() >= LAST_PRICE_CAP && !self.last_price.contains_key(&mint) {
            if let Some(oldest) = self
                .last_price
                .iter()
                .min_by_key(|(_, (_, s))| *s)
                .map(|(m, _)| *m)
            {
                self.last_price.remove(&oldest);
            }
        }
        self.last_price.insert(mint, (price_fp, seq));
    }

    /// The drawdown allowance: [`DD_ALLOWANCE_BPS`] of the mean deployed size.
    fn mean_size_allowance(&self) -> i128 {
        if self.opens == 0 {
            return 0;
        }
        let mean = self.total_size_lamports / u128::from(self.opens);
        let allowance = mean.saturating_mul(DD_ALLOWANCE_BPS as u128) / 10_000;
        i128::try_from(allowance).unwrap_or(i128::MAX)
    }
}

// ============================================================================
// LAW B8 — brain-grounded challenger PROPOSALS (report-only).
// ============================================================================

/// §102 lower bound on a proposed conditional hold, ticks. A time stop below one
/// minute of information time (150 ticks ≈ 60 s at 400 ms/tick) is not an exit
/// policy, it is a latency artefact.
pub const PROPOSAL_HOLD_MIN_TICKS: u64 = 150;
/// §102 upper bound on a proposed conditional hold, ticks (≈ 40 minutes). Past
/// this the position is a swing trade and belongs to a different law than §24.
pub const PROPOSAL_HOLD_MAX_TICKS: u64 = 6_000;
/// §102 lower bound on a proposed TP1 principal-recovery arm, mult bps. Below
/// 1.05× the trim does not clear its own round-trip cost (§18).
pub const PROPOSAL_TP1_MIN_BPS: u32 = 10_500;
/// §102 upper bound on a proposed TP1 arm, mult bps (3×). A first trim that far
/// out is not principal recovery.
pub const PROPOSAL_TP1_MAX_BPS: u32 = 30_000;
/// §102 lower bound on a proposed trailing width, bps.
pub const PROPOSAL_TRAIL_MIN_BPS: u32 = 500;
/// §102 upper bound on a proposed trailing width, bps.
pub const PROPOSAL_TRAIL_MAX_BPS: u32 = 6_000;

/// §24/§102 share of a winner's p75 maximum favourable excursion to place the
/// proposed first trim at: 60%.
///
/// Placing TP1 *at* the p75 MFE would fill on roughly a quarter of winners and
/// never on the rest — a target you reach one time in four is not a
/// principal-recovery arm, it is a lottery ticket. 60% of the p75 excursion sits
/// comfortably inside the body of the winning distribution while still being
/// meaningfully further out than the incumbent's fixed arm, which is exactly the
/// hypothesis worth pre-registering.
pub const PROPOSAL_TP1_MFE_SHARE_BP: u64 = 6_000;

/// §24/§102 slack over a winner's median adverse excursion for the proposed trail:
/// 150%.
///
/// A trailing stop set exactly at the heat winners historically took would have
/// stopped out half of them. The proposal is therefore 1.5× that heat — wide
/// enough that the median winner survives it, tight enough to be a genuinely
/// different hypothesis from a fixed width.
pub const PROPOSAL_TRAIL_MAE_SLACK_BP: u64 = 15_000;

/// §99 bound on emitted proposals per export (three axes × two venue phases).
pub const PROPOSAL_CAP: usize = 6;

/// Which exit lever a proposal addresses. One axis per proposal — a challenger
/// that moves three levers at once cannot attribute its edge to any of them
/// (§56.1, no post-hoc fishing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProposalAxis {
    /// Conditional max hold, ticks — derived from the median hold of WINNERS.
    TimeStopTicks,
    /// TP1 principal-recovery arm, mult bps — derived from winners' p75 MFE.
    Tp1Bps,
    /// Trailing width, bps — derived from winners' median adverse excursion.
    TrailBps,
}

impl ProposalAxis {
    /// Stable label for the report.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TimeStopTicks => "time_stop_ticks",
            Self::Tp1Bps => "tp1_bps",
            Self::TrailBps => "trail_bps",
        }
    }
}

/// One brain-grounded challenger **proposal**.
///
/// # What this is, and what it is emphatically not
///
/// The §48 tournament grid is a fixed 2×2×2 factorial chosen by research, and it
/// is fixed for a good reason: a challenger set that moves with the data is a
/// challenger set that has been fitted to the data, and its SPRT verdict means
/// nothing. So the brain does not get to edit the grid.
///
/// What it gets to do is *propose*. A proposal is a parameter value the episodic
/// record says is worth pre-registering — "your winners' median hold is 900 ticks
/// and your incumbent time stop is 420, so a longer stop is a hypothesis your own
/// data generated". An operator promotes a proposal into the pre-registered grid
/// through the §56.2 config envelope, at which point it races the incumbent under
/// the existing SPRT machinery like every other challenger. Nothing here adopts
/// anything, and there is no code path from a proposal to a live exit parameter.
///
/// Fail-closed: no proposal is emitted below the sample floor, and none is emitted
/// when the derived value equals the incumbent's (that is not a challenger).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitProposal {
    /// The single lever this proposal moves.
    pub axis: ProposalAxis,
    /// [`pump_quant_brain::fingerprint::VenuePhase::ordinal`] the evidence came
    /// from (§100: curve and pool exit behaviour are never pooled).
    pub venue_phase_code: u8,
    /// The proposed value (ticks for `TimeStopTicks`, bps otherwise).
    pub value: u64,
    /// The incumbent's current value on the same axis, for the diff.
    pub incumbent_value: u64,
    /// Winning episodes the derivation stood on. Always ≥ the sample floor.
    pub derived_from_n: u32,
}

/// Nearest-rank order statistic over a sorted slice at `num/den` (e.g. 3/4 for
/// p75). Integer only; empty ⇒ `0`.
fn order_stat_u64(sorted: &[u64], num: usize, den: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (sorted.len().saturating_mul(num) / den).min(sorted.len() - 1);
    sorted[idx]
}

/// Nearest-rank order statistic over a sorted `i64` slice.
fn order_stat_i64(sorted: &[i64], num: usize, den: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (sorted.len().saturating_mul(num) / den).min(sorted.len() - 1);
    sorted[idx]
}

/// LAW B8: derive pre-registration candidates for the §48 grid from the recall
/// distribution of the setups that actually PAID.
///
/// The cohort is every admitted episode in `phase` whose realized net is strictly
/// positive — the best-performing setups, taken as a cohort rather than as a
/// single fingerprint class, because a single class rarely clears a sample floor
/// worth trusting and pooling within one venue phase is exactly the pooling §100
/// permits.
///
/// Three derivations, each on one axis:
///
/// * **time stop** ← winners' median hold. If half our winners were still working
///   at tick 900, a stop at 420 was cutting them.
/// * **TP1 arm** ← [`PROPOSAL_TP1_MFE_SHARE_BP`] of winners' p75 MFE. Where the
///   good trades actually went, discounted so the arm is reachable.
/// * **trail width** ← [`PROPOSAL_TRAIL_MAE_SLACK_BP`] of winners' median adverse
///   excursion. How much heat a winner has to be allowed to take.
///
/// Fail-closed: fewer than `min_sample` winners ⇒ an EMPTY vector, not a
/// low-confidence guess. Every derived value is clamped into a named envelope, so
/// a pathological tape cannot propose a two-tick time stop.
///
/// Sorted by `(venue_phase, axis)` — total and independent of index order.
#[must_use]
pub fn brain_exit_proposals(
    index: &pump_quant_brain::recall::EpisodicIndex,
    incumbent: &LifecycleParams,
    phase: pump_quant_brain::fingerprint::VenuePhase,
    min_sample: u32,
    tick_ns: u64,
) -> Vec<ExitProposal> {
    if tick_ns == 0 {
        return Vec::new();
    }
    let mut holds: Vec<u64> = Vec::new();
    let mut mfes: Vec<i64> = Vec::new();
    let mut maes: Vec<i64> = Vec::new();
    for e in index.iter_oldest_first() {
        if e.context().venue_phase != phase {
            continue;
        }
        let o = e.outcome();
        if !o.was_admitted || o.realized_net_lamports <= 0 {
            continue;
        }
        holds.push(o.hold_duration_ns);
        mfes.push(o.mfe_bps);
        maes.push(o.mae_bps.saturating_abs());
    }
    let n = u32::try_from(holds.len()).unwrap_or(u32::MAX);
    if n < min_sample {
        // §46: below the floor there is no proposal, not a weak one.
        return Vec::new();
    }
    holds.sort_unstable();
    mfes.sort_unstable();
    maes.sort_unstable();

    let phase_code = phase.ordinal();
    let mut out: Vec<ExitProposal> = Vec::new();

    // --- time stop ---------------------------------------------------------
    let median_hold_ns = order_stat_u64(&holds, 1, 2);
    let hold_ticks =
        (median_hold_ns / tick_ns).clamp(PROPOSAL_HOLD_MIN_TICKS, PROPOSAL_HOLD_MAX_TICKS);
    if hold_ticks != incumbent.max_hold_ticks {
        out.push(ExitProposal {
            axis: ProposalAxis::TimeStopTicks,
            venue_phase_code: phase_code,
            value: hold_ticks,
            incumbent_value: incumbent.max_hold_ticks,
            derived_from_n: n,
        });
    }

    // --- TP1 arm -----------------------------------------------------------
    let p75_mfe = order_stat_i64(&mfes, 3, 4).max(0);
    let tp1 = u32::try_from(
        10_000u64
            .saturating_add((p75_mfe as u64).saturating_mul(PROPOSAL_TP1_MFE_SHARE_BP) / 10_000),
    )
    .unwrap_or(PROPOSAL_TP1_MAX_BPS)
    .clamp(PROPOSAL_TP1_MIN_BPS, PROPOSAL_TP1_MAX_BPS);
    if tp1 != incumbent.tp1_bps {
        out.push(ExitProposal {
            axis: ProposalAxis::Tp1Bps,
            venue_phase_code: phase_code,
            value: u64::from(tp1),
            incumbent_value: u64::from(incumbent.tp1_bps),
            derived_from_n: n,
        });
    }

    // --- trailing width ----------------------------------------------------
    let median_mae = order_stat_i64(&maes, 1, 2).max(0);
    let trail =
        u32::try_from((median_mae as u64).saturating_mul(PROPOSAL_TRAIL_MAE_SLACK_BP) / 10_000)
            .unwrap_or(PROPOSAL_TRAIL_MAX_BPS)
            .clamp(PROPOSAL_TRAIL_MIN_BPS, PROPOSAL_TRAIL_MAX_BPS);
    if trail != incumbent.trail_base_bps {
        out.push(ExitProposal {
            axis: ProposalAxis::TrailBps,
            venue_phase_code: phase_code,
            value: u64::from(trail),
            incumbent_value: u64::from(incumbent.trail_base_bps),
            derived_from_n: n,
        });
    }

    out.sort_by(|a, b| {
        a.venue_phase_code
            .cmp(&b.venue_phase_code)
            .then(a.axis.cmp(&b.axis))
    });
    out.truncate(PROPOSAL_CAP);
    out
}

/// `|median|` of the incumbent net ring (even lengths average the two central
/// elements, integer division). Zero for an empty ring.
fn median_abs(nets: &VecDeque<i128>) -> i128 {
    if nets.is_empty() {
        return 0;
    }
    let mut sorted: Vec<i128> = nets.iter().copied().collect();
    sorted.sort_unstable();
    let n = sorted.len();
    let median = if n % 2 == 1 {
        sorted[n / 2]
    } else {
        let a = sorted[n / 2 - 1];
        let b = sorted[n / 2];
        (a / 2)
            .saturating_add(b / 2)
            .saturating_add((a % 2 + b % 2) / 2)
    };
    median.saturating_abs()
}

// ============================================================================
// Tests.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct deterministic mint ids.
    fn mint(i: u64) -> [u8; 32] {
        let mut m = [0u8; 32];
        m[..8].copy_from_slice(&i.to_le_bytes());
        m
    }

    const ENTRY: u64 = 1_000_000;
    const SIZE: u64 = 1_000_000_000;

    /// One same-admit pair: open everywhere, then the live close arrives with
    /// the market marked at `mark_px` and the incumbent netting `inc_net`.
    /// Challengers force-close at the mark.
    fn run_pair(t: &mut ExitTournament, i: u64, mark_px: u64, inc_net: i128) {
        t.open(mint(i), ENTRY, SIZE, SIZE, i, 0);
        t.incumbent_closed(&mint(i), inc_net, &|_| Some(mark_px));
    }

    /// Challenger net for a full force-close at `mult` bps of entry with this
    /// module's SIZE/cost conventions (fee 1%, first-sell 150 bps, tip 1e4).
    fn force_close_net(mult_bps: u128) -> i128 {
        let gross = u128::from(SIZE) * mult_bps / 10_000;
        let fee = gross * 100 / 10_000;
        let penalty = u128::from(SIZE) * 150 / 10_000;
        (gross - fee - penalty) as i128 - i128::from(SIZE) - 10_000
    }

    #[test]
    fn builds_the_registered_factorial() {
        let t = ExitTournament::new(LifecycleParams::standard());
        let s = t.standings();
        assert_eq!(s.len(), K_CHALLENGERS);
        // The first 8 challengers form the 2×2×2 factorial: all combinations once.
        let factorial = &s[..K_FACTORIAL];
        let mut combos: Vec<(u32, u32, u64)> = factorial
            .iter()
            .map(|c| (c.trail_base_bps, c.tp1_bps, c.max_hold_ticks))
            .collect();
        combos.sort_unstable();
        combos.dedup();
        assert_eq!(combos.len(), K_FACTORIAL);
        for c in factorial {
            assert!(matches!(c.trail_base_bps, TRAIL_TIGHT_BPS | TRAIL_WIDE_BPS));
            assert!(matches!(c.tp1_bps, TP1_ARM_EARLY_BPS | TP1_ARM_LATE_BPS));
            assert!(matches!(
                c.max_hold_ticks,
                HOLD_SHORT_TICKS | HOLD_LONG_TICKS
            ));
            assert_eq!(c.verdict, TournamentVerdict::Racing);
        }
        // Challengers 8 and 9 are the Batch-2a single-axis arms: they inherit every
        // incumbent parameter (so trail/tp1/hold read as the incumbent's), and only
        // their new exit axis differs — verified behaviourally in audit_wave2_laws.
        let inc = LifecycleParams::standard();
        for c in &s[K_FACTORIAL..] {
            assert_eq!(c.trail_base_bps, inc.trail_base_bps);
            assert_eq!(c.tp1_bps, inc.tp1_bps);
            assert_eq!(c.max_hold_ticks, inc.max_hold_ticks);
            assert_eq!(c.verdict, TournamentVerdict::Racing);
        }
        assert!(t.is_empty());
    }

    #[test]
    fn appended_challengers_arm_only_their_axis() {
        let t = ExitTournament::new(LifecycleParams::standard());
        // The exit-into-strength arm toggles ONLY its axis; the vol-stop arm too.
        let is_arm = &t.challengers[IDX_INTO_STRENGTH];
        assert!(is_arm.params.into_strength_exit_enable);
        assert!(!is_arm.params.vol_stop_enable);
        let vs_arm = &t.challengers[IDX_VOL_STOP];
        assert!(vs_arm.params.vol_stop_enable);
        assert!(!vs_arm.params.into_strength_exit_enable);
    }

    #[test]
    fn pairs_score_sign_correctly() {
        let mut t = ExitTournament::new(LifecycleParams::standard());
        // Pair 1: mark at 1.2× → challenger net ≈ +173e6 vs incumbent −1_000:
        // d > 0, a win.
        run_pair(&mut t, 0, 1_200_000, -1_000);
        for c in &t.standings() {
            assert_eq!(c.pairs, 1);
            assert_eq!(c.sprt_millinats, SPRT_WIN_MILLINATS);
        }
        // Pair 2: mark at 0.4× → challenger deep negative vs incumbent 0:
        // d < 0, a loss.
        run_pair(&mut t, 1, 400_000, 0);
        for c in &t.standings() {
            assert_eq!(c.pairs, 2);
            assert_eq!(c.sprt_millinats, SPRT_WIN_MILLINATS + SPRT_LOSS_MILLINATS);
        }
        // Pair 3: exact tie — incumbent net equals the challenger force-close
        // net at 1.0× — contributes zero.
        let tie_net = force_close_net(10_000);
        run_pair(&mut t, 2, 1_000_000, tie_net);
        for c in &t.standings() {
            assert_eq!(c.pairs, 3);
            assert_eq!(c.sprt_millinats, SPRT_WIN_MILLINATS + SPRT_LOSS_MILLINATS);
        }
        assert!(t.is_empty(), "every pair force-closed the shadow books");
    }

    #[test]
    fn challenger_close_first_pairs_from_pending() {
        let mut t = ExitTournament::new(LifecycleParams::standard());
        t.open(mint(0), ENTRY, SIZE, SIZE, 0, 0);
        // A −40% single print: the rug precursor closes ALL shadow books.
        t.on_trade(&mint(0), 1_100_000, 500_000, 1);
        t.on_trade(&mint(0), 660_000, -900_000, 2);
        assert!(t.is_empty(), "precursor closed every challenger");
        // Live close arrives later, much worse: every challenger banked a
        // better exit → d > 0 for all.
        t.incumbent_closed(&mint(0), -800_000_000, &|_| None);
        for c in &t.standings() {
            assert_eq!(c.pairs, 1);
            assert_eq!(c.sprt_millinats, SPRT_WIN_MILLINATS);
        }
        // Pending fully drained.
        for c in &t.challengers {
            assert!(c.pending.is_empty());
            assert!(c.partial.is_empty());
        }
    }

    #[test]
    fn sustained_losses_drop_the_challengers_and_empty_their_books() {
        let mut t = ExitTournament::new(LifecycleParams::standard());
        for i in 0..u64::from(PAIRS_MIN) {
            // Mark 0.4×: challenger ≈ −619e6 vs incumbent 0 → always a loss.
            run_pair(&mut t, i, 400_000, 0);
        }
        for c in &t.standings() {
            assert_eq!(c.verdict, TournamentVerdict::Dropped);
            assert_eq!(c.pairs, PAIRS_MIN);
        }
        // A dropped challenger takes no further admits.
        t.open(mint(9_999), ENTRY, SIZE, SIZE, 1_000, 0);
        assert!(t.is_empty(), "dropped challengers refuse new admits");
    }

    #[test]
    fn sustained_wins_flag_adoptable_under_the_guards() {
        let mut t = ExitTournament::new(LifecycleParams::standard());
        for i in 0..u64::from(PAIRS_MIN) {
            // Mark 1.2×: challenger ≈ +173e6 vs incumbent −1_000 → every pair
            // a win; materiality holds (mean d ≫ 10% of |median| = 100) and
            // the challenger equity only rises (zero drawdown).
            run_pair(&mut t, i, 1_200_000, -1_000);
        }
        for c in &t.standings() {
            assert_eq!(c.verdict, TournamentVerdict::Adoptable);
            assert_eq!(c.pairs, PAIRS_MIN);
            assert!(c.sprt_millinats >= SPRT_UPPER_MILLINATS);
        }
    }

    #[test]
    fn truncation_resets_the_ledger_and_keeps_params() {
        let mut t = ExitTournament::new(LifecycleParams::standard());
        let tie_net = force_close_net(10_000);
        for i in 0..u64::from(PAIRS_TRUNCATE) {
            run_pair(&mut t, i, 1_000_000, tie_net);
        }
        let s = t.standings();
        for c in &s {
            assert_eq!(c.verdict, TournamentVerdict::Racing);
            assert_eq!(c.pairs, 0, "SPRT ledger reset at truncation");
            assert_eq!(c.sprt_millinats, 0);
        }
        // Params survive the reset (factorial block keeps its tight/wide arms).
        for c in &s[..K_FACTORIAL] {
            assert!(matches!(c.trail_base_bps, TRAIL_TIGHT_BPS | TRAIL_WIDE_BPS));
        }
    }

    #[test]
    fn determinism_same_stream_same_standings() {
        let run = || {
            let mut t = ExitTournament::new(LifecycleParams::standard());
            for i in 0..60u64 {
                let px = if i % 3 == 0 { 900_000 } else { 1_150_000 };
                let inc = if i % 3 == 0 { -40_000_000 } else { 30_000_000 };
                t.open(mint(i), ENTRY, SIZE, SIZE, i * 10, 0);
                t.on_trade(&mint(i), 1_050_000, 250_000, i * 10 + 1);
                t.on_tick(i * 10 + 2, &|_| Some(px));
                t.incumbent_closed(&mint(i), inc, &|_| Some(px));
            }
            t.standings()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn bounded_pending_and_price_caches() {
        let mut t = ExitTournament::new(LifecycleParams::standard());
        // Open + rug-close many mints WITHOUT ever reporting a live close:
        // pending outcomes and price marks must stay capped.
        for i in 0..(PENDING_CLOSED_CAP as u64 + 40) {
            t.open(mint(i), ENTRY, SIZE, SIZE, i * 3, 0);
            t.on_trade(&mint(i), 1_100_000, 500_000, i * 3 + 1);
            t.on_trade(&mint(i), 660_000, -900_000, i * 3 + 2);
        }
        for c in &t.challengers {
            assert!(c.pending.len() <= PENDING_CLOSED_CAP);
        }
        assert!(t.last_price.len() <= LAST_PRICE_CAP);
        assert!(t.is_empty());
    }
}
