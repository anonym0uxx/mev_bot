//! # screen — flow-authenticity law (§21.7) + smart-money follow screen (§28)
//!
//! Two admission screens the engine consults before a candidate can size up:
//!
//! * [`FlowScreen`] — **entity-deduplicated flow authenticity**. Raw volume on
//!   pump.fun is an adversarial quantity: MemeTrans measures **21.4% ambient
//!   wash volume** and **36.5% bundled supply at launch** as the *baseline*
//!   condition, so volume is discounted, never trusted (§21.7 "flow
//!   authenticity is a spectrum, not a boolean"). The screen folds every
//!   decoded swap into a bounded per-mint entity-flow table and derives
//!   round-trip (matched) volume, concentration, and a reduce-only
//!   authenticity multiplier. The only *discovery gate* it may emit is the
//!   hard fabrication signature (§21.7: soft evidence haircuts size; only a
//!   fabrication signature may drop a candidate).
//! * [`WalletScreen`] — the §28 **follower-executable smart-money verdict**.
//!   A wallet is followable only if its family-netted realized PnL survives
//!   the truth/luck screen *and* a lagged shadow of its entries — at this
//!   system's latency (base and stress), size, exit policy, and costs — beats
//!   an activity-matched control. Both are wired verbatim from
//!   `pump_quant_wallet_graph::smart_money`.
//! * [`creator_credibility_haircut_bp`] — the §27/§70.9 deployer-credibility
//!   fold: serial deployers and heavy-prior creators earn a reduce-only size
//!   haircut, never a boost.
//!
//! ## Determinism & bounds
//! Integer-only, no wall-clock, no RNG (§22). Every table is capacity-bounded
//! with deterministic smallest-gross / oldest-activity eviction (§99). The
//! authenticity derivation is `O(ring)` over at most
//! [`MAX_ENTITIES_PER_MINT`] entries and runs per-admit (warm path); `record`
//! is `O(log mints + ring)` per decoded swap.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use pump_quant_ingest::social_parse::fnv1a_64;
use pump_quant_wallet_graph::smart_money::{
    classify_smart_money, lagged_shadow, LaggedShadowResult, Legibility, PnlScreen,
    PnlScreenConfig, PnlScreenResult, PriceOracle, ShadowConfig, Trade, WalletAction,
    WalletQualityState,
};
use pump_quant_wallet_graph::{FamilyId, TokenId, PRICE_SCALE};

// ===========================================================================================
// Named constants (§102 — every threshold carries its provenance)
// ===========================================================================================

/// Basis-point scale (100% == 10 000 bps) shared by every ratio here (§22).
const BPS: u64 = 10_000;

/// Maximum entities tracked per mint. 64 covers the entire flagged cohort of a
/// typical pump.fun launch (MemeTrans: bundle + sniper rings are tens of
/// wallets, not hundreds); smaller flows than the 64 largest cannot move the
/// wash/concentration ratios materially (§99 bounded state).
pub const MAX_ENTITIES_PER_MINT: usize = 64;

/// Maximum mints tracked by [`FlowScreen`]. 4096 exceeds the live watch
/// universe by an order of magnitude while keeping worst-case memory ~6 MiB
/// (§99). Eviction is smallest-gross: dust mints yield to flowing ones.
pub const MAX_TRACKED_MINTS: usize = 4096;

/// Minimum recorded swaps before authenticity is computed rather than assumed.
/// Below this the matched-volume and concentration ratios are sample noise (a
/// single organic round trip reads as 100% wash), so the neutral prior is
/// returned instead (§6.4: label thin evidence, never fabricate certainty).
pub const MIN_SWAPS_FOR_AUTH: u32 = 16;

/// Neutral authenticity prior (bps) below the swap-sample floor: mildly
/// discounted from perfect because the *baseline* pump.fun tape already
/// carries 21.4% ambient wash volume (MemeTrans), but far above the floor —
/// thin evidence is not evidence of fabrication.
pub const NEUTRAL_PRIOR_BPS: u32 = 8_000;

/// Round-trip (matched) volume tolerance in bps. MemeTrans' 21.4% ambient
/// wash share means matched volume up to ~20% of gross is indistinguishable
/// from the venue baseline; only the excess above this is penalized.
pub const WASH_TOLERANCE_BPS: u64 = 2_000;

/// Penalty multiplier on excess round-trip volume. Wash volume is charged
/// double because it corrupts *both* sides of the tape (it manufactures the
/// buy pressure and the exit-liquidity illusion — Victor & Weintraud
/// net-position-cycle wash accounting; NBER w30783 wash-trade prevalence).
pub const WASH_PENALTY_MULT: u64 = 2;

/// Concentration (HHI, bps) tolerance. An organic early tape of ~8 equal
/// participants scores HHI ≈ 1 250; penalizing only above 1 500 leaves normal
/// launch concentration unpunished while single-operator tapes (HHI → 10 000)
/// are cut hard.
pub const HHI_TOLERANCE_BPS: u64 = 1_500;

/// Authenticity floor (bps). Even a maximally wash-signed tape retains a
/// small multiplier rather than zero: the *gate* decision belongs to the
/// fabrication signature and the engine, not to a silent zero-size (§21.7
/// haircut-not-veto separation).
pub const AUTH_FLOOR_BPS: u32 = 2_500;

/// Authenticity ceiling (bps) — a tape can never earn a size *boost* from
/// authenticity; the screen is reduce-only (§29 fade-first).
pub const AUTH_CEIL_BPS: u32 = 10_000;

/// Hard fabrication signature: matched volume ≥ 60% of gross. Triple the
/// MemeTrans ambient baseline and beyond any organic churn pattern; at this
/// level the tape is being *printed*, not traded (NBER w30783 signatures).
pub const FABRICATED_RT_BPS: u64 = 6_000;

