//! Section 28 **smart-money authentication**.
//!
//! On-chain "profitability" is an adversarial, manufactured quantity by
//! default. This module implements the two gates the constitution requires
//! before any wallet may be treated as "smart":
//!
//! 1. [`PnlScreen`] — the **PnL truth rules** and **skill-vs-luck statistics**:
//!    realized (never marked), executable-proceeds, external-counterparty PnL
//!    netted at the operator-family level, with self-dealing excluded, a
//!    minimum-sample floor, and a top-trade-removed concentration screen.
//! 2. [`lagged_shadow`] — the **follower-executable PnL law**, *the only
//!    admissible definition of smart money*: simulate entering at this system's
//!    observation + decision + execution latency after the wallet acted,
//!    exiting under this system's own policy, at this system's size, with full
//!    costs, and compare against an activity-matched control cohort. Insider
//!    timing, bait sequences, and self-dealt pumps fail this mechanically.
//!
//! [`classify_smart_money`] combines both gates with the copy-bait / legibility
//! screens into a [`WalletQualityState`].
//!
//! All arithmetic is integer / fixed-point (Section 22). The external price
//! series is modelled behind [`PriceOracle`]; this crate never implements or
//! calls a live oracle.

use crate::{mul_div_u128, FamilyId, TokenId, BPS_DENOM, PRICE_SCALE};

/// One realized-or-partial trade by a wallet's operator family in a token.
///
/// SOL amounts are unsigned lamports (`sol_lamports` = SOL spent on a buy, or
/// SOL received on a sell); direction is carried by `is_buy`. Trades are
/// aggregated at the family level so that intra-family transfers and wash
/// cycles cancel (Section 28 rule (c)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trade {
    /// Operator family that executed the trade.
    pub family: FamilyId,
    /// Token traded.
    pub token: TokenId,
    /// `true` for a buy, `false` for a sell.
    pub is_buy: bool,
    /// Token base units transacted.
    pub units: u64,
    /// Lamports spent (buy) or received (sell) — executable proceeds, never a
    /// displayed/marked price.
    pub sol_lamports: u64,
    /// Whether this token was launched / funded / bundled by the wallet's own
    /// family (self-dealing per Section 28 rule (d)).
    pub self_dealt: bool,
    /// Slot of the trade (for ordering / recency; not used in the realized-PnL
    /// arithmetic itself).
    pub slot: u64,
}

/// Configuration for [`PnlScreen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PnlScreenConfig {
    /// Minimum number of distinct non-self-dealt tokens with realized activity
    /// required before any positive classification (skill-vs-luck sample floor).
    pub min_tokens: u32,
}

/// Distinct PnL components (never collapsed into one opaque score, Section 28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnlComponents {
    /// Family-netted realized PnL over non-self-dealt tokens (lamports, signed).
    pub total_realized: i128,
    /// Realized PnL with the single most-profitable token removed
    /// (concentration screen: "one jackpot is not skill").
    pub top_removed_realized: i128,
    /// Largest single-token realized PnL.
    pub top_token_pnl: i128,
    /// Number of non-self-dealt tokens with realized activity.
    pub token_count: u32,
    /// Number of those tokens that were realized-profitable.
    pub profitable_token_count: u32,
    /// Number of tokens excluded as self-dealing.
    pub self_dealt_token_count: u32,
    /// Realized PnL on the excluded self-dealt tokens (reported separately,
    /// never counted as skill).
    pub self_dealt_realized: i128,
}

/// Reason a wallet fails the PnL screen (`None` = passes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PnlFailReason {
    /// Passes all PnL-screen requirements.
    None,
    /// Fewer than `min_tokens` non-self-dealt realized tokens.
    InsufficientSample,
    /// Positive profit came from self-dealt tokens.
    SelfDealing,
    /// Profit is concentrated in a single token (top-removed PnL <= 0).
    LuckyConcentrated,
    /// Family-netted realized PnL is not positive.
    NonPositiveRealized,
}

/// Result of the PnL screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnlScreenResult {
    /// Distinct PnL components.
    pub components: PnlComponents,
    /// Failure reason (or `None`).
    pub reason: PnlFailReason,
}

impl PnlScreenResult {
    /// Whether the wallet passed the PnL truth + luck screen and is eligible to
    /// proceed to the follower-executable lagged-shadow test.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.reason == PnlFailReason::None
    }
}

/// The Section 28 PnL truth + skill-vs-luck screen.
#[derive(Debug, Clone, Copy)]
pub struct PnlScreen {
    cfg: PnlScreenConfig,
}

impl PnlScreen {
    /// Create a screen with the given configuration.
    #[must_use]
    pub fn new(cfg: PnlScreenConfig) -> Self {
        Self { cfg }
    }

