//! `evaluator_stats` — the frozen evaluator's statistics core.
//!
//! Every statistic in this module is computed with integer / fixed-point
//! arithmetic only (constitution §22): all money is integer lamports carried in
//! `i128` accumulators, and all ratios are basis points (bps). There are no
//! `f32`/`f64` values anywhere in outcome-controlling logic.
//!
//! Determinism is a hard contract: identical input slices always produce
//! byte-for-byte identical outputs. No `HashMap` iteration order, no wall-clock,
//! no RNG. Grouping that must be ordered uses `BTreeMap` so output order is a
//! deterministic function of the keys.
//!
//! Overflow is never silent. Money accumulation uses checked adds that panic
//! with an explicit message if the (very large) `i128` headroom is ever
//! exhausted, which for reconciled lamport books is an impossibility that would
//! signal corrupt input rather than normal operation.

use std::collections::BTreeMap;

// ============================================================================
// Shared newtypes / enums
// ============================================================================

/// Trading lane an order belongs to. Lanes never blend (§48 objective law):
/// a per-lane statistic filters to exactly one lane by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    /// Fast in/out scalps.
    Scalp,
    /// Early-entry / discovery lane.
    Early,
}

/// Stable identifier for a single reconciled trade. Ordering is used only as a
/// deterministic tie-break, never as a value in a statistic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TradeId(pub u64);

impl TradeId {
    /// Test/golden-vector constructor.
    pub fn test(id: u64) -> Self {
        TradeId(id)
    }
}

/// Identifier for a rejection gate in the post-rejection forward-sampling
/// ledger. Ordering drives deterministic per-gate output order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GateId(pub u64);

impl GateId {
    /// Test/golden-vector constructor.
    pub fn test(id: u64) -> Self {
        GateId(id)
    }
}

/// Archetype grouping key for excursion statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArchetypeKey {
    /// Opaque archetype discriminator.
    pub id: u64,
}

impl ArchetypeKey {
    /// Test/golden-vector constructor — a single fixed archetype.
    pub fn test() -> Self {
        ArchetypeKey { id: 0 }
    }
}

/// Order side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// Buy / long entry.
    Buy,
    /// Sell / short or exit.
    Sell,
}

/// Fill classification for markout bucketing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FillClass {
    /// Scalp-lane entry fill.
    ScalpEntry,
    /// Scalp-lane exit fill.
    ScalpExit,
    /// Early-lane entry fill.
    EarlyEntry,
    /// Early-lane exit fill.
    EarlyExit,
}

// ============================================================================
// Small integer helpers (no floats)
// ============================================================================

/// Signed basis-point move from `from` to `to`: `(to - from) * 10_000 / from`.
///
/// Computed in `i128` to avoid intermediate overflow, truncating toward zero
/// (standard integer division). A `from` of zero has no defined relative move
/// and yields `0`.
fn signed_bps_move(from: u64, to: u64) -> i64 {
    if from == 0 {
        return 0;
    }
    let num = to as i128 - from as i128;
    ((num * 10_000) / from as i128) as i64
}

/// Deterministic integer median of an already-sorted slice.
///
/// Odd length picks the middle element; even length averages the two central
/// elements with integer division (in `i128` to avoid overflow). Empty -> 0.
fn median_sorted(sorted: &[i64]) -> i64 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        let a = sorted[n / 2 - 1] as i128;
        let b = sorted[n / 2] as i128;
        ((a + b) / 2) as i64
    }
}

/// Nearest-rank quantile (`num`/`den`) of an already-sorted slice.
///
/// Deterministic and float-free: rank = ceil(num/den * n), clamped into range.
fn quantile_sorted(sorted: &[i64], num: usize, den: usize) -> i64 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    let rank = (num * n + den - 1) / den; // ceil(num/den * n)
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

// ============================================================================
// Leaf: ev_net_sol
// ============================================================================