/// Hard fabrication signature: a single entity owns ≥ 35% of gross flow.
/// Combined with the round-trip term this is the bundled-operator print
/// (MemeTrans 36.5% bundled supply; Victor–Weintraud cycles return flow to
/// its origin entity). Both legs must fire — this is the ONLY discovery gate
/// this screen emits.
pub const FABRICATED_TOP1_BPS: u64 = 3_500;

/// Phase weight (bps) applied to the authenticity haircut while on the curve.
/// Curve exit cost is analytic from decoded curve state (§21.7 phase
/// asymmetry), so fabricated flow distorts the exit less — the haircut is
/// applied at half strength.
pub const PHASE_W_CURVE_BPS: u64 = 5_000;

/// Phase weight (bps) for pool-phase markets: exit realizability there
/// depends on the very flow being judged, so the haircut applies in full.
pub const PHASE_W_POOL_BPS: u64 = 10_000;

// --- WalletScreen (§28 research-fixed shadow parameters) ---

/// Base follower latency in slots (observe + decide + submit on a healthy
/// leader schedule). §28: the shadow must run at *this system's* latency.
pub const BASE_LAG_SLOTS: u64 = 3;

/// Stress follower latency in slots (congested leader / retry path). A wallet
/// is followable only if the edge survives BOTH latencies — edges that die
/// between 3 and 8 slots are insider timing, not followable skill (§28).
pub const STRESS_LAG_SLOTS: u64 = 8;

/// Shadow holding horizon in slots (~2 minutes at 400 ms/slot) — this
/// system's scalp max-hold, per the follower-executable law.
pub const SHADOW_HORIZON_SLOTS: u64 = 300;

/// Shadow take-profit, bps above entry (this system's exit policy).
pub const SHADOW_TP_BPS: u32 = 3_000;

/// Shadow stop-loss, bps below entry (this system's exit policy; wider than
/// TP because memecoin drawdown noise exceeds run-up persistence).
pub const SHADOW_SL_BPS: u32 = 3_500;

/// Shadow position size, lamports (0.1 SOL — this system's probe clip; §28
/// demands simulation at OUR size, never the whale's).
pub const SHADOW_SIZE_LAMPORTS: u64 = 100_000_000;

/// Round-trip fee bps charged on each shadow leg (venue fee ≈ 1%).
pub const SHADOW_FEE_BPS: u32 = 100;

/// Flat priority-tip lamports per shadow leg (0.001 SOL, the landing tip this
/// system actually pays).
pub const SHADOW_TIP_LAMPORTS: u64 = 1_000_000;

/// Minimum wallet entry actions before any followable verdict — the §28
/// skill-vs-luck sample floor at the action level; below it every verdict is
/// `false` (never "unknown-but-followed").
pub const MIN_FOLLOW_ACTIONS: usize = 40;

/// Control-cohort shift in slots: the activity-matched control replays the
/// same tokens and cadence displaced +50 slots (~20 s), stripping the
/// wallet's timing while keeping its token selection (§46 placebo cohort).
pub const CONTROL_SHIFT_SLOTS: u64 = 50;

/// Minimum distinct realized tokens for the PnL truth screen — one jackpot is
/// not skill (§28 luck screen; five realized names is the smallest sample
/// where top-removed PnL is meaningful).
pub const PNL_MIN_TOKENS: u32 = 5;

/// Maximum wallets tracked (§99). 128 exceeds the count of simultaneously
/// credible smart-money candidates by a wide margin.
pub const MAX_WALLETS: usize = 128;

/// Maximum stored trades per wallet (§99). 256 trades at scalp cadence spans
/// far more history than the 300-slot shadow horizon needs.
pub const MAX_TRADES_PER_WALLET: usize = 256;

// --- Deployer credibility (§27 / §70.9) ---

/// Baseline credibility multiplier: no haircut (bps).
pub const CREDIBILITY_BASELINE_BPS: u32 = 10_000;

/// Serial-deployer multiplier (bps): a creator spraying launches inside the
/// window is running a lottery funnel, not a project (§27 serial-deploy flag;
/// §70.9 recycle detection) — 40% haircut.
pub const SERIAL_DEPLOYER_MULT_BPS: u32 = 6_000;

/// Lifetime prior-launch count at which the heavy-prior haircut applies.
/// §70.9: ten prior CAs is the recycle-farm signature, not a builder résumé.
pub const HEAVY_PRIOR_LAUNCHES: u32 = 10;

/// Heavy-prior multiplier (bps): 20% haircut for creators with
/// [`HEAVY_PRIOR_LAUNCHES`]+ lifetime launches (§27).
pub const HEAVY_PRIOR_MULT_BPS: u32 = 8_000;

/// Floor on the combined credibility multiplier (bps): credibility is a
/// haircut input, never a veto — hard exclusion belongs to the rug-cluster
/// screens, not this fold (§27).
pub const CREDIBILITY_FLOOR_BPS: u32 = 5_000;

// ===========================================================================================
// FlowScreen — §21.7 entity-dedup authenticity
// ===========================================================================================

/// Per-entity flow accumulator inside one mint's table.
#[derive(Clone, Copy, Debug)]
struct EntityFlow {
    /// Upstream entity-deduplicated actor id (§28: one real actor, one id).
    entity: u64,
    /// Lamports spent buying (saturating).
    buy_lamports: u64,
    /// Lamports received selling (saturating).
    sell_lamports: u64,
}

impl EntityFlow {
    /// Gross flow attributed to this entity (saturating).
    fn gross(&self) -> u64 {
        self.buy_lamports.saturating_add(self.sell_lamports)
    }
}