    /// Evaluate a wallet's family-level trades.
    ///
    /// Per-token realized PnL uses a cost-basis netting: for each token the
    /// realized quantity is `min(bought_units, sold_units)`; the realized cost
    /// is the buy cost pro-rated to that quantity, and the realized proceeds
    /// are the sell proceeds pro-rated to that quantity. All arithmetic widens
    /// through `u128` and results are `i128` lamports.
    #[must_use]
    pub fn evaluate(&self, trades: &[Trade]) -> PnlScreenResult {
        // Aggregate per token: (buy_units, buy_cost, sell_units, sell_proceeds,
        // self_dealt).
        #[derive(Default, Clone, Copy)]
        struct Agg {
            buy_units: u128,
            buy_cost: u128,
            sell_units: u128,
            sell_proceeds: u128,
            self_dealt: bool,
        }
        // Deterministic ordering by token id.
        let mut tokens: std::collections::BTreeMap<u64, Agg> = std::collections::BTreeMap::new();
        for t in trades {
            let e = tokens.entry(t.token.0).or_default();
            if t.self_dealt {
                e.self_dealt = true;
            }
            if t.is_buy {
                e.buy_units = e.buy_units.saturating_add(u128::from(t.units));
                e.buy_cost = e.buy_cost.saturating_add(u128::from(t.sol_lamports));
            } else {
                e.sell_units = e.sell_units.saturating_add(u128::from(t.units));
                e.sell_proceeds = e.sell_proceeds.saturating_add(u128::from(t.sol_lamports));
            }
        }

        let mut total_realized: i128 = 0;
        let mut top_token_pnl: i128 = i128::MIN;
        let mut token_count: u32 = 0;
        let mut profitable_token_count: u32 = 0;
        let mut self_dealt_token_count: u32 = 0;
        let mut self_dealt_realized: i128 = 0;

        for agg in tokens.values() {
            // A token is realized only if it was both bought and sold.
            if agg.buy_units == 0 || agg.sell_units == 0 {
                continue;
            }
            let realized_units = agg.buy_units.min(agg.sell_units);
            // Pro-rated cost of the realized units.
            let cost_of_sold =
                mul_div_u128(agg.buy_cost, realized_units, agg.buy_units).unwrap_or(0);
            // Pro-rated proceeds for the realized units.
            let proceeds_of_sold =
                mul_div_u128(agg.sell_proceeds, realized_units, agg.sell_units).unwrap_or(0);
            let pnl = proceeds_of_sold as i128 - cost_of_sold as i128;

            if agg.self_dealt {
                self_dealt_token_count = self_dealt_token_count.saturating_add(1);
                self_dealt_realized = self_dealt_realized.saturating_add(pnl);
                continue; // excluded from skill PnL
            }

            token_count = token_count.saturating_add(1);
            total_realized = total_realized.saturating_add(pnl);
            if pnl > 0 {
                profitable_token_count = profitable_token_count.saturating_add(1);
            }
            if pnl > top_token_pnl {
                top_token_pnl = pnl;
            }
        }

        let top_token_pnl = if token_count == 0 { 0 } else { top_token_pnl };
        let top_removed_realized = total_realized - top_token_pnl;

        let components = PnlComponents {
            total_realized,
            top_removed_realized,
            top_token_pnl,
            token_count,
            profitable_token_count,
            self_dealt_token_count,
            self_dealt_realized,
        };

        // Classification order matters: sample floor first, then self-dealing,
        // then sign, then concentration.
        let reason = if token_count < self.cfg.min_tokens {
            PnlFailReason::InsufficientSample
        } else if self_dealt_token_count > 0 && self_dealt_realized > 0 {
            PnlFailReason::SelfDealing
        } else if total_realized <= 0 {
            PnlFailReason::NonPositiveRealized
        } else if top_removed_realized <= 0 {
            PnlFailReason::LuckyConcentrated
        } else {
            PnlFailReason::None
        };

        PnlScreenResult { components, reason }
    }
}

/// A price series behind which any I/O is hidden. The lagged-shadow simulator
/// reads scaled prices from this trait; this crate never implements a live one
/// (Section 22 determinism: model I/O behind a trait, never call it).
pub trait PriceOracle {
    /// Executable price of `token` at `slot`, as scaled lamports per token base
    /// unit (`lamports_per_unit * PRICE_SCALE`). `None` if unknown at that slot.
    fn price_scaled(&self, token: TokenId, slot: u64) -> Option<u64>;
}

/// A wallet action to shadow (the wallet's own entry into a token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalletAction {
    /// Token the wallet entered.
    pub token: TokenId,
    /// Slot at which the wallet acted.
    pub action_slot: u64,
}

