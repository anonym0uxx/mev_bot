//! **C3 — the CURVE STATE and AMM POOL STATE annotations, produced from LIVE observations.**
//!
//! # Why this module exists
//!
//! `render_curve` / `render_amm` have always been able to *print* the two reserve lines, and
//! nothing ever built the values they print. The corpus rows carry them; the live engine
//! annotated `CurveState::Absent` for every mint (the C5 harness caught it: "the
//! `CurveState::Absent` placeholder masked signal"). A bundle whose curve and pool state are
//! absent is a bundle whose model cannot see the two prices it is being asked to trade
//! against — and one whose training text it does not match.
//!
//! # The rules, and where they come from
//!
//! These reproduce `build_sft_c6.py::curve_state` and `::amm_state`, the producers of the
//! corpus rows this engine is graded against. The rules are not inventions; each carries the
//! reason it is the way it is:
//!
//! * **The LAST observation at or before the decision clock** (`bisect_right − 1`), never an
//!   interpolation: both are step functions of a discrete event stream, and interpolating a
//!   price that was never quoted would put a number in the prompt that no trade produced.
//! * **Staleness is always carried**, and `pricing_eligible = staleness <= 60_000 ms`. A
//!   snapshot older than the annotation cap (24 h) is refused outright rather than reported
//!   as a price — but a 5-minute-old snapshot is reported, and marked ineligible, because
//!   "how stale is my view" is a decision input, not an error.
//! * **`curve_price_sol_per_raw_token` is SOL per RAW token** (`vsol/1e9 / vtok`). The
//!   per-whole-token number is 1e6 larger and would read as a second, contradictory price in
//!   the same prompt; both are emitted by the corpus, each explicitly named, and this bundle
//!   line carries the raw one.
//! * **AMM attribution is by MINT only when the mint has exactly ONE pool in total.**
//!   A mint with one WSOL pool *plus* a USDC pool would otherwise accept the USDC pool's 6dp
//!   quote as WSOL — a 1000× unit error in the prompt. Multi-pool mints must present a pool
//!   that is in the authoritative WSOL set, or the line is annotated absent rather than
//!   guessed.
//! * **Out-of-order observations are dropped, not applied.** A gRPC replay can deliver an
//!   older slot after a newer one; applying it would move the annotation backwards in time
//!   while the clock moves forwards.
//!
//! # Refusal reasons are strings on purpose
//!
//! They are rendered into the prompt and parsed back by the parity harness, so an absent
//! line's *reason* is part of the contract: `mint_absent`, `no_event_before_t_dec`,
//! `older_than_annotation_cap`, `nonpositive_reserve` for the curve; plus `never_graduated`,
//! `no_amm_event_in_backfill` and `pool_not_authoritative_wsol` for the pool.

use std::collections::HashMap;

use pump_quant_proposal::decision::{AmmState, CurveState};

/// Lamports in one SOL.
pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;
/// Raw token units in one whole token (pump.fun tokens are 6dp).
pub const TOKEN_SCALE: u64 = 1_000_000;
/// Real-SOL raised at which the curve is considered graduated (85 SOL).
pub const V_GRAD_LAMPORTS: u64 = 85 * LAMPORTS_PER_SOL;
/// A snapshot older than this is refused rather than reported.
pub const ANNOTATION_CAP_MS: i64 = 24 * 3600 * 1000;
/// A snapshot no older than this may be priced against.
pub const PRICING_BUDGET_MS: i64 = 60_000;

/// One bonding-curve reserve observation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CurveObservation {
    /// Virtual SOL reserves, lamports.
    pub v_sol_lamports: u64,
    /// Virtual token reserves, raw.
    pub v_tokens: u64,
    /// Real SOL reserves, lamports.
    pub real_sol_lamports: u64,
    /// Real token reserves, raw.
    pub real_tokens: u64,
    /// Observation time, unix milliseconds.
    pub ts_ms: i64,
    /// Slot the snapshot was read at; used only to reject out-of-order replays.
    pub slot: u64,
}

/// One AMM pool reserve observation, together with what the engine knows about the pool.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AmmObservation {
    /// Pool account, base58 (or the engine's own stable rendering of it).
    pub pool: String,
    /// Base (token) reserve, raw.
    pub base_reserves_raw: u64,
    /// Quote reserve as decoded, in the quote mint's own units.
    pub quote_reserves_lamports: u64,
    /// Whether the decoded quote mint is WSOL. **A USDC-quoted pool's reserve is 6dp USDC
    /// sitting in a field named `lamports`; using it is a 1000x unit error.**
    pub quote_is_wsol: bool,
    /// Observation time, unix milliseconds.
    pub ts_ms: i64,
    /// Slot the snapshot was read at.
    pub slot: u64,
}