/// Per-mint bounded entity-flow table.
#[derive(Clone, Debug, Default)]
struct MintFlow {
    /// Tracked entities, at most [`MAX_ENTITIES_PER_MINT`], insertion order.
    entities: Vec<EntityFlow>,
    /// Σ gross over tracked entities (kept in sync incrementally, saturating).
    gross_total: u64,
    /// Total `record` calls for this mint, including dropped-dust
    /// observations (saturating) — the sample-floor counter.
    recorded_swaps: u32,
}

/// §21.7 entity-deduplicated flow-authenticity screen.
///
/// Folds decoded swaps into bounded per-mint entity tables and derives, per
/// mint: matched (round-trip) volume, gross flow, concentration (HHI), and
/// top-entity share — from which the reduce-only authenticity multiplier and
/// the hard fabrication signature follow. Deterministic, integer-only,
/// bounded (§22/§99): identical event sequences produce identical verdicts.
#[derive(Clone, Debug, Default)]
pub struct FlowScreen {
    /// Per-mint tables, keyed by mint bytes (deterministic order).
    mints: BTreeMap<[u8; 32], MintFlow>,
    /// Smallest-gross eviction index: `(gross_total, mint)` kept in lockstep
    /// with `mints` so mint eviction is `O(log n)`, never a scan.
    by_gross: BTreeSet<(u64, [u8; 32])>,
}

impl FlowScreen {
    /// New empty screen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one decoded swap into the per-mint entity-flow table.
    ///
    /// Bounds (§99), all deterministic:
    /// * per mint ≤ [`MAX_ENTITIES_PER_MINT`] entities — a new entity beyond
    ///   the bound evicts the smallest-gross entry (tie: smallest entity id)
    ///   only if the incoming swap outweighs it, else the observation is
    ///   dropped from the table (still counted in the swap sample);
    /// * ≤ [`MAX_TRACKED_MINTS`] mints — a new mint beyond the bound evicts
    ///   the smallest-gross mint (tie: smallest key) only if the incoming
    ///   swap outweighs it, else the observation is dropped.
    ///
    /// The table is thereby a lower-bound sketch biased toward the largest
    /// flows — exactly the flows a wash operation must use to matter (§21.7).
    pub fn record(&mut self, mint: &[u8; 32], entity: u64, is_buy: bool, quote_lamports: u64) {
        if let Some(flow) = self.mints.get_mut(mint) {
            let old_gross = flow.gross_total;
            Self::fold_into(flow, entity, is_buy, quote_lamports);
            let new_gross = flow.gross_total;
            if new_gross != old_gross {
                self.by_gross.remove(&(old_gross, *mint));
                self.by_gross.insert((new_gross, *mint));
            }
            return;
        }
        // New mint: admit within capacity, else displace the smallest-gross
        // mint only when the incoming flow is strictly larger than its entire
        // recorded gross (deterministic; dust can never displace real flow).
        if self.mints.len() >= MAX_TRACKED_MINTS {
            match self.by_gross.first().copied() {
                Some((min_gross, min_key)) if quote_lamports > min_gross => {
                    self.by_gross.remove(&(min_gross, min_key));
                    self.mints.remove(&min_key);
                }
                _ => return, // incoming flow does not outweigh any tracked mint
            }
        }
        let mut flow = MintFlow::default();
        Self::fold_into(&mut flow, entity, is_buy, quote_lamports);
        self.by_gross.insert((flow.gross_total, *mint));
        self.mints.insert(*mint, flow);
    }

