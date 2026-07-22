//! `ActiveMarketUniverse` selector (constitution §21.5, criterion 90).
//!
//! Deterministic, computationally-bounded active-market screening/qualification:
//! the pipeline that *produces* `ActiveMarketScalp`-lane candidates rather than
//! merely tagging ones some other lane already surfaced. It runs the §21.5
//! stages over reconstructed market criteria:
//!
//! 1. **broad screen** — cheap liquidity / volume / activity / breadth gates,
//! 2. **progressive filter** — stricter secondary gates (spread, holder
//!    concentration, age band),
//! 3. **deep analysis** — a composite integer priority score over the criteria,
//! 4. **reprioritization** — deterministic rank by score (tie-broken by
//!    `token_id`),
//! 5. **removal** — drop below a floor score and cap the universe to a bounded
//!    capacity (§99).
//!
//! Every survivor is stamped with `discovery_source =
//! DiscoverySource::ActiveMarketQualification`, the provenance tag introduced
//! here (no such enum/field existed anywhere before).
//!
//! # Constitution constraints (§22)
//!
//! Pure, deterministic, integer-only. Liquidity/volume are lamports (`u128`),
//! sub-scores and weights are basis points, the composite is an integer.
//! Bounded state (§99): the output is capped at `capacity`. Live market
//! reconstruction is server-side; callers feed decoded fixtures.

/// Provenance of a discovered candidate.
///
/// Responsibility: the previously-absent `discovery_source` discriminator so a
/// candidate produced by active-market qualification is attributable as such
/// (§21.5, criterion 90). Constitution §22: data only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySource {
    /// Surfaced from the launch discovery feed.
    LaunchFeed,
    /// Surfaced by a lane observation already in flight.
    LaneObservation,
    /// Produced by the §21.5 active-market qualification pipeline (this module).
    ActiveMarketQualification,
}

/// Reconstructed per-market criteria the selector screens over (§21.5).
///
/// Responsibility: the integer feature bundle a caller reconstructs from
/// market-state reducers. Constitution §22: lamports / bps / ms integers,
/// `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketObservation {
    /// Opaque token identity (mint-hash).
    pub token_id: u64,
    /// Reserve depth (quote lamports).
    pub liquidity_lamports: u128,
    /// Recent-window traded quote volume (lamports).
    pub volume_lamports_window: u128,
    /// Recent-window swap count (activity).
    pub swap_count_window: u32,
    /// Recent-window distinct trader entities (breadth).
    pub unique_traders_window: u32,
    /// Token age in milliseconds.
    pub age_ms: u64,
    /// Executable spread / round-trip impact proxy in basis points (lower is
    /// better).
    pub spread_bps: u32,
    /// Top-holder concentration in basis points (lower is better).
    pub top_holder_concentration_bps: u32,
}

/// Broad-screen + progressive-filter gate thresholds (§21.5).
///
/// Responsibility: the qualification criteria. Broad-screen fields gate cheap
/// liquidity/volume/activity/breadth; progressive fields gate spread /
/// concentration / age band. Constitution §22: integer thresholds, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenCriteria {
    /// Broad screen: minimum reserve depth (lamports).
    pub min_liquidity_lamports: u128,
    /// Broad screen: minimum recent volume (lamports).
    pub min_volume_lamports: u128,
    /// Broad screen: minimum swap count.
    pub min_swap_count: u32,
    /// Broad screen: minimum distinct traders.
    pub min_unique_traders: u32,
    /// Progressive: maximum tolerable spread (bps).
    pub max_spread_bps: u32,
    /// Progressive: maximum tolerable holder concentration (bps).
    pub max_concentration_bps: u32,
    /// Progressive: minimum token age (ms) — too-young markets are excluded.
    pub min_age_ms: u64,
    /// Progressive: maximum token age (ms) — stale markets are excluded.
    pub max_age_ms: u64,
}

/// Deep-analysis scoring weights + normalization references (§21.5).
///
/// Responsibility: turn reconstructed criteria into one composite integer
/// priority score. Each positive criterion is normalized to `0..=10_000`
/// against its reference and weighted; the two penalty criteria contribute an
/// inverted quality. Weights are bps and are expected to sum to 10_000 (they
/// are re-normalized by the `/ 10_000` divide regardless). Constitution §22:
/// integer references/weights, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalysisConfig {
    /// Liquidity that maps to a full sub-score of 10_000 (lamports).
    pub liquidity_ref_lamports: u128,
    /// Volume that maps to a full sub-score of 10_000 (lamports).
    pub volume_ref_lamports: u128,
    /// Distinct traders that map to a full sub-score of 10_000.
    pub traders_ref: u32,
    /// Swap count that maps to a full sub-score of 10_000.
    pub swaps_ref: u32,
    /// Weight (bps) on the liquidity sub-score.
    pub w_liquidity_bps: u32,
    /// Weight (bps) on the volume sub-score.
    pub w_volume_bps: u32,
    /// Weight (bps) on the breadth sub-score.
    pub w_breadth_bps: u32,
    /// Weight (bps) on the activity sub-score.
    pub w_activity_bps: u32,
    /// Weight (bps) on the (inverted) spread-quality sub-score.
    pub w_spread_bps: u32,
    /// Weight (bps) on the (inverted) concentration-quality sub-score.
    pub w_concentration_bps: u32,
}