/// A reconciled trade: gross P&L plus every cost class, tagged with its lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconTrade {
    /// Lane this trade belongs to.
    pub lane: Lane,
    /// Gross lamports = proceeds - cost_basis (may be negative).
    pub gross_lamports: i128,
    /// Trading/protocol fees paid, lamports.
    pub fees: u128,
    /// Priority tips paid, lamports.
    pub tips: u128,
    /// Cost of failed attempts attributable to this trade, lamports.
    pub failed_costs: u128,
}

impl ReconTrade {
    /// Test/golden-vector constructor.
    pub fn test(lane: Lane, gross: i128, fee: u128, tip: u128, failc: u128) -> Self {
        ReconTrade {
            lane,
            gross_lamports: gross,
            fees: fee,
            tips: tip,
            failed_costs: failc,
        }
    }
}

/// Reconciled net-SOL aggregate for one lane. Carries its sample size; an
/// aggregate over no trades is [`NetSol::missing`], never a fabricated zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetSol {
    /// Net lamports = gross - fees - tips - failed_costs.
    pub net_lamports: i128,
    /// Gross lamports summed over included trades.
    pub gross_lamports: i128,
    /// Total fees, lamports.
    pub fees: u128,
    /// Total tips, lamports.
    pub tips: u128,
    /// Total failed-attempt costs, lamports.
    pub failed_costs: u128,
    /// Number of included trades.
    pub n: u32,
}

impl NetSol {
    /// The "no data" aggregate: every field zero, `n == 0`.
    pub fn missing() -> Self {
        NetSol {
            net_lamports: 0,
            gross_lamports: 0,
            fees: 0,
            tips: 0,
            failed_costs: 0,
            n: 0,
        }
    }

    /// True iff this aggregate reflects zero included trades.
    pub fn is_missing(&self) -> bool {
        self.n == 0
    }
}

/// Reconciled net-SOL aggregation for a single lane, every cost class included.
///
/// `net = gross - fees - tips - failed_costs`, all in `i128`-accumulated
/// lamports. Filters to `lane` — cross-lane blending is impossible by
/// construction. Empty (post-filter) input returns [`NetSol::missing`].
pub fn net_sol(trades: &[ReconTrade], lane: Lane) -> NetSol {
    let mut gross: i128 = 0;
    let mut fees: u128 = 0;
    let mut tips: u128 = 0;
    let mut failed: u128 = 0;
    let mut n: u32 = 0;

    for t in trades.iter().filter(|t| t.lane == lane) {
        gross = gross
            .checked_add(t.gross_lamports)
            .expect("net_sol: gross i128 overflow");
        fees = fees.checked_add(t.fees).expect("net_sol: fees overflow");
        tips = tips.checked_add(t.tips).expect("net_sol: tips overflow");
        failed = failed
            .checked_add(t.failed_costs)
            .expect("net_sol: failed_costs overflow");
        n += 1;
    }

    if n == 0 {
        return NetSol::missing();
    }

    let costs = (fees as i128)
        .checked_add(tips as i128)
        .and_then(|x| x.checked_add(failed as i128))
        .expect("net_sol: cost i128 overflow");
    let net = gross
        .checked_sub(costs)
        .expect("net_sol: net i128 overflow");

    NetSol {
        net_lamports: net,
        gross_lamports: gross,
        fees,
        tips,
        failed_costs: failed,
        n,
    }
}

// ============================================================================
// Leaf: ev_mfe_capture
// ============================================================================

/// One excursion observation for a trade, in basis points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExcursionRow {
    /// Archetype this row belongs to.
    pub key: ArchetypeKey,
    /// Maximum favorable excursion, bps.
    pub mfe_bps: i64,
    /// Maximum adverse excursion, bps.
    pub mae_bps: i64,
    /// Realized outcome, bps (may be negative).
    pub realized_bps: i64,
    /// Whether this row passed authenticity screening (criterion 107).
    pub authenticity_screened: bool,
}

impl ExcursionRow {
    /// Test/golden-vector constructor.
    pub fn test(key: ArchetypeKey, mfe: i64, mae: i64, real: i64, scr: bool) -> Self {
        ExcursionRow {
            key,
            mfe_bps: mfe,
            mae_bps: mae,
            realized_bps: real,
            authenticity_screened: scr,
        }
    }
}