    /// Fold one swap into a mint table (entity upsert + bounded eviction).
    fn fold_into(flow: &mut MintFlow, entity: u64, is_buy: bool, quote_lamports: u64) {
        flow.recorded_swaps = flow.recorded_swaps.saturating_add(1);
        if let Some(e) = flow.entities.iter_mut().find(|e| e.entity == entity) {
            if is_buy {
                e.buy_lamports = e.buy_lamports.saturating_add(quote_lamports);
            } else {
                e.sell_lamports = e.sell_lamports.saturating_add(quote_lamports);
            }
            flow.gross_total = flow.gross_total.saturating_add(quote_lamports);
            return;
        }
        if flow.entities.len() >= MAX_ENTITIES_PER_MINT {
            // Smallest-gross entity, tie-broken by smallest entity id — a
            // pure function of table contents, so eviction is deterministic.
            let min_idx = flow
                .entities
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| (e.gross(), e.entity))
                .map(|(i, _)| i);
            match min_idx {
                Some(i) if quote_lamports > flow.entities[i].gross() => {
                    flow.gross_total = flow.gross_total.saturating_sub(flow.entities[i].gross());
                    flow.entities.swap_remove(i);
                }
                _ => return, // dust: sample counted above, flow not tracked
            }
        }
        let (buy_lamports, sell_lamports) = if is_buy {
            (quote_lamports, 0)
        } else {
            (0, quote_lamports)
        };
        flow.entities.push(EntityFlow {
            entity,
            buy_lamports,
            sell_lamports,
        });
        flow.gross_total = flow.gross_total.saturating_add(quote_lamports);
    }

    /// `(auth_bps, fabricated)` — raw authenticity in `0..=10_000` plus the
    /// hard fabrication signature (§21.7).
    ///
    /// Derivation over the entity ring (`O(ring)`, warm path):
    /// * matched volume `M = Σ 2·min(buy_e, sell_e)` (Victor–Weintraud
    ///   net-position-cycle accounting: flow that returned to its origin);
    /// * gross `G = Σ (buy_e + sell_e)`; round-trip share
    ///   `rt_bps = 10_000·M/G`;
    /// * concentration `hhi_bps = Σ share_e_bps² / 10_000`; `top1_bps` = max
    ///   entity share;
    /// * `auth = clamp(10_000 − 2·max(0, rt − 2_000) − max(0, hhi − 1_500),
    ///   2_500, 10_000)`.
    ///
    /// Below [`MIN_SWAPS_FOR_AUTH`] recorded swaps (or an untracked /
    /// zero-gross mint) the neutral prior `(8_000, false)` is returned — thin
    /// samples neither convict nor absolve (§6.4). The fabrication signature
    /// (`rt ≥ 6_000` AND `top1 ≥ 3_500`) is the ONLY discovery gate this
    /// screen emits; everything else is a haircut.
    /// Whether this mint's authenticity is EVIDENCED rather than assumed: it is
    /// tracked, has at least [`MIN_SWAPS_FOR_AUTH`] recorded swaps, and nonzero
    /// gross flow. The neutral prior returned below that floor is a label for
    /// thin evidence (§6.4) — callers that ADD risk on authenticity (the §33
    /// scale-in) must require this alongside the bps bar, so absence of
    /// evidence can never read as confirmation.
    #[must_use]
    pub fn has_auth_evidence(&self, mint: &[u8; 32]) -> bool {
        self.mints
            .get(mint)
            .is_some_and(|f| f.recorded_swaps >= MIN_SWAPS_FOR_AUTH && f.gross_total > 0)
    }

    #[must_use]
    pub fn authenticity(&self, mint: &[u8; 32]) -> (u32, bool) {
        let Some(flow) = self.mints.get(mint) else {
            return (NEUTRAL_PRIOR_BPS, false);
        };
        if flow.recorded_swaps < MIN_SWAPS_FOR_AUTH {
            return (NEUTRAL_PRIOR_BPS, false);
        }
        let gross = u128::from(flow.gross_total);
        if gross == 0 {
            return (NEUTRAL_PRIOR_BPS, false);
        }
        let mut matched: u128 = 0;
        let mut hhi_acc: u128 = 0;
        let mut top1_bps: u64 = 0;
        for e in &flow.entities {
            let m = u128::from(e.buy_lamports.min(e.sell_lamports)) * 2;
            matched = matched.saturating_add(m);
            let share_wide = u128::from(e.gross()).saturating_mul(u128::from(BPS)) / gross;
            let share_bps = u64::try_from(share_wide.min(u128::from(BPS))).unwrap_or(BPS);
            hhi_acc = hhi_acc.saturating_add(u128::from(share_bps) * u128::from(share_bps));
            top1_bps = top1_bps.max(share_bps);
        }
        let rt_wide = matched.saturating_mul(u128::from(BPS)) / gross;
        let rt_bps = u64::try_from(rt_wide.min(u128::from(BPS))).unwrap_or(BPS);
        let hhi_bps =
            u64::try_from((hhi_acc / u128::from(BPS)).min(u128::from(BPS))).unwrap_or(BPS);

        let wash_excess = rt_bps.saturating_sub(WASH_TOLERANCE_BPS);
        let hhi_excess = hhi_bps.saturating_sub(HHI_TOLERANCE_BPS);
        let penalty = wash_excess
            .saturating_mul(WASH_PENALTY_MULT)
            .saturating_add(hhi_excess);
        let auth_i =
            i64::try_from(BPS).unwrap_or(i64::MAX) - i64::try_from(penalty).unwrap_or(i64::MAX);
        let auth = auth_i.clamp(i64::from(AUTH_FLOOR_BPS), i64::from(AUTH_CEIL_BPS));
        let auth_bps = u32::try_from(auth).unwrap_or(AUTH_FLOOR_BPS);

        let fabricated = rt_bps >= FABRICATED_RT_BPS && top1_bps >= FABRICATED_TOP1_BPS;
        (auth_bps, fabricated)
    }

    /// Reduce-only size multiplier (bps ≤ 10 000) for the market's phase.
    ///
    /// `mult = 10_000 − (10_000 − auth) · phase_w / 10_000` with
    /// [`PHASE_W_CURVE_BPS`] on the curve and [`PHASE_W_POOL_BPS`] in the
    /// pool: curve exits are analytically priced from decoded curve state, so
    /// inauthentic flow is haircut at half strength there; pool exit
    /// realizability rides on the judged flow itself, so the haircut is full
    /// (§21.7 phase asymmetry).
    #[must_use]
    pub fn size_mult_bp(&self, mint: &[u8; 32], is_pool: bool) -> u32 {
        let (auth_bps, _) = self.authenticity(mint);
        let phase_w = if is_pool {
            PHASE_W_POOL_BPS
        } else {
            PHASE_W_CURVE_BPS
        };
        let haircut =
            u64::from(AUTH_CEIL_BPS.saturating_sub(auth_bps)).saturating_mul(phase_w) / BPS;
        AUTH_CEIL_BPS.saturating_sub(u32::try_from(haircut).unwrap_or(AUTH_CEIL_BPS))
    }

    /// Test-only: number of tracked mints (bound audit).
    #[cfg(test)]
    fn tracked_mints(&self) -> usize {
        self.mints.len()
    }

    /// Test-only: number of tracked entities for a mint (bound audit).
    #[cfg(test)]
    fn entities_of(&self, mint: &[u8; 32]) -> usize {
        self.mints.get(mint).map_or(0, |f| f.entities.len())
    }
}

// ===========================================================================================
// WalletScreen — §28 smart-money follow screen
// ===========================================================================================