/// Per-mint AMM attribution context: which pools are authoritative WSOL pools for the mint,
/// and how many pools the mint has in total.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AmmAttribution {
    /// Authoritative WSOL pool accounts for this mint, in a stable order.
    pub wsol_pools: Vec<String>,
    /// Total pools known for the mint (WSOL + every other quote).
    pub pools_total: usize,
    /// Whether the mint has graduated at all (a pump-amm pool exists).
    pub graduated: bool,
}

/// The live annotation state: last observation per mint, and the AMM attribution facts.
///
/// Not a cache with an eviction policy — the *engine* owns lifetime. This holds what the
/// annotator needs and nothing else, so an audit can read the exact inputs a bundle was
/// built from.
#[derive(Debug, Default)]
pub struct AnnotationState {
    curves: HashMap<[u8; 32], CurveObservation>,
    amms: HashMap<[u8; 32], AmmObservation>,
    attribution: HashMap<[u8; 32], AmmAttribution>,
    applied_curve: u64,
    dropped_curve: u64,
    applied_amm: u64,
    dropped_amm: u64,
}

impl AnnotationState {
    /// An empty state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a curve observation. Returns `true` when it was applied.
    pub fn observe_curve(&mut self, mint: [u8; 32], obs: CurveObservation) -> bool {
        match self.curves.get(&mint) {
            Some(prev)
                if obs.ts_ms < prev.ts_ms || (obs.ts_ms == prev.ts_ms && obs.slot < prev.slot) =>
            {
                self.dropped_curve += 1;
                false
            }
            _ => {
                self.curves.insert(mint, obs);
                self.applied_curve += 1;
                true
            }
        }
    }

    /// Record an AMM observation. A non-WSOL pool is **stored** (so the mint's pool set is
    /// visible to the attribution logic) but can never be priced from.
    pub fn observe_amm(&mut self, mint: [u8; 32], obs: AmmObservation) -> bool {
        match self.amms.get(&mint) {
            Some(prev) if !obs.quote_is_wsol && prev.quote_is_wsol => {
                // Never let a non-WSOL pool displace the WSOL one we can price from.
                self.dropped_amm += 1;
                false
            }
            Some(prev) if obs.ts_ms < prev.ts_ms => {
                self.dropped_amm += 1;
                false
            }
            _ => {
                self.amms.insert(mint, obs);
                self.applied_amm += 1;
                true
            }
        }
    }

    /// Record the mint's pool attribution facts (authoritative WSOL set + total pools).
    pub fn set_attribution(&mut self, mint: [u8; 32], attr: AmmAttribution) {
        self.attribution.insert(mint, attr);
    }

    /// Observation/drop counters, for telemetry and for tests that assert replays are dropped.
    #[must_use]
    pub fn counters(&self) -> (u64, u64, u64, u64) {
        (
            self.applied_curve,
            self.dropped_curve,
            self.applied_amm,
            self.dropped_amm,
        )
    }

    /// The last curve observation for a mint, if any.
    #[must_use]
    pub fn curve_of(&self, mint: &[u8; 32]) -> Option<&CurveObservation> {
        self.curves.get(mint)
    }

    /// The last AMM observation for a mint, if any.
    #[must_use]
    pub fn amm_of(&self, mint: &[u8; 32]) -> Option<&AmmObservation> {
        self.amms.get(mint)
    }