/// Configuration for the follower-executable lagged-shadow simulation. These
/// are *this system's* latency, size, exit policy, and costs — never the
/// watched wallet's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowConfig {
    /// Observation + decision + execution latency, in slots, before the
    /// follower can act after the wallet.
    pub latency_slots: u64,
    /// Maximum holding horizon, in slots, after the follower's entry.
    pub horizon_slots: u64,
    /// Take-profit threshold in basis points above entry.
    pub tp_bps: u32,
    /// Stop-loss threshold in basis points below entry.
    pub sl_bps: u32,
    /// Position size in lamports.
    pub size_lamports: u64,
    /// Round-trip fee applied to both entry and exit notionals, in bps.
    pub fee_bps: u32,
    /// Flat priority/tip lamports paid on each of entry and exit.
    pub tip_lamports: u64,
}

/// Aggregate result of a lagged-shadow comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaggedShadowResult {
    /// Summed follower-executable net PnL over the wallet's actions (lamports).
    pub wallet_net: i128,
    /// Summed follower-executable net PnL over the activity-matched control
    /// actions (lamports).
    pub control_net: i128,
    /// Number of wallet actions that produced a simulated round trip.
    pub wallet_actions_simulated: u32,
    /// Number of wallet actions skipped (no price at entry, or no exit price).
    pub wallet_actions_skipped: u32,
}

impl LaggedShadowResult {
    /// Whether the wallet's lagged shadow is *followable*: strictly positive
    /// and strictly better than the activity-matched control cohort. This is
    /// the follower-executable PnL law's pass condition.
    #[must_use]
    pub fn is_followable(&self) -> bool {
        self.wallet_net > 0 && self.wallet_net > self.control_net
    }
}

/// Simulate a single follower-executable round trip for one action. Returns the
/// net PnL in lamports, or `None` if the action cannot be shadowed (missing
/// entry price, zero entry price, or no exit price within the horizon).
#[must_use]
pub fn simulate_action(
    oracle: &dyn PriceOracle,
    action: WalletAction,
    cfg: &ShadowConfig,
) -> Option<i128> {
    let entry_slot = action.action_slot.saturating_add(cfg.latency_slots);
    let entry_price = u128::from(oracle.price_scaled(action.token, entry_slot)?);
    if entry_price == 0 {
        return None;
    }

    // Units bought = size_lamports * PRICE_SCALE / entry_price (base units).
    let size = u128::from(cfg.size_lamports);
    let units = mul_div_u128(size, PRICE_SCALE, entry_price)?;

    // TP/SL price thresholds (scaled lamports/unit).
    let tp_price = mul_div_u128(
        entry_price,
        u128::from(BPS_DENOM) + u128::from(cfg.tp_bps),
        u128::from(BPS_DENOM),
    )?;
    let sl_price = mul_div_u128(
        entry_price,
        u128::from(BPS_DENOM).saturating_sub(u128::from(cfg.sl_bps)),
        u128::from(BPS_DENOM),
    )?;

    // Walk the horizon; exit on first TP/SL touch, else last observed price.
    let mut exit_price: Option<u128> = None;
    let last_slot = entry_slot.saturating_add(cfg.horizon_slots);
    let mut s = entry_slot.saturating_add(1);
    while s <= last_slot {
        if let Some(p) = oracle.price_scaled(action.token, s) {
            let p = u128::from(p);
            exit_price = Some(p); // remember most recent observed price
            if p >= tp_price || p <= sl_price {
                exit_price = Some(p);
                break;
            }
        }
        s = s.saturating_add(1);
    }
    let exit_price = exit_price?;

    // Proceeds = units * exit_price / PRICE_SCALE.
    let proceeds = mul_div_u128(units, exit_price, PRICE_SCALE)?;

    // Costs: fee on entry notional (size) and exit notional (proceeds), plus a
    // flat tip on each leg.
    let entry_fee = mul_div_u128(size, u128::from(cfg.fee_bps), u128::from(BPS_DENOM))?;
    let exit_fee = mul_div_u128(proceeds, u128::from(cfg.fee_bps), u128::from(BPS_DENOM))?;
    let tips = u128::from(cfg.tip_lamports).saturating_mul(2);

    let gross = proceeds as i128 - size as i128;
    let net = gross - entry_fee as i128 - exit_fee as i128 - tips as i128;
    Some(net)
}