/// Deterministic quartile summary (bps), float-free nearest-rank / integer
/// median. `min`/`max` are the extremes; `q1`/`median`/`q3` the 25/50/75 marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Quartiles {
    /// Minimum value.
    pub min: i64,
    /// First quartile (25th percentile, nearest-rank).
    pub q1: i64,
    /// Median (50th percentile).
    pub median: i64,
    /// Third quartile (75th percentile, nearest-rank).
    pub q3: i64,
    /// Maximum value.
    pub max: i64,
}

impl Quartiles {
    /// Build quartiles from a slice by sorting a copy (deterministic).
    fn from_values(values: &[i64]) -> Quartiles {
        if values.is_empty() {
            return Quartiles::default();
        }
        let mut s = values.to_vec();
        s.sort_unstable();
        Quartiles {
            min: s[0],
            q1: quantile_sorted(&s, 1, 4),
            median: median_sorted(&s),
            q3: quantile_sorted(&s, 3, 4),
            max: s[s.len() - 1],
        }
    }
}

/// Capture-efficiency ratio: present as an integer bps figure, or `Missing`
/// when it is undefined (no favorable excursion to capture).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureRatio {
    /// Capture ratio in bps of MFE.
    Bps(u32),
    /// Undefined — `sum(mfe_bps) <= 0`.
    Missing,
}

impl CaptureRatio {
    /// True iff undefined.
    pub fn is_missing(&self) -> bool {
        matches!(self, CaptureRatio::Missing)
    }
}

/// Direct comparison against a raw bps figure, so golden vectors can assert a
/// plain integer while [`CaptureRatio::Missing`] remains representable.
impl PartialEq<u32> for CaptureRatio {
    fn eq(&self, other: &u32) -> bool {
        matches!(self, CaptureRatio::Bps(v) if v == other)
    }
}

/// Per-archetype MFE/MAE distribution and capture efficiency (criterion 107).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MfeReport {
    /// Number of screened rows for this archetype.
    pub n: u32,
    /// Number of rows excluded because they were not authenticity-screened.
    pub excluded_unscreened: u32,
    /// MFE distribution over screened rows.
    pub mfe_bps: Quartiles,
    /// MAE distribution over screened rows.
    pub mae_bps: Quartiles,
    /// Realized-vs-MFE capture ratio in bps.
    pub capture_bps_of_mfe: CaptureRatio,
}

/// Per-archetype MFE/MAE distribution and capture-efficiency ratio, computed
/// over authenticity-screened rows ONLY (criterion 107).
///
/// Unscreened rows matching `key` are counted in `excluded_unscreened` and
/// otherwise ignored — a wash-printed phantom excursion cannot inflate MFE.
/// `capture = sum(realized clamped >= 0) * 10_000 / sum(mfe_bps)` (integer),
/// [`CaptureRatio::Missing`] when `sum(mfe_bps) <= 0`.
pub fn mfe_capture(rows: &[ExcursionRow], key: ArchetypeKey) -> MfeReport {
    let mut mfe: Vec<i64> = Vec::new();
    let mut mae: Vec<i64> = Vec::new();
    let mut sum_mfe: i128 = 0;
    let mut sum_realized_pos: i128 = 0;
    let mut excluded_unscreened: u32 = 0;

    for row in rows.iter().filter(|r| r.key == key) {
        if !row.authenticity_screened {
            excluded_unscreened += 1;
            continue;
        }
        mfe.push(row.mfe_bps);
        mae.push(row.mae_bps);
        sum_mfe += row.mfe_bps as i128;
        sum_realized_pos += row.realized_bps.max(0) as i128;
    }

    let capture = if sum_mfe <= 0 {
        CaptureRatio::Missing
    } else {
        let raw = (sum_realized_pos * 10_000) / sum_mfe;
        let clamped = raw.clamp(0, u32::MAX as i128);
        CaptureRatio::Bps(clamped as u32)
    };

    MfeReport {
        n: mfe.len() as u32,
        excluded_unscreened,
        mfe_bps: Quartiles::from_values(&mfe),
        mae_bps: Quartiles::from_values(&mae),
        capture_bps_of_mfe: capture,
    }
}