/// One stored wallet-trade observation.
#[derive(Clone, Copy, Debug)]
struct StoredTrade {
    /// Mint bytes (kept verbatim so the price callback can be consulted).
    mint: [u8; 32],
    /// Buy (`true`) or sell (`false`).
    is_buy: bool,
    /// Lamports spent (buy) or received (sell) — executable proceeds.
    sol_lamports: u64,
    /// Slot / logical tick of the observation.
    slot: u64,
}

/// Bounded per-wallet history.
#[derive(Clone, Debug, Default)]
struct WalletHist {
    /// Oldest-first trade ring, ≤ [`MAX_TRADES_PER_WALLET`].
    trades: VecDeque<StoredTrade>,
    /// Most recent observed slot (recency key for wallet eviction).
    last_slot: u64,
}

/// Adapter presenting the engine's `price_of(mint, slot)` callback as the
/// wallet-graph crate's [`PriceOracle`], with the `TokenId → mint` reverse
/// map built from the candidate wallet's own stored trades. Prices must
/// already be scaled by the wallet-graph crate's [`PRICE_SCALE`] (1e6 —
/// scaled lamports per token base unit).
struct FnOracle<'a> {
    /// The engine's deterministic price series.
    price_of: &'a dyn Fn(&[u8; 32], u64) -> Option<u64>,
    /// FNV-1a-64 token id → mint bytes, for reverse lookup.
    token_of: &'a BTreeMap<u64, [u8; 32]>,
}

impl PriceOracle for FnOracle<'_> {
    fn price_scaled(&self, token: TokenId, slot: u64) -> Option<u64> {
        let mint = self.token_of.get(&token.0)?;
        (self.price_of)(mint, slot)
    }
}

/// §28 smart-money follow screen: bounded wallet-trade history + the
/// follower-executable verdict (PnL truth screen + double lagged shadow).
///
/// The engine records wallet trades off its decoded event stream and asks
/// [`WalletScreen::followable`] at admit time. Deterministic and bounded
/// (§22/§99): ≤ [`MAX_WALLETS`] wallets × ≤ [`MAX_TRADES_PER_WALLET`] trades,
/// oldest-evicted.
#[derive(Clone, Debug, Default)]
pub struct WalletScreen {
    /// Per-wallet bounded histories, keyed by entity id (deterministic order).
    wallets: BTreeMap<u64, WalletHist>,
}

impl WalletScreen {
    /// New empty screen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one wallet trade observation (wallet entity id, token mint,
    /// buy?, sol size, slot/tick).
    ///
    /// Bounds (§99): per wallet the oldest trade is dropped beyond
    /// [`MAX_TRADES_PER_WALLET`]; beyond [`MAX_WALLETS`] the wallet with the
    /// oldest `last_slot` (tie: smallest wallet id) is evicted — recency
    /// wins, deterministically. The eviction scan is bounded by
    /// [`MAX_WALLETS`].
    pub fn record(
        &mut self,
        wallet: u64,
        mint: &[u8; 32],
        is_buy: bool,
        sol_lamports: u64,
        slot: u64,
    ) {
        if !self.wallets.contains_key(&wallet) && self.wallets.len() >= MAX_WALLETS {
            // Oldest-activity eviction over a ≤128-entry map: bounded scan.
            let oldest = self
                .wallets
                .iter()
                .map(|(&w, h)| (h.last_slot, w))
                .min()
                .map(|(_, w)| w);
            if let Some(w) = oldest {
                self.wallets.remove(&w);
            }
        }
        let hist = self.wallets.entry(wallet).or_default();
        hist.trades.push_back(StoredTrade {
            mint: *mint,
            is_buy,
            sol_lamports,
            slot,
        });
        if hist.trades.len() > MAX_TRADES_PER_WALLET {
            hist.trades.pop_front();
        }
        hist.last_slot = hist.last_slot.max(slot);
    }