    /// Build the `CurveState` for `t_dec_ms`.
    #[must_use]
    pub fn curve_state(&self, mint: &[u8; 32], t_dec_ms: i64) -> CurveState {
        let Some(obs) = self.curves.get(mint) else {
            return CurveState::Absent {
                reason: "mint_absent".to_string(),
            };
        };
        if obs.ts_ms > t_dec_ms {
            return CurveState::Absent {
                reason: "no_event_before_t_dec".to_string(),
            };
        }
        let staleness_ms = t_dec_ms - obs.ts_ms;
        if staleness_ms > ANNOTATION_CAP_MS {
            return CurveState::Absent {
                reason: "older_than_annotation_cap".to_string(),
            };
        }
        if obs.v_sol_lamports == 0 || obs.v_tokens == 0 {
            return CurveState::Absent {
                reason: "nonpositive_reserve".to_string(),
            };
        }
        let px_lamports_per_raw = obs.v_sol_lamports as f64 / obs.v_tokens as f64;
        let progress = (obs.real_sol_lamports as f64 / V_GRAD_LAMPORTS as f64).clamp(0.0, 1.0);
        CurveState::Present {
            staleness_ms,
            pricing_eligible: staleness_ms <= PRICING_BUDGET_MS,
            v_sol_reserves_lamports: i64_or_saturate(obs.v_sol_lamports),
            v_tokens_reserves: i64_or_saturate(obs.v_tokens),
            real_sol_reserves_lamports: i64_or_saturate(obs.real_sol_lamports),
            real_tokens_reserves: i64_or_saturate(obs.real_tokens),
            curve_price_sol_per_raw_token: px_lamports_per_raw / LAMPORTS_PER_SOL as f64,
            curve_k: (u128::from(obs.v_sol_lamports) * u128::from(obs.v_tokens)).to_string(),
            curve_progress: progress,
            curve_regime: if obs.real_sol_lamports >= V_GRAD_LAMPORTS {
                "graduated".to_string()
            } else {
                "bonding".to_string()
            },
        }
    }

    /// Build the `AmmState` for `t_dec_ms`.
    ///
    /// Attribution, in the order the corpus applies it:
    /// 1. no attribution at all / not graduated → `never_graduated`;
    /// 2. a graduated mint with no observation → `no_amm_event_in_backfill`;
    /// 3. a snapshot-only-in-the-future mint → `no_event_before_t_dec`;
    /// 4. older than the cap → `older_than_annotation_cap`;
    /// 5. one pool in total and exactly one authoritative WSOL pool → attribute by MINT
    ///    (safe: there is no other pool to confuse it with);
    /// 6. otherwise the observation's pool must BE authoritative → else
    ///    `pool_not_authoritative_wsol`;
    /// 7. non-positive reserves → `nonpositive_reserve`.
    #[must_use]
    pub fn amm_state(&self, mint: &[u8; 32], t_dec_ms: i64) -> AmmState {
        let absent = |r: &str| AmmState::Absent {
            reason: r.to_string(),
        };
        let Some(attr) = self.attribution.get(mint) else {
            return absent("never_graduated");
        };
        if !attr.graduated || attr.wsol_pools.is_empty() {
            return absent("never_graduated");
        }
        let Some(obs) = self.amms.get(mint) else {
            return absent("no_amm_event_in_backfill");
        };
        if obs.ts_ms > t_dec_ms {
            return absent("no_event_before_t_dec");
        }
        let staleness_ms = t_dec_ms - obs.ts_ms;
        if staleness_ms > ANNOTATION_CAP_MS {
            return absent("older_than_annotation_cap");
        }
        let sole_pool_in_total = attr.pools_total == 1;
        let pool_used = if sole_pool_in_total && attr.wsol_pools.len() == 1 {
            attr.wsol_pools[0].clone()
        } else if attr.wsol_pools.iter().any(|p| p == &obs.pool) {
            obs.pool.clone()
        } else {
            return absent("pool_not_authoritative_wsol");
        };
        if obs.base_reserves_raw == 0 || obs.quote_reserves_lamports == 0 {
            return absent("nonpositive_reserve");
        }
        if !obs.quote_is_wsol {
            // Defence in depth: the attribution says WSOL, the observation says otherwise.
            return absent("pool_not_authoritative_wsol");
        }
        let px_lamports_per_raw = obs.quote_reserves_lamports as f64 / obs.base_reserves_raw as f64;
        AmmState::Present {
            pool: pool_used,
            staleness_ms,
            pricing_eligible: staleness_ms <= PRICING_BUDGET_MS,
            base_reserves_raw: i64_or_saturate(obs.base_reserves_raw),
            quote_reserves_lamports: i64_or_saturate(obs.quote_reserves_lamports),
            amm_price_sol_per_raw_token: px_lamports_per_raw / LAMPORTS_PER_SOL as f64,
            reserve_slot: Some(i64_or_saturate(obs.slot)),
        }
    }
}