/// Full pipeline configuration (§21.5).
///
/// Responsibility: bundle screen criteria, analysis weights, and the bounded
/// output controls (floor score + capacity, §99). Constitution §22: `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UniverseConfig {
    /// Broad-screen + progressive-filter gates.
    pub screen: ScreenCriteria,
    /// Deep-analysis scoring configuration.
    pub analysis: AnalysisConfig,
    /// Removal: minimum composite score to remain in the universe.
    pub min_priority_score: u64,
    /// Removal: maximum universe size (bounded state, §99).
    pub capacity: usize,
}

/// A candidate produced by the active-market qualification pipeline (§21.5).
///
/// Responsibility: a screened, scored, ranked, provenance-stamped candidate —
/// the output the watchlist ingest sink consumes. Constitution §22: integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualifiedCandidate {
    /// Token identity.
    pub token_id: u64,
    /// Provenance — always [`DiscoverySource::ActiveMarketQualification`] here.
    pub discovery_source: DiscoverySource,
    /// Composite priority score (higher = better).
    pub priority_score: u64,
    /// Zero-based rank after reprioritization (0 = highest priority).
    pub rank: u32,
}

/// Broad screen: cheap liquidity / volume / activity / breadth gates (§21.5).
///
/// Responsibility: stage 1 — reject obviously-inactive markets before any
/// expensive analysis. Constitution §22: integer comparisons.
#[inline]
pub fn passes_broad_screen(obs: &MarketObservation, c: &ScreenCriteria) -> bool {
    obs.liquidity_lamports >= c.min_liquidity_lamports
        && obs.volume_lamports_window >= c.min_volume_lamports
        && obs.swap_count_window >= c.min_swap_count
        && obs.unique_traders_window >= c.min_unique_traders
}

/// Progressive filter: stricter spread / concentration / age-band gates (§21.5).
///
/// Responsibility: stage 2 — reject markets that pass the broad screen but are
/// un-executable (wide spread), unsafe (concentrated), or out of the age band.
/// Constitution §22: integer comparisons.
#[inline]
pub fn passes_progressive_filter(obs: &MarketObservation, c: &ScreenCriteria) -> bool {
    obs.spread_bps <= c.max_spread_bps
        && obs.top_holder_concentration_bps <= c.max_concentration_bps
        && obs.age_ms >= c.min_age_ms
        && obs.age_ms <= c.max_age_ms
}

/// Normalize `value` against `reference` to a `0..=10_000` sub-score.
/// `value >= reference` saturates at 10_000; `reference == 0` yields 0.
#[inline]
fn sub_score(value: u128, reference: u128) -> u128 {
    if reference == 0 {
        return 0;
    }
    (value.saturating_mul(10_000) / reference).min(10_000)
}

/// Deep analysis: composite integer priority score over the criteria (§21.5).
///
/// Each positive criterion is normalized to `0..=10_000` against its reference
/// and weighted; spread and concentration contribute inverted quality
/// (`10_000 - min(bps, 10_000)`). The weighted sum is divided by 10_000, so a
/// weight set summing to 10_000 yields a score in `0..=10_000`.
///
/// Responsibility: stage 3 — the single scoring function (§21.5). Constitution
/// §22: `u128` accumulation, integer division, saturating casts.
#[inline]
pub fn analyze_candidate(obs: &MarketObservation, a: &AnalysisConfig) -> u64 {
    let liq = sub_score(obs.liquidity_lamports, a.liquidity_ref_lamports);
    let vol = sub_score(obs.volume_lamports_window, a.volume_ref_lamports);
    let breadth = sub_score(obs.unique_traders_window as u128, a.traders_ref as u128);
    let activity = sub_score(obs.swap_count_window as u128, a.swaps_ref as u128);
    let spread_quality = 10_000u128.saturating_sub((obs.spread_bps as u128).min(10_000));
    let conc_quality =
        10_000u128.saturating_sub((obs.top_holder_concentration_bps as u128).min(10_000));

    let weighted = liq * a.w_liquidity_bps as u128
        + vol * a.w_volume_bps as u128
        + breadth * a.w_breadth_bps as u128
        + activity * a.w_activity_bps as u128
        + spread_quality * a.w_spread_bps as u128
        + conc_quality * a.w_concentration_bps as u128;

    (weighted / 10_000).min(u64::MAX as u128) as u64
}