    /// Follower-executable verdict for `wallet` (§28).
    ///
    /// Pipeline (all `pump_quant_wallet_graph::smart_money`, wired verbatim):
    /// 1. Rebuild family-level [`Trade`]s from the stored history:
    ///    `family = FamilyId(wallet)`, `token = TokenId(fnv1a_64(mint))`,
    ///    `self_dealt = false` (v0 — no funding-graph feed yet). Trade
    ///    `units` are recovered from the executable price series at the trade
    ///    slot (`sol · PRICE_SCALE / price`); when no price is observable the
    ///    raw sol-lamports proxy is used for that leg. (The pure sol proxy is
    ///    degenerate under family netting — prorated cost ≡ prorated
    ///    proceeds, so realized PnL would be identically zero and no wallet
    ///    could ever pass; price-recovered units restore real cost-basis
    ///    accounting.)
    /// 2. [`PnlScreen`] with [`PNL_MIN_TOKENS`] — truth + luck screen.
    /// 3. DOUBLE [`lagged_shadow`] at [`BASE_LAG_SLOTS`] and
    ///    [`STRESS_LAG_SLOTS`] against a control of the same wallet's actions
    ///    shifted +[`CONTROL_SHIFT_SLOTS`] slots; BOTH must beat control.
    /// 4. [`classify_smart_money`] under the v0 [`Legibility::PublicBurned`]
    ///    presumption (every observed wallet is treated as publicly legible
    ///    until private-discovery provenance is wired) with
    ///    `bait_suspect = false`; the wallet is followable iff both
    ///    classifications land in a followable terminal state — every §28
    ///    hard-fail state (bait, self-dealing, luck concentration, insider
    ///    timing, insufficient sample) returns `false`.
    ///
    /// Fewer than [`MIN_FOLLOW_ACTIONS`] recorded entry actions → `false`
    /// (not-followed, never "unknown-but-followed"). `price_of(mint_key,
    /// slot)` supplies fixed-point prices scaled by [`PRICE_SCALE`] (1e6, per
    /// the wallet-graph crate) — scale inputs accordingly. Deterministic
    /// given the same history and price series (§22). Warm path: cost is
    /// `O(trades × horizon)` price lookups, run per-admit only.
    #[must_use]
    pub fn followable(
        &self,
        wallet: u64,
        price_of: &dyn Fn(&[u8; 32], u64) -> Option<u64>,
    ) -> bool {
        let Some(hist) = self.wallets.get(&wallet) else {
            return false;
        };
        let mut token_of: BTreeMap<u64, [u8; 32]> = BTreeMap::new();
        let mut trades: Vec<Trade> = Vec::with_capacity(hist.trades.len());
        let mut actions: Vec<WalletAction> = Vec::new();
        for t in &hist.trades {
            let tid = fnv1a_64(&t.mint);
            token_of.insert(tid, t.mint);
            let units = match price_of(&t.mint, t.slot) {
                Some(p) if p > 0 => {
                    let u = u128::from(t.sol_lamports).saturating_mul(PRICE_SCALE) / u128::from(p);
                    u64::try_from(u).unwrap_or(u64::MAX)
                }
                // No observable price: degrade to the sol proxy for this leg.
                _ => t.sol_lamports,
            };
            trades.push(Trade {
                family: FamilyId(wallet),
                token: TokenId(tid),
                is_buy: t.is_buy,
                units,
                sol_lamports: t.sol_lamports,
                self_dealt: false,
                slot: t.slot,
            });
            if t.is_buy {
                actions.push(WalletAction {
                    token: TokenId(tid),
                    action_slot: t.slot,
                });
            }
        }
        if actions.len() < MIN_FOLLOW_ACTIONS {
            return false;
        }
        // Activity-matched control: same tokens, same cadence, timing
        // displaced +CONTROL_SHIFT_SLOTS — the wallet's timing is the only
        // thing removed (§46 placebo construction).
        let control: Vec<WalletAction> = actions
            .iter()
            .map(|a| WalletAction {
                token: a.token,
                action_slot: a.action_slot.saturating_add(CONTROL_SHIFT_SLOTS),
            })
            .collect();
        let oracle = FnOracle {
            price_of,
            token_of: &token_of,
        };
        let pnl = PnlScreen::new(PnlScreenConfig {
            min_tokens: PNL_MIN_TOKENS,
        })
        .evaluate(&trades);
        let base = lagged_shadow(
            &oracle,
            &actions,
            &control,
            &Self::shadow_cfg(BASE_LAG_SLOTS),
        );
        let stress = lagged_shadow(
            &oracle,
            &actions,
            &control,
            &Self::shadow_cfg(STRESS_LAG_SLOTS),
        );
        Self::verdict(&pnl, &base) && Self::verdict(&pnl, &stress)
    }

    /// Number of wallets currently tracked (≤ [`MAX_WALLETS`]).
    #[must_use]
    pub fn tracked_wallets(&self) -> usize {
        self.wallets.len()
    }

    /// This system's shadow configuration at the given latency (§28: the
    /// follower's latency/size/policy/costs, never the watched wallet's).
    fn shadow_cfg(latency_slots: u64) -> ShadowConfig {
        ShadowConfig {
            latency_slots,
            horizon_slots: SHADOW_HORIZON_SLOTS,
            tp_bps: SHADOW_TP_BPS,
            sl_bps: SHADOW_SL_BPS,
            size_lamports: SHADOW_SIZE_LAMPORTS,
            fee_bps: SHADOW_FEE_BPS,
            tip_lamports: SHADOW_TIP_LAMPORTS,
        }
    }

    /// Map one classification to the boolean follow verdict: only the
    /// terminal states reachable by passing BOTH §28 gates count; every
    /// hard-fail state is `false`. (Under the v0 PUBLIC_BURNED presumption a
    /// passing wallet classifies as `PublicBurned`; the crowding discount for
    /// burned-legibility wallets is the engine's sizing concern, not a veto
    /// here.)
    fn verdict(pnl: &PnlScreenResult, shadow: &LaggedShadowResult) -> bool {
        matches!(
            classify_smart_money(pnl, shadow, Legibility::PublicBurned, false),
            WalletQualityState::SmartMoneyFollowable
                | WalletQualityState::PreLegibilityCandidate
                | WalletQualityState::PublicBurned
        )
    }

    /// Test-only: stored trade count for a wallet (bound audit).
    #[cfg(test)]
    fn trades_of(&self, wallet: u64) -> usize {
        self.wallets.get(&wallet).map_or(0, |h| h.trades.len())
    }
}

// ===========================================================================================
// Deployer credibility (§27 / §70.9)
// ===========================================================================================

/// Reduce-only deployer-credibility size multiplier in bps (§27, §70.9).
///
/// * Baseline [`CREDIBILITY_BASELINE_BPS`] (no haircut).
/// * `launches_in_window ≥ serial_threshold` → ×[`SERIAL_DEPLOYER_MULT_BPS`]
///   (serial-deploy lottery funnel, §27 serial flag).
/// * `prior_launches ≥` [`HEAVY_PRIOR_LAUNCHES`] → ×[`HEAVY_PRIOR_MULT_BPS`]
///   (recycle-farm prior, §70.9 prior-CA count).
/// * Combined multiplicatively, floored at [`CREDIBILITY_FLOOR_BPS`] —
///   credibility haircuts size; it never vetoes (hard exclusion belongs to
///   the rug-cluster screens) and never boosts.
///
/// Pure and deterministic (§22). A `serial_threshold` of zero marks every
/// deployer serial by definition (degenerate but well-defined).
#[must_use]
pub fn creator_credibility_haircut_bp(
    prior_launches: u32,
    launches_in_window: u32,
    serial_threshold: u32,
) -> u32 {
    let mut mult: u64 = u64::from(CREDIBILITY_BASELINE_BPS);
    if launches_in_window >= serial_threshold {
        mult = mult * u64::from(SERIAL_DEPLOYER_MULT_BPS) / BPS;
    }
    if prior_launches >= HEAVY_PRIOR_LAUNCHES {
        mult = mult * u64::from(HEAVY_PRIOR_MULT_BPS) / BPS;
    }
    let mult = u32::try_from(mult).unwrap_or(CREDIBILITY_BASELINE_BPS);
    mult.max(CREDIBILITY_FLOOR_BPS)
}