/// Clamp a u64 reserve into the i64 the bundle's fields use, rather than wrapping.
///
/// A reserve above i64::MAX is not a price; it is a decode failure, and saturating makes it
/// visible as a large number instead of a negative one.
fn i64_or_saturate(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn obs(vsol: u64, vtok: u64, rsol: u64, rtok: u64, ts: i64, slot: u64) -> CurveObservation {
        CurveObservation {
            v_sol_lamports: vsol,
            v_tokens: vtok,
            real_sol_lamports: rsol,
            real_tokens: rtok,
            ts_ms: ts,
            slot,
        }
    }

    #[test]
    fn curve_matches_the_corpus_rules() {
        let mut st = AnnotationState::new();
        // 30 SOL virtual / 1.073e15 raw is the pump.fun initialisation.
        st.observe_curve(
            mint(1),
            obs(
                30 * LAMPORTS_PER_SOL,
                1_073_000_000_000_000,
                0,
                793_100_000_000_000,
                1_000,
                7,
            ),
        );
        match st.curve_state(&mint(1), 1_500) {
            CurveState::Present {
                staleness_ms,
                pricing_eligible,
                curve_price_sol_per_raw_token,
                curve_k,
                curve_progress,
                curve_regime,
                ..
            } => {
                assert_eq!(staleness_ms, 500, "staleness is t_dec - t_obs");
                assert!(pricing_eligible, "500 ms is inside the 60 s pricing budget");
                assert!(
                    (curve_price_sol_per_raw_token - 30.0 / 1_073_000_000_000_000.0 * 1e9 * 1e-9)
                        .abs()
                        < 1e-18
                );
                assert_eq!(
                    curve_k,
                    (30_000_000_000u128 * 1_073_000_000_000_000u128).to_string()
                );
                assert!((curve_progress - 0.0).abs() < 1e-12);
                assert_eq!(curve_regime, "bonding");
            }
            other => panic!("expected present, got {other:?}"),
        }
    }

    #[test]
    fn curve_staleness_is_reported_before_it_is_refused() {
        let mut st = AnnotationState::new();
        st.observe_curve(mint(2), obs(1_000, 1_000, 0, 0, 0, 1));
        // 5 minutes old: reported, ineligible (the corpus's own contract).
        match st.curve_state(&mint(2), 300_000) {
            CurveState::Present {
                staleness_ms,
                pricing_eligible,
                ..
            } => {
                assert_eq!(staleness_ms, 300_000);
                assert!(!pricing_eligible);
            }
            other => panic!("expected present, got {other:?}"),
        }
        // Past the 24 h cap: refused, with the reason the corpus uses.
        match st.curve_state(&mint(2), ANNOTATION_CAP_MS + 1) {
            CurveState::Absent { reason } => assert_eq!(reason, "older_than_annotation_cap"),
            other => panic!("expected absent, got {other:?}"),
        }
    }

    #[test]
    fn curve_absent_reasons_are_the_corpus_dialect() {
        let mut st = AnnotationState::new();
        assert_eq!(
            st.curve_state(&mint(3), 10),
            CurveState::Absent {
                reason: "mint_absent".to_string()
            }
        );
        st.observe_curve(mint(3), obs(5, 5, 0, 0, 100, 1));
        match st.curve_state(&mint(3), 50) {
            CurveState::Absent { reason } => assert_eq!(reason, "no_event_before_t_dec"),
            other => panic!("expected absent, got {other:?}"),
        }
        st.observe_curve(mint(4), obs(0, 5, 0, 0, 1, 1));
        match st.curve_state(&mint(4), 10) {
            CurveState::Absent { reason } => assert_eq!(reason, "nonpositive_reserve"),
            other => panic!("expected absent, got {other:?}"),
        }
    }

    #[test]
    fn out_of_order_observations_never_move_the_annotation_backwards() {
        let mut st = AnnotationState::new();
        assert!(st.observe_curve(mint(5), obs(100, 100, 0, 0, 5_000, 20)));
        assert!(
            !st.observe_curve(mint(5), obs(90, 110, 0, 0, 4_000, 19)),
            "older ts is dropped"
        );
        assert!(
            !st.observe_curve(mint(5), obs(90, 110, 0, 0, 5_000, 19)),
            "same ts, older slot is dropped"
        );
        match st.curve_state(&mint(5), 5_100) {
            CurveState::Present {
                v_sol_reserves_lamports,
                ..
            } => assert_eq!(v_sol_reserves_lamports, 100),
            other => panic!("expected present, got {other:?}"),
        }
        let (applied, dropped, _, _) = st.counters();
        assert_eq!((applied, dropped), (1, 2));
    }

    fn amm(base: u64, quote: u64, ts: i64, pool: &str, wsol: bool) -> AmmObservation {
        AmmObservation {
            pool: pool.to_string(),
            base_reserves_raw: base,
            quote_reserves_lamports: quote,
            quote_is_wsol: wsol,
            ts_ms: ts,
            slot: 42,
        }
    }

    #[test]
    fn amm_attribution_by_mint_requires_one_pool_in_total() {
        let mut st = AnnotationState::new();
        st.set_attribution(
            mint(6),
            AmmAttribution {
                wsol_pools: vec!["P1".into()],
                pools_total: 1,
                graduated: true,
            },
        );
        st.observe_amm(mint(6), amm(1_000_000, 2_000_000_000, 10_000, "P1", true));
        match st.amm_state(&mint(6), 10_500) {
            AmmState::Present {
                pool,
                staleness_ms,
                quote_reserves_lamports,
                reserve_slot,
                ..
            } => {
                assert_eq!(pool, "P1");
                assert_eq!(staleness_ms, 500);
                assert_eq!(quote_reserves_lamports, 2_000_000_000);
                assert_eq!(reserve_slot, Some(42));
            }
            other => panic!("expected present, got {other:?}"),
        }
    }

    #[test]
    fn a_multi_pool_mint_refuses_a_non_authoritative_pool_rather_than_guessing() {
        let mut st = AnnotationState::new();
        // One WSOL pool AND another pool: attribution by mint alone is no longer safe.
        st.set_attribution(
            mint(7),
            AmmAttribution {
                wsol_pools: vec!["WSOLP".into()],
                pools_total: 2,
                graduated: true,
            },
        );
        st.observe_amm(mint(7), amm(1_000_000, 2_000_000, 10_000, "USDCP", false));
        match st.amm_state(&mint(7), 10_500) {
            AmmState::Absent { reason } => assert_eq!(reason, "pool_not_authoritative_wsol"),
            other => panic!("expected absent, got {other:?}"),
        }
        // The authoritative WSOL pool is accepted.
        st.observe_amm(mint(7), amm(1_000_000, 2_000_000, 11_000, "WSOLP", true));
        match st.amm_state(&mint(7), 11_500) {
            AmmState::Present { pool, .. } => assert_eq!(pool, "WSOLP"),
            other => panic!("expected present, got {other:?}"),
        }
    }

    #[test]
    fn a_non_wsol_observation_cannot_displace_the_wsol_one() {
        let mut st = AnnotationState::new();
        st.set_attribution(
            mint(8),
            AmmAttribution {
                wsol_pools: vec!["W".into()],
                pools_total: 2,
                graduated: true,
            },
        );
        assert!(st.observe_amm(mint(8), amm(1_000_000, 2_000_000_000, 10_000, "W", true)));
        assert!(
            !st.observe_amm(mint(8), amm(1_000_000, 1_000_000, 12_000, "U", false)),
            "a newer USDC-pool row must not replace the WSOL view"
        );
        match st.amm_state(&mint(8), 12_500) {
            AmmState::Present {
                quote_reserves_lamports,
                ..
            } => assert_eq!(quote_reserves_lamports, 2_000_000_000),
            other => panic!("expected the WSOL view, got {other:?}"),
        }
    }

    #[test]
    fn amm_absent_reasons_are_the_corpus_dialect() {
        let mut st = AnnotationState::new();
        match st.amm_state(&mint(9), 10) {
            AmmState::Absent { reason } => assert_eq!(reason, "never_graduated"),
            other => panic!("expected absent, got {other:?}"),
        }
        st.set_attribution(
            mint(9),
            AmmAttribution {
                wsol_pools: vec!["W".into()],
                pools_total: 1,
                graduated: true,
            },
        );
        match st.amm_state(&mint(9), 10) {
            AmmState::Absent { reason } => assert_eq!(reason, "no_amm_event_in_backfill"),
            other => panic!("expected absent, got {other:?}"),
        }
        st.observe_amm(mint(9), amm(10, 10, 9_000, "W", true));
        match st.amm_state(&mint(9), 5_000) {
            AmmState::Absent { reason } => assert_eq!(reason, "no_event_before_t_dec"),
            other => panic!("expected absent, got {other:?}"),
        }
        match st.amm_state(&mint(9), ANNOTATION_CAP_MS + 10_000) {
            AmmState::Absent { reason } => assert_eq!(reason, "older_than_annotation_cap"),
            other => panic!("expected absent, got {other:?}"),
        }
        st.observe_amm(mint(9), amm(0, 10, 20_000, "W", true));
        match st.amm_state(&mint(9), 20_100) {
            AmmState::Absent { reason } => assert_eq!(reason, "nonpositive_reserve"),
            other => panic!("expected absent, got {other:?}"),
        }
    }
}