// ============================================================================
// Leaf: ev_topk_excision
// ============================================================================

/// Result of removing the top-`k` winners from a lane's reconciled net book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Excision {
    /// Number of top winners excised.
    pub k: u32,
    /// Lane net with the top-`k` winners removed.
    pub net_without_topk: i128,
    /// True iff the full book was positive but goes non-positive after excision.
    pub flipped_negative: bool,
}

/// Top-k winner-excision PnL concentration for a lane (criterion 108).
///
/// Trades are sorted descending by reconciled net, tie-broken ascending by
/// [`TradeId`] (deterministic). For each requested `k`, the top-`k` *winners*
/// are removed: `net_without_topk = total - sum(top-k positive nets)`.
/// Because only positive nets are excised, `k >= n` leaves exactly the sum of
/// the non-positive trades (all winners removed), and `flipped_negative`
/// surfaces Kamat-class fragility where a lane's whole profit rests on a few
/// trades.
pub fn topk_excision(net_per_trade: &[(TradeId, i128)], ks: &[u32]) -> Vec<Excision> {
    // Total over the whole book.
    let mut total: i128 = 0;
    for &(_, net) in net_per_trade {
        total = total
            .checked_add(net)
            .expect("topk_excision: total i128 overflow");
    }

    // Sort a copy: descending by net, then ascending by TradeId.
    let mut sorted: Vec<(TradeId, i128)> = net_per_trade.to_vec();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Prefix sums of *clamped* (winners-only) nets in descending order.
    // prefix[j] = sum over the first j sorted entries of max(net, 0).
    let n = sorted.len();
    let mut prefix: Vec<i128> = Vec::with_capacity(n + 1);
    prefix.push(0);
    for &(_, net) in &sorted {
        let clamped = net.max(0);
        let next = prefix
            .last()
            .unwrap()
            .checked_add(clamped)
            .expect("topk_excision: prefix i128 overflow");
        prefix.push(next);
    }

    ks.iter()
        .map(|&k| {
            let take = (k as usize).min(n);
            let excised = prefix[take];
            let net_without_topk = total
                .checked_sub(excised)
                .expect("topk_excision: net_without_topk i128 overflow");
            let flipped_negative = total > 0 && net_without_topk <= 0;
            Excision {
                k,
                net_without_topk,
                flipped_negative,
            }
        })
        .collect()
}

// ============================================================================
// Leaf: ev_inactivity_label
// ============================================================================

/// Versioned terminal-state label for an inactivity interval.
///
/// The label is a parameterized fact: it is only meaningful for the exact
/// `(delta_t_ns, window_end_ns)` recorded in `params_version`. A token that
/// resumes trading after `delta_t` under a *different* parameterization does not
/// retroactively flip this label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalLabel {
    /// Whether a qualifying inactivity gap was found.
    pub dead: bool,
    /// End of the last swap before the first qualifying gap.
    pub died_at_ns: Option<u64>,
    /// The `(delta_t_ns, window_end_ns)` parameterization that produced this label.
    pub params_version: (u64, u64),
}

/// Versioned inactivity-interval terminal-state labeler (criterion 108).
///
/// `dead` iff there exists a gap `>= delta_t_ns` between two consecutive swaps,
/// or between the last swap and `window_end_ns`. `died_at_ns` is the timestamp
/// of the last swap before the FIRST qualifying gap. With no swaps, the token
/// is dead iff `window_end_ns >= delta_t_ns`, with the window treated as
/// starting at 0 (`died_at_ns == Some(0)`).
///
/// Input timestamps must be non-decreasing; a violation is an input error
/// (debug-asserted), never silently sorted away.
pub fn label_terminal(swap_ts_ns: &[u64], window_end_ns: u64, delta_t_ns: u64) -> TerminalLabel {
    debug_assert!(
        swap_ts_ns.windows(2).all(|w| w[0] <= w[1]),
        "label_terminal: swap timestamps must be non-decreasing"
    );

    let params_version = (delta_t_ns, window_end_ns);

    // No swaps: the entire window is one inactivity gap starting at 0.
    if swap_ts_ns.is_empty() {
        let dead = window_end_ns >= delta_t_ns;
        return TerminalLabel {
            dead,
            died_at_ns: if dead { Some(0) } else { None },
            params_version,
        };
    }

    // Adjacent swap-to-swap gaps first; the first qualifier wins.
    for w in swap_ts_ns.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b.saturating_sub(a) >= delta_t_ns {
            return TerminalLabel {
                dead: true,
                died_at_ns: Some(a),
                params_version,
            };
        }
    }

    // Trailing gap between the last swap and the window end.
    let last = *swap_ts_ns.last().unwrap();
    if window_end_ns.saturating_sub(last) >= delta_t_ns {
        return TerminalLabel {
            dead: true,
            died_at_ns: Some(last),
            params_version,
        };
    }

    TerminalLabel {
        dead: false,
        died_at_ns: None,
        params_version,
    }
}