/// Run the follower-executable lagged-shadow comparison for a candidate wallet
/// against an activity-matched control cohort.
///
/// Both the wallet's actions and the control actions are shadowed with the
/// *same* [`ShadowConfig`] (this system's latency, size, policy, and costs).
/// The comparison is the pass condition of the follower-executable PnL law.
#[must_use]
pub fn lagged_shadow(
    oracle: &dyn PriceOracle,
    wallet_actions: &[WalletAction],
    control_actions: &[WalletAction],
    cfg: &ShadowConfig,
) -> LaggedShadowResult {
    let mut wallet_net: i128 = 0;
    let mut simulated: u32 = 0;
    let mut skipped: u32 = 0;
    for &a in wallet_actions {
        match simulate_action(oracle, a, cfg) {
            Some(net) => {
                wallet_net = wallet_net.saturating_add(net);
                simulated = simulated.saturating_add(1);
            }
            None => skipped = skipped.saturating_add(1),
        }
    }
    let mut control_net: i128 = 0;
    for &a in control_actions {
        if let Some(net) = simulate_action(oracle, a, cfg) {
            control_net = control_net.saturating_add(net);
        }
    }
    LaggedShadowResult {
        wallet_net,
        control_net,
        wallet_actions_simulated: simulated,
        wallet_actions_skipped: skipped,
    }
}

/// Legibility status of a wallet (Section 28 PUBLIC_BURNED presumption).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Legibility {
    /// Not publicly legible; discovered by this system's own evidence.
    Private,
    /// Discovered by this system *before* public trackers (pre-legibility
    /// preference — the highest-value target).
    PreLegibility,
    /// Publicly legible (leaderboard-ranked, tracker-tagged, KOL-posted, or a
    /// promoted "alpha wallet") — carries the PUBLIC_BURNED presumption.
    PublicBurned,
}

/// The eleven Section 28 wallet-quality states (confidence + evidence + decay,
/// never permanent from one episode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletQualityState {
    /// Passed PnL screen and the follower-executable lagged shadow; private.
    SmartMoneyFollowable,
    /// Followable and discovered pre-legibility.
    PreLegibilityCandidate,
    /// Profit concentrated in a single jackpot token.
    LuckyConcentratedPnl,
    /// Own PnL is real but not replicable at this system's latency (insider
    /// timing, or no edge over matched controls).
    InsiderTimingNonreplicable,
    /// Positive profit derived from self-dealt tokens.
    SelfDealingPnl,
    /// Wash / circular PnL (surfaced via the bait/self-dealing screens).
    WashPnl,
    /// Realized exits concentrate into induced follower flow.
    CopyBaitSuspect,
    /// Publicly legible; crowded and adversarially gameable until re-proven.
    PublicBurned,
    /// A rotated operator re-identified by behavioral fingerprint (research
    /// tier; never asserted as factual identity).
    RotatedReidentificationCandidate,
    /// Hedged / arbitrage behavior; excluded from directional-follow signals.
    HedgedOrArbBot,
    /// Not enough evidence to classify.
    InsufficientSample,
}

/// Combine the PnL screen, the lagged-shadow result, the copy-bait signal, and
/// legibility into a [`WalletQualityState`].
///
/// * `bait_suspect` — set when the copy-bait/legibility screen found the
///   wallet's realized exits concentrating into follower flow it induced.
///
/// Ordering follows Section 28: bait dominates (a bait wallet is never
/// followable regardless of shadow); then the PnL-screen failure reasons; then
/// the follower-executable law; then legibility.
#[must_use]
pub fn classify_smart_money(
    pnl: &PnlScreenResult,
    shadow: &LaggedShadowResult,
    legibility: Legibility,
    bait_suspect: bool,
) -> WalletQualityState {
    if bait_suspect {
        return WalletQualityState::CopyBaitSuspect;
    }
    if !pnl.passed() {
        return match pnl.reason {
            PnlFailReason::SelfDealing => WalletQualityState::SelfDealingPnl,
            PnlFailReason::LuckyConcentrated => WalletQualityState::LuckyConcentratedPnl,
            // Non-positive realized and insufficient sample both mean there is
            // no demonstrated skill to follow.
            PnlFailReason::InsufficientSample | PnlFailReason::NonPositiveRealized => {
                WalletQualityState::InsufficientSample
            }
            PnlFailReason::None => WalletQualityState::InsufficientSample, // unreachable
        };
    }
    // PnL screen passed: apply the follower-executable law.
    if !shadow.is_followable() {
        // Real PnL for them, but not executable at our latency/size/costs, or
        // no edge over matched controls.
        return WalletQualityState::InsiderTimingNonreplicable;
    }
    // Followable — legibility decides the final label.
    match legibility {
        Legibility::PublicBurned => WalletQualityState::PublicBurned,
        Legibility::PreLegibility => WalletQualityState::PreLegibilityCandidate,
        Legibility::Private => WalletQualityState::SmartMoneyFollowable,
    }
}