// ===========================================================================================
// Tests
// ===========================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic mint key from an index.
    fn mk(i: u32) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[..4].copy_from_slice(&i.to_be_bytes());
        k
    }

    // ---- FlowScreen ----

    #[test]
    fn authenticity_neutral_prior_below_sample_then_computed() {
        let mut s = FlowScreen::new();
        let m = mk(1);
        // 15 swaps of a pure single-entity round trip: still neutral.
        for i in 0..15u64 {
            s.record(&m, 7, i % 2 == 0, 1_000);
        }
        assert_eq!(s.authenticity(&m), (NEUTRAL_PRIOR_BPS, false));
        // Untracked mint is also neutral.
        assert_eq!(s.authenticity(&mk(999)), (NEUTRAL_PRIOR_BPS, false));
        // 16th swap crosses the floor: single-entity wash tape -> clamped
        // floor + fabrication signature (rt = 10_000, top1 = 10_000).
        s.record(&m, 7, false, 1_000);
        assert_eq!(s.authenticity(&m), (AUTH_FLOOR_BPS, true));
    }

    #[test]
    fn authenticity_formula_midpoint_and_phase_weighting() {
        let mut s = FlowScreen::new();
        let m = mk(2);
        // 4 entities round-trip 1_000 each (buy+sell); 12 entities buy 1_000.
        // M = 8_000, G = 20_000 -> rt = 4_000 (excess 2_000 -> penalty 4_000).
        // Shares: 4 x 1_000bps, 12 x 500bps -> HHI = 700 (no penalty).
        for e in 0..4u64 {
            s.record(&m, e, true, 1_000);
            s.record(&m, e, false, 1_000);
        }
        for e in 4..16u64 {
            s.record(&m, e, true, 1_000);
        }
        let (auth, fab) = s.authenticity(&m);
        assert_eq!(auth, 6_000);
        assert!(!fab, "rt 4_000 < 6_000 must not trip the hard signature");
        // Phase weighting: curve haircut at half strength, pool in full.
        assert_eq!(s.size_mult_bp(&m, false), 8_000);
        assert_eq!(s.size_mult_bp(&m, true), 6_000);
        // Clean organic tape: full multiplier in both phases.
        let organic = mk(3);
        for e in 0..20u64 {
            s.record(&organic, e, true, 1_000_000);
        }
        assert_eq!(s.authenticity(&organic), (AUTH_CEIL_BPS, false));
        assert_eq!(s.size_mult_bp(&organic, false), AUTH_CEIL_BPS);
        assert_eq!(s.size_mult_bp(&organic, true), AUTH_CEIL_BPS);
    }

    #[test]
    fn flow_screen_bounds_and_smallest_gross_eviction() {
        let mut s = FlowScreen::new();
        // Entity bound: 64 tracked, then a whale displaces the smallest.
        let m = mk(4);
        for e in 0..MAX_ENTITIES_PER_MINT as u64 {
            s.record(&m, e, true, 1_000 + e); // entity 0 has smallest gross
        }
        assert_eq!(s.entities_of(&m), MAX_ENTITIES_PER_MINT);
        s.record(&m, 10_000, true, 5_000_000); // outweighs min -> evicts
        assert_eq!(s.entities_of(&m), MAX_ENTITIES_PER_MINT);
        s.record(&m, 10_001, true, 1); // dust: dropped, table unchanged
        assert_eq!(s.entities_of(&m), MAX_ENTITIES_PER_MINT);

        // Mint bound: fill to cap with fabricated tapes, displace smallest.
        let mut s = FlowScreen::new();
        for i in 0..MAX_TRACKED_MINTS as u32 {
            let m = mk(i);
            for j in 0..16u64 {
                // Single-entity round trip, per-mint gross ascending with i.
                s.record(&m, 1, j % 2 == 0, u64::from(i + 1) * 100);
            }
        }
        assert_eq!(s.tracked_mints(), MAX_TRACKED_MINTS);
        assert_eq!(s.authenticity(&mk(0)), (AUTH_FLOOR_BPS, true));
        // New heavy mint outweighs mint 0's whole gross -> mint 0 evicted.
        s.record(&mk(1_000_000), 1, true, u64::MAX / 4);
        assert_eq!(s.tracked_mints(), MAX_TRACKED_MINTS);
        assert_eq!(s.authenticity(&mk(0)), (NEUTRAL_PRIOR_BPS, false));
        assert_eq!(s.authenticity(&mk(1)), (AUTH_FLOOR_BPS, true));
        // Dust mint cannot displace anyone.
        s.record(&mk(2_000_000), 1, true, 1);
        assert_eq!(s.tracked_mints(), MAX_TRACKED_MINTS);
        assert_eq!(s.authenticity(&mk(2_000_000)), (NEUTRAL_PRIOR_BPS, false));
    }

    #[test]
    fn flow_screen_is_deterministic() {
        let feed = |s: &mut FlowScreen| {
            for i in 0..500u64 {
                let mint = mk(u32::try_from(i % 7).unwrap());
                s.record(&mint, i % 11, i % 3 == 0, 1_000 + i * 13);
            }
        };
        let (mut a, mut b) = (FlowScreen::new(), FlowScreen::new());
        feed(&mut a);
        feed(&mut b);
        for i in 0..7u32 {
            assert_eq!(a.authenticity(&mk(i)), b.authenticity(&mk(i)));
            assert_eq!(a.size_mult_bp(&mk(i), false), b.size_mult_bp(&mk(i), false));
            assert_eq!(a.size_mult_bp(&mk(i), true), b.size_mult_bp(&mk(i), true));
        }
    }

    // ---- WalletScreen ----

    /// Price series for the happy path: per token-k window keyed off the buy
    /// slot `1_000·k`: flat at 1.0 (scaled 1e6) for 10 slots, then a +40%
    /// pop through slot +40 (the wallet sells at +30; the follower's TP
    /// fires), then -10% stale through +400 (the shifted control enters here
    /// and bleeds fees flat). Deterministic pure function.
    fn happy_price(mint: &[u8; 32], slot: u64) -> Option<u64> {
        let mut kb = [0u8; 4];
        kb.copy_from_slice(&mint[..4]);
        let base = u64::from(u32::from_be_bytes(kb)) * 1_000;
        let d = slot.checked_sub(base)?;
        match d {
            0..=10 => Some(1_000_000),
            11..=40 => Some(1_400_000),
            41..=400 => Some(900_000),
            _ => None,
        }
    }

    /// Record `n` profitable round trips (buy at +0, sell at +30) on `n`
    /// distinct tokens for one wallet.
    fn feed_round_trips(s: &mut WalletScreen, wallet: u64, n: u32) {
        for k in 0..n {
            let m = mk(k);
            let t0 = u64::from(k) * 1_000;
            s.record(wallet, &m, true, 100_000_000, t0);
            // Sold at 1.4x: proceeds consistent with the price pop.
            s.record(wallet, &m, false, 140_000_000, t0 + 30);
        }
    }

    #[test]
    fn followable_true_end_to_end_and_deterministic() {
        let mut s = WalletScreen::new();
        feed_round_trips(&mut s, 42, u32::try_from(MIN_FOLLOW_ACTIONS).unwrap());
        assert!(s.followable(42, &happy_price));
        // Same inputs, fresh instance: identical verdict (§22).
        let mut s2 = WalletScreen::new();
        feed_round_trips(&mut s2, 42, u32::try_from(MIN_FOLLOW_ACTIONS).unwrap());
        assert!(s2.followable(42, &happy_price));
        // Unknown wallet is never followable.
        assert!(!s.followable(7, &happy_price));
    }

    #[test]
    fn followable_false_below_min_sample() {
        let mut s = WalletScreen::new();
        // 39 actions: one below the floor; generous prices cannot rescue it.
        feed_round_trips(&mut s, 42, u32::try_from(MIN_FOLLOW_ACTIONS).unwrap() - 1);
        assert!(!s.followable(42, &happy_price));
    }

    #[test]
    fn followable_false_when_edge_not_replicable_at_lag() {
        // Prices where the pop happens before the follower can act: the
        // wallet's own realized PnL is real (bought 1.0, sold 1.4), but a
        // follower at +3/+8 slots enters at the 1.4 top and is stopped out in
        // the collapse -> insider timing, not followable (§28).
        let insider_price = |mint: &[u8; 32], slot: u64| -> Option<u64> {
            let mut kb = [0u8; 4];
            kb.copy_from_slice(&mint[..4]);
            let base = u64::from(u32::from_be_bytes(kb)) * 1_000;
            let d = slot.checked_sub(base)?;
            match d {
                0..=2 => Some(1_000_000),  // wallet's entry price
                3..=30 => Some(1_400_000), // wallet's exit level; follower entry
                31..=400 => Some(500_000), // followers dumped on
                _ => None,
            }
        };
        let mut s = WalletScreen::new();
        feed_round_trips(&mut s, 42, u32::try_from(MIN_FOLLOW_ACTIONS).unwrap());
        assert!(!s.followable(42, &insider_price));
    }

    #[test]
    fn wallet_screen_bounds_oldest_eviction() {
        let mut s = WalletScreen::new();
        // Per-wallet trade-ring bound.
        for i in 0..300u64 {
            s.record(1, &mk(0), true, 1_000, i);
        }
        assert_eq!(s.trades_of(1), MAX_TRADES_PER_WALLET);
        // Wallet-count bound: the wallet with the oldest activity is evicted.
        let mut s = WalletScreen::new();
        for w in 0..=(MAX_WALLETS as u64) {
            s.record(w, &mk(0), true, 1_000, w); // wallet 0 is oldest
        }
        assert_eq!(s.tracked_wallets(), MAX_WALLETS);
        assert_eq!(s.trades_of(0), 0, "oldest-activity wallet must be evicted");
        assert_eq!(s.trades_of(MAX_WALLETS as u64), 1);
    }

    // ---- Deployer credibility ----

    #[test]
    fn creator_credibility_haircut_composition_and_floor() {
        // Baseline: clean deployer.
        assert_eq!(creator_credibility_haircut_bp(0, 0, 3), 10_000);
        assert_eq!(creator_credibility_haircut_bp(9, 2, 3), 10_000);
        // Serial only.
        assert_eq!(creator_credibility_haircut_bp(0, 3, 3), 6_000);
        // Heavy prior only.
        assert_eq!(creator_credibility_haircut_bp(10, 0, 3), 8_000);
        // Both: 6_000 * 8_000 / 10_000 = 4_800 -> floored at 5_000.
        assert_eq!(creator_credibility_haircut_bp(25, 5, 3), 5_000);
    }
}