// ============================================================================
// Leaf: ev_prfs_ledger
// ============================================================================

/// One post-rejection forward-sampling observation: where a gated-out
/// candidate's price went, relative to the price at rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrfsSample {
    /// Gate that rejected the candidate.
    pub gate: GateId,
    /// Reference (fixed-point) price at the moment of rejection.
    pub ref_price_fp: u64,
    /// Sampled (fixed-point) price at the horizon.
    pub sampled_price_fp: u64,
    /// Forward horizon of this sample, seconds.
    pub horizon_s: u32,
}

impl PrfsSample {
    /// Test/golden-vector constructor.
    pub fn test(gate: u64, ref_price_fp: u64, sampled_price_fp: u64, horizon_s: u32) -> Self {
        PrfsSample {
            gate: GateId(gate),
            ref_price_fp,
            sampled_price_fp,
            horizon_s,
        }
    }
}

/// Per-gate forward-sampling ledger: what a gate's rejections avoided AND cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateLedger {
    /// The gate this ledger describes.
    pub gate: GateId,
    /// Number of counted rejection events for this gate.
    pub n: u32,
    /// Events where the sampled price halved (<= ref/2) within 24h.
    pub halved_within_24h: u32,
    /// Events where the sampled price doubled (>= ref*2) within 24h.
    pub doubled_within_24h: u32,
    /// Sum of loss-avoided bps (downside the rejection dodged), accumulated positive.
    pub loss_avoided_bps_sum: i128,
    /// Sum of upside-foregone bps (winners the rejection discarded).
    pub upside_foregone_bps_sum: i128,
}

/// Horizon ceiling for "within 24h" accounting, in seconds.
const HORIZON_24H_S: u32 = 86_400;

/// Post-rejection forward-sampling ledger fold (criterion 108).
///
/// Each sample within the 24h horizon is a rejection event judged on BOTH sides
/// of the over-rejection law: `loss_avoided` accumulates how far below the
/// reference the price fell (downside the gate dodged), and `upside_foregone`
/// accumulates how far above it rose (winners the gate discarded). A gate is
/// never scored on avoided losses alone. Output is grouped by gate in
/// deterministic ascending [`GateId`] order.
pub fn prfs_fold(samples: &[PrfsSample]) -> Vec<GateLedger> {
    let mut out: BTreeMap<GateId, GateLedger> = BTreeMap::new();

    for s in samples {
        // Only samples within the 24h horizon are counted.
        if s.horizon_s > HORIZON_24H_S {
            continue;
        }

        let g = out.entry(s.gate).or_insert(GateLedger {
            gate: s.gate,
            n: 0,
            halved_within_24h: 0,
            doubled_within_24h: 0,
            loss_avoided_bps_sum: 0,
            upside_foregone_bps_sum: 0,
        });

        g.n += 1;

        let ref_fp = s.ref_price_fp;
        let sampled = s.sampled_price_fp;

        if sampled <= ref_fp / 2 {
            g.halved_within_24h += 1;
        }
        if sampled >= ref_fp.saturating_mul(2) {
            g.doubled_within_24h += 1;
        }

        // Loss avoided: downside below ref (<= 0 move) recorded as positive.
        let down_bps = signed_bps_move(ref_fp, sampled.min(ref_fp));
        g.loss_avoided_bps_sum = g
            .loss_avoided_bps_sum
            .checked_add(-(down_bps as i128))
            .expect("prfs_fold: loss_avoided overflow");

        // Upside foregone: upside above ref (>= 0 move) recorded as positive.
        let up_bps = signed_bps_move(ref_fp, sampled.max(ref_fp));
        g.upside_foregone_bps_sum = g
            .upside_foregone_bps_sum
            .checked_add(up_bps as i128)
            .expect("prfs_fold: upside_foregone overflow");
    }

    out.into_values().collect()
}