/// Reprioritize candidates in place: sort by descending score, tie-broken by
/// ascending `token_id` for determinism, then assign zero-based ranks (§21.5).
///
/// Responsibility: stage 4 — deterministic ordering + rank assignment.
/// Constitution §22: total order, no floats.
#[inline]
pub fn reprioritize(cands: &mut [QualifiedCandidate]) {
    cands.sort_by(|a, b| {
        b.priority_score
            .cmp(&a.priority_score)
            .then(a.token_id.cmp(&b.token_id))
    });
    for (i, c) in cands.iter_mut().enumerate() {
        c.rank = i as u32;
    }
}

/// Run the full §21.5 active-market qualification pipeline.
///
/// broad screen → progressive filter → deep analysis (stamp
/// `ActiveMarketQualification`) → reprioritize → remove (below floor, and
/// beyond `capacity`). Returns the bounded, ranked universe.
///
/// Responsibility: the single producer of `ActiveMarketScalp`-lane candidates
/// (§21.5, criterion 90). Constitution §22: deterministic, integer, bounded
/// output (§99).
pub fn select_active_market_universe(
    observations: &[MarketObservation],
    cfg: &UniverseConfig,
) -> Vec<QualifiedCandidate> {
    let mut qualified: Vec<QualifiedCandidate> = observations
        .iter()
        .filter(|obs| passes_broad_screen(obs, &cfg.screen))
        .filter(|obs| passes_progressive_filter(obs, &cfg.screen))
        .map(|obs| {
            let score = analyze_candidate(obs, &cfg.analysis);
            QualifiedCandidate {
                token_id: obs.token_id,
                discovery_source: DiscoverySource::ActiveMarketQualification,
                priority_score: score,
                rank: 0,
            }
        })
        // Removal (floor): drop candidates below the minimum score.
        .filter(|c| c.priority_score >= cfg.min_priority_score)
        .collect();

    reprioritize(&mut qualified);
    // Removal (capacity): bound the universe to `capacity` (§99).
    qualified.truncate(cfg.capacity);
    qualified
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis() -> AnalysisConfig {
        AnalysisConfig {
            liquidity_ref_lamports: 100_000_000_000, // 100 SOL
            volume_ref_lamports: 50_000_000_000,     // 50 SOL
            traders_ref: 100,
            swaps_ref: 500,
            w_liquidity_bps: 2_500,
            w_volume_bps: 2_500,
            w_breadth_bps: 2_000,
            w_activity_bps: 1_000,
            w_spread_bps: 1_000,
            w_concentration_bps: 1_000,
        }
    }

    fn screen() -> ScreenCriteria {
        ScreenCriteria {
            min_liquidity_lamports: 10_000_000_000, // 10 SOL
            min_volume_lamports: 5_000_000_000,     // 5 SOL
            min_swap_count: 20,
            min_unique_traders: 10,
            max_spread_bps: 300,
            max_concentration_bps: 5_000,
            min_age_ms: 60_000,
            max_age_ms: 86_400_000,
        }
    }

    fn cfg() -> UniverseConfig {
        UniverseConfig {
            screen: screen(),
            analysis: analysis(),
            min_priority_score: 1,
            capacity: 10,
        }
    }

    fn obs(id: u64) -> MarketObservation {
        // A comfortably-qualifying baseline observation.
        MarketObservation {
            token_id: id,
            liquidity_lamports: 50_000_000_000,
            volume_lamports_window: 25_000_000_000,
            swap_count_window: 250,
            unique_traders_window: 50,
            age_ms: 600_000,
            spread_bps: 100,
            top_holder_concentration_bps: 2_000,
        }
    }

    #[test]
    fn broad_screen_rejects_thin_markets() {
        let c = screen();
        let mut o = obs(1);
        o.liquidity_lamports = 1_000_000_000; // 1 SOL < 10
        assert!(!passes_broad_screen(&o, &c));
        o = obs(1);
        o.swap_count_window = 5; // < 20
        assert!(!passes_broad_screen(&o, &c));
        assert!(passes_broad_screen(&obs(1), &c));
    }

    #[test]
    fn progressive_filter_rejects_unexecutable_or_out_of_band() {
        let c = screen();
        let mut o = obs(1);
        o.spread_bps = 400; // > 300
        assert!(!passes_progressive_filter(&o, &c));
        o = obs(1);
        o.top_holder_concentration_bps = 6_000; // > 5_000
        assert!(!passes_progressive_filter(&o, &c));
        o = obs(1);
        o.age_ms = 1_000; // younger than 60s
        assert!(!passes_progressive_filter(&o, &c));
        o = obs(1);
        o.age_ms = 999_999_999; // stale
        assert!(!passes_progressive_filter(&o, &c));
        assert!(passes_progressive_filter(&obs(1), &c));
    }

    #[test]
    fn analyze_candidate_computes_expected_composite() {
        // Craft an observation with clean round sub-scores.
        // liquidity 50/100 => 5_000 ; volume 25/50 => 5_000 ;
        // traders 50/100 => 5_000 ; swaps 250/500 => 5_000 ;
        // spread 100 => quality 9_900 ; concentration 2_000 => quality 8_000.
        // weighted = 5000*2500 + 5000*2500 + 5000*2000 + 5000*1000
        //          + 9900*1000 + 8000*1000
        //   = 12_500_000 + 12_500_000 + 10_000_000 + 5_000_000
        //     + 9_900_000 + 8_000_000 = 57_900_000
        // /10_000 = 5_790.
        let score = analyze_candidate(&obs(1), &analysis());
        assert_eq!(score, 5_790);
    }

    #[test]
    fn sub_scores_saturate_and_guard_zero() {
        // value above reference saturates at 10_000.
        assert_eq!(sub_score(1_000, 10), 10_000);
        // reference zero guards to 0.
        assert_eq!(sub_score(1_000, 0), 0);
        // exact half.
        assert_eq!(sub_score(5, 10), 5_000);
    }

    #[test]
    fn pipeline_stamps_source_and_ranks_by_score() {
        // Three qualifying tokens with descending strength.
        let mut strong = obs(7);
        strong.liquidity_lamports = 100_000_000_000; // full liquidity sub-score
        strong.volume_lamports_window = 50_000_000_000;
        strong.unique_traders_window = 100;
        strong.swap_count_window = 500;
        strong.spread_bps = 0;
        strong.top_holder_concentration_bps = 0;

        let mid = obs(3);
        let mut weak = obs(9);
        weak.liquidity_lamports = 12_000_000_000;
        weak.volume_lamports_window = 6_000_000_000;
        weak.unique_traders_window = 12;
        weak.swap_count_window = 25;

        let out = select_active_market_universe(&[mid, weak, strong], &cfg());
        assert_eq!(out.len(), 3);
        // Ranked by descending score.
        assert_eq!(out[0].token_id, 7);
        assert_eq!(out[0].rank, 0);
        assert!(out[0].priority_score >= out[1].priority_score);
        assert!(out[1].priority_score >= out[2].priority_score);
        assert_eq!(out[2].rank, 2);
        for c in &out {
            assert_eq!(
                c.discovery_source,
                DiscoverySource::ActiveMarketQualification
            );
        }
    }

    #[test]
    fn pipeline_excludes_non_qualifying() {
        let mut bad = obs(1);
        bad.liquidity_lamports = 100; // fails broad screen
        let good = obs(2);
        let out = select_active_market_universe(&[bad, good], &cfg());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].token_id, 2);
    }

    #[test]
    fn removal_applies_floor_score() {
        let mut c = cfg();
        c.min_priority_score = 100_000; // impossibly high floor
        let out = select_active_market_universe(&[obs(1), obs(2)], &c);
        assert!(out.is_empty());
    }

    #[test]
    fn capacity_bounds_the_universe() {
        let mut c = cfg();
        c.capacity = 2;
        let obss: Vec<MarketObservation> = (1..=5).map(obs).collect();
        let out = select_active_market_universe(&obss, &c);
        assert_eq!(out.len(), 2); // bounded (§99)
        assert_eq!(out[0].rank, 0);
        assert_eq!(out[1].rank, 1);
    }

    #[test]
    fn reprioritize_is_deterministic_on_ties() {
        // Equal scores tie-break by ascending token_id.
        let mut cands = vec![
            QualifiedCandidate {
                token_id: 30,
                discovery_source: DiscoverySource::ActiveMarketQualification,
                priority_score: 500,
                rank: 0,
            },
            QualifiedCandidate {
                token_id: 10,
                discovery_source: DiscoverySource::ActiveMarketQualification,
                priority_score: 500,
                rank: 0,
            },
            QualifiedCandidate {
                token_id: 20,
                discovery_source: DiscoverySource::ActiveMarketQualification,
                priority_score: 900,
                rank: 0,
            },
        ];
        reprioritize(&mut cands);
        assert_eq!(cands[0].token_id, 20); // highest score
        assert_eq!(cands[1].token_id, 10); // tie: lower id first
        assert_eq!(cands[2].token_id, 30);
        assert_eq!(cands[2].rank, 2);
    }
}