// ============================================================================
// Leaf: ev_markout
// ============================================================================

/// One fill and the reference price observed at its fixed forward horizon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillRow {
    /// Fill classification.
    pub class: FillClass,
    /// Side of the fill.
    pub side: Side,
    /// Fill price (fixed-point / integer).
    pub fill_price: u64,
    /// Price at the horizon (fixed-point / integer).
    pub later_price: u64,
    /// Horizon at which `later_price` was observed, seconds.
    pub horizon_s: u32,
}

impl FillRow {
    /// Test/golden-vector constructor.
    pub fn test(class: FillClass, side: Side, fill: u64, later: u64, horizon_s: u32) -> Self {
        FillRow {
            class,
            side,
            fill_price: fill,
            later_price: later,
            horizon_s,
        }
    }
}

/// One markout cell: a `(class, horizon)` bucket's markout distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkoutCell {
    /// Fill class of this bucket.
    pub class: FillClass,
    /// Horizon of this bucket, seconds.
    pub horizon_s: u32,
    /// Number of fills in the bucket.
    pub n: u32,
    /// Median sign-adjusted markout, bps.
    pub median_bps: i32,
    /// Mean sign-adjusted markout in fixed-point bps * 100.
    pub mean_bps_x100: i64,
}

/// Sign-adjust a raw bps move so that positive is always favorable to the
/// position: buys favor upward moves, sells favor downward moves.
fn favorable_bps(side: Side, fill_price: u64, later_price: u64) -> i64 {
    let raw = signed_bps_move(fill_price, later_price);
    match side {
        Side::Buy => raw,
        Side::Sell => -raw,
    }
}

/// Fixed-horizon markouts per fill class vs the fill reference price.
///
/// For each `(class, horizon)` bucket whose fills carry a horizon present in
/// `horizons_s`, markouts are sign-adjusted so positive is favorable, the
/// median is taken by deterministic select, and the mean is emitted in
/// fixed-point (`bps * 100`) — no floats reach the output. Empty buckets are
/// omitted entirely, never emitted as zeros. Output is ordered deterministically
/// by `(class, horizon_s)`.
pub fn markouts(fills: &[FillRow], horizons_s: &[u32]) -> Vec<MarkoutCell> {
    // Bucket sign-adjusted bps by (class, horizon), only for requested horizons.
    let mut buckets: BTreeMap<(FillClass, u32), Vec<i64>> = BTreeMap::new();

    for f in fills {
        if !horizons_s.contains(&f.horizon_s) {
            continue;
        }
        let bps = favorable_bps(f.side, f.fill_price, f.later_price);
        buckets
            .entry((f.class, f.horizon_s))
            .or_default()
            .push(bps);
    }

    let mut cells: Vec<MarkoutCell> = Vec::with_capacity(buckets.len());
    for ((class, horizon_s), mut values) in buckets {
        if values.is_empty() {
            continue; // never emit a zero cell
        }
        values.sort_unstable();
        let n = values.len();

        let median_bps = median_sorted(&values) as i32;

        let sum: i128 = values.iter().map(|&v| v as i128).sum();
        // mean * 100 in fixed point, integer division (truncates toward zero).
        let mean_bps_x100 = ((sum * 100) / n as i128) as i64;

        cells.push(MarkoutCell {
            class,
            horizon_s,
            n: n as u32,
            median_bps,
            mean_bps_x100,
        });
    }

    cells
}
