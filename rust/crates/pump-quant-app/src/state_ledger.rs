//! The causal state ledger — the live port of the corpus's as-of-t state block.
//!
//! WHY THIS EXISTS. The Qwen entry prompt states twenty-odd *causal* facts about the mint at
//! a decision clock: counts, volumes, net flow, momentum over 5 s / 30 s / 300 s, the
//! 30 s return volatility, trader concentration, venue, evidence status. Those values are not
//! features this engine can invent — they are the **corpus's own numbers**, produced by
//! `build_states_v2._episode` (the stage-3 causal state ledger builder) and re-rendered by the
//! prompt builder. §1.4 makes prompt byte-parity a correctness requirement, so the live
//! derivation has to reproduce that function's semantics exactly, branch by branch, including
//! its guards.
//!
//! THE SEMANTICS THIS FILE PROMISES (each one is a way to get it wrong):
//! * **Strictly causal.** `i = searchsorted(tt, t_dec, "left")` — a trade whose
//!   `recv_unix_ms == t_dec` is excluded. Every window is half-open `[t_dec - win, t_dec)`.
//! * **Momentum is measured against the OLDEST finite price in the window**, not the newest
//!   (`_ret_bp` takes `seg[0]` after filtering), and the reference price is the last finite
//!   price *before the clock*.
//! * **Volatility is the population std of consecutive log returns** inside 30 s, requiring
//!   `n_prior_trades >= 30` and at least three positive prices. Not a sample std, not
//!   high-low, not absolute returns.
//! * **`unique_traders` and the top-1/top-5 shares are windowed** to the last
//!   [`CONC_WINDOW`] trades (volume-weighted, `abs(volume)`), *not* all-time.
//! * **Venue is a set over the last [`VENUE_WINDOW`] trades** — two venues read `mixed`, so
//!   the label is a statement about the recent tape, not about the mint forever.
//! * **`evidence_status` is `partial` if ANY price before the clock was non-finite**, which is
//!   why a non-finite price is carried as `None` rather than dropped.
//! * **Dust floors** ([`MIN_SOL_LAMPORTS`] / [`MIN_TOKENS_RAW`]) reject pass-through legs at
//!   ingest: a leg below them has a meaningless price and produced the 1e-11 prices and 1e12×
//!   moves in an earlier corpus.
//!
//! HONEST LIMITS, stated rather than hidden:
//! * The corpus additionally applies a **robust band** (keep trades within 10× of the mint's
//!   median price) computed over the *whole* mint series before any clock. That is a
//!   non-causal cleaning step; a live ledger cannot reproduce it without lookahead. This port
//!   therefore exposes [`StateSnapshot::complete`] and lets the caller refuse a bundle rather
//!   than serve a number the corpus would not have printed. The band is a **builder-level
//!   decision**, tracked in the v2 plan.
//! * The ring is bounded ([`RING_CAP`]). If an eviction ever removes a trade that a requested
//!   clock still needs (a clock older than the retained window), the snapshot is served
//!   `complete = false` instead of silently recomputing over a truncated tape.
//!
//! No clock, no RNG, no I/O: every time value arrives inside an event.

use std::collections::{BTreeMap, VecDeque};

/// Trades retained per mint for the concentration window and the trailing time windows.
pub const CONC_WINDOW: usize = 2_000;
/// Trades retained for the venue label (the corpus reads the last 200).
pub const VENUE_WINDOW: usize = 200;
/// Hard per-mint ring bound. Must exceed [`CONC_WINDOW`] and any realistic 300 s of tape.
pub const RING_CAP: usize = 8_192;
/// Hard bound on tracked mints (§99: every live table is capped). A NEW mint beyond this is
/// REFUSED and counted rather than evicting an older one: eviction would silently make an
/// existing mint's snapshot inexact, whereas a refusal is visible and costs only the
/// unwatched mint. The caller frees real capacity with [`StateLedger::forget`] when a mint
/// leaves the watchlist.
pub const MAX_TRACKED_MINTS: usize = 4_096;
/// `pump_quant_features::types::PRICE_SCALE` — `price_fp` is **lamports per raw token**, so
/// SOL per raw token is `price_fp / (PRICE_SCALE * LAMPORTS_PER_SOL)` = `price_fp / 1e18`.
///
/// Both scales are real and only one of them is the one people remember: the corpus's price is
/// SOL per raw token (~2.8e-8), the engine's `price_fp` is that times 1e9 for the lamport leg
/// and times another 1e9 for SOL→lamports. Getting this wrong by a single factor of 1e9 puts
/// every window's reference price off by 1e9 — which is exactly the class of error the corpus's
/// price floors (1e-11 prices, 1e12x moves) were introduced to kill.
pub const PRICE_SCALE: f64 = 1_000_000_000.0;
/// Lamports per SOL, as the engine's own conversion does it (`live_status.rs`: `price_fp / 1e18`).
pub const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;
/// Below this, a SOL leg is a pass-through, not a swap (`build_states_v2.MIN_SOL_LAMPORTS`).
pub const MIN_SOL_LAMPORTS: i64 = 100_000;
/// Below this, a token leg is rounding residue (`build_states_v2.MIN_TOKENS_RAW`).
pub const MIN_TOKENS_RAW: i64 = 1_000_000;
/// The three momentum horizons the corpus stores (`RET_HORIZONS`).
pub const RET_HORIZONS_S: [i64; 3] = [5, 30, 300];
/// The 30 s volatility horizon.
pub const VOL_WINDOW_S: i64 = 30;
/// The corpus's minimum predecessor-trade count before volatility may be computed.
pub const MIN_TRADES_FOR_VOL: usize = 30;

/// The venue label a trade carried, as the corpus's venue column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VenueLabel {
    /// pump.fun bonding curve.
    Pumpfun,
    /// PumpSwap AMM.
    Pumpswap,
    /// Not resolvable from the source string.
    Unknown,
}

impl VenueLabel {
    /// The corpus's spelling. Never reword: the prompt renders this string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VenueLabel::Pumpfun => "pumpfun",
            VenueLabel::Pumpswap => "pumpswap",
            VenueLabel::Unknown => "unknown",
        }
    }
}

/// One trade as the state ledger sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StateTrade {
    /// Receive time in unix milliseconds — the causal clock.
    pub recv_unix_ms: i64,
    /// Price in SOL per raw token, `None` when the tape's price was non-finite.
    pub price_sol_per_raw: Option<f64>,
    /// Signed SOL volume in lamports (buys negative on the tape; magnitude is the volume).
    pub sol_lamports_signed: i64,
    /// Signed base (token) volume in raw units, `None` when the producer could not supply it.
    /// The corpus's dust floors require BOTH legs (`build_states_v2.py`: `|sol| >= 100_000`
    /// AND `|tok| >= 1_000_000`), because a healthy SOL leg against a rounding-residue token
    /// leg is a pass-through whose price is meaningless.
    pub base_qty: Option<i64>,
    /// Which side.
    pub is_buy: bool,
    /// Stable per-entity trader id.
    pub trader: u64,
    /// Venue label.
    pub venue: VenueLabel,
}

impl StateTrade {
    /// Build the ledger's view of one live engine print.
    ///
    /// This is the ONLY way a live trade should enter the ledger, because each refusal below
    /// is a print that cannot honestly contribute to a corpus-parity number:
    ///
    /// * `price_fp <= 0` — the LaserStream path emits `price_fp: 0` until the reserve-delta or
    ///   account-snapshot step fills it. A zero price is a **placeholder, not a price**: the
    ///   corpus never priced 0 (its own floors reject such legs), and admitting one would drag
    ///   every window's reference price to zero.
    /// * `buyer_entity == 0` — the producer's "wallet could not be extracted" sentinel. Every
    ///   unknown trader would collapse into ONE entity, so unique-trader counts and the top-1 /
    ///   top-5 shares would read a single whale where the corpus saw many wallets.
    /// * `signed_base == 0` or `quote_lamports == 0` — no base moved and no quote paid: the
    ///   print carries no trade, and the corpus's `n_prior_trades` never counted it.
    ///
    /// `recv_unix_ms` is passed in rather than read here: this module never samples a clock, so
    /// a replay caller cannot fabricate wall-clock windows it did not record. Dust is NOT
    /// refused here — [`StateLedger::on_trade`] owns that floor so there is one authority.
    #[must_use]
    pub fn from_market_trade(
        recv_unix_ms: i64,
        price_fp: i128,
        quote_lamports: u64,
        signed_base: i64,
        buyer_entity: u64,
        venue: VenueLabel,
        base_qty: Option<i64>,
    ) -> Option<StateTrade> {
        if price_fp <= 0 || signed_base == 0 || quote_lamports == 0 {
            return None;
        }
        let is_buy = signed_base > 0;
        // The tape's sign convention (buys negative), which is also what `FlowEvent` carries.
        let magnitude = i64::try_from(quote_lamports).unwrap_or(i64::MAX);
        let sol_lamports_signed = if is_buy { -magnitude } else { magnitude };
        Some(StateTrade {
            recv_unix_ms,
            // lamports-per-raw-token / 1e9 = SOL per raw token (cross-check against the
            // engine's own `entry_price_fp / 1e18` in `live_status.rs`)).
            price_sol_per_raw: Some(price_fp as f64 / (PRICE_SCALE * LAMPORTS_PER_SOL)), // LINT-ALLOW(money_float_cast): corpus price is SOL per raw token; the wire value is fixed-point
            sol_lamports_signed,
            base_qty,
            is_buy,
            // `0` is the producer's unknown-trader sentinel, and the reserve-delta path — the
            // ONLY producer that carries a real price — sets it by design. Refusing those
            // prints would starve the ledger of every usable trade, so they are admitted and
            // the snapshot reports `identity_known: false`, which is what stops a prompt being
            // built from a number the corpus would not have printed.
            trader: buyer_entity,
            venue,
        })
    }

    /// The trade's volume, `abs()` as the corpus takes it (`vol = np.abs(sv)`).
    #[must_use]
    pub fn volume_lamports(self) -> i64 {
        self.sol_lamports_signed.saturating_abs()
    }

    /// Whether the trade is pass-through dust by the corpus's own floors.
    ///
    /// BOTH legs are checked, as `build_states_v2` does. An unknown token leg cannot be
    /// cleared, so it is treated as unknown rather than assumed healthy: [`MintLedger`] counts
    /// those prints and the snapshot reports `identity_known`/`token_leg_known` so a caller
    /// refuses instead of averaging a print it could not clear.
    #[must_use]
    pub fn is_dust(self) -> bool {
        self.volume_lamports() < MIN_SOL_LAMPORTS
            || self
                .base_qty
                .is_some_and(|q| q.unsigned_abs() < MIN_TOKENS_RAW as u64)
    }
}

/// Everything the decision prompt's state block states, as f64/ints.
///
/// Optional fields are `None` exactly where the corpus wrote `n/a`; a caller must render
/// `None` as `n/a`, never as `0`.
#[derive(Debug, Clone, PartialEq)]
pub struct StateSnapshot {
    /// Trades strictly before the clock.
    pub n_prior_trades: u64,
    /// Buys strictly before the clock.
    pub buy_count: u64,
    /// Sells strictly before the clock.
    pub sell_count: u64,
    /// Distinct traders in the concentration window.
    pub unique_traders: u64,
    /// Traded SOL volume strictly before the clock, lamports.
    pub sol_volume_lamports: i64,
    /// Buy-side volume, lamports.
    pub buy_volume_lamports: i64,
    /// Sell-side volume, lamports.
    pub sell_volume_lamports: i64,
    /// Buy minus sell, lamports.
    pub net_flow_lamports: i64,
    /// The last finite price before the clock.
    pub price_sol_per_raw: f64,
    /// Mint age at the clock, seconds.
    pub age_s: f64,
    /// Age of the last trade before the clock, seconds.
    pub last_trade_age_s: f64,
    /// Largest trader's share of concentration-window volume.
    pub top1_trader_share: Option<f64>,
    /// Top-5 traders' share of concentration-window volume.
    pub top5_trader_share: Option<f64>,
    /// Buy count / sell count, `None` when there were no sells.
    pub buyer_seller_ratio: Option<f64>,
    /// Population std of consecutive log returns inside 30 s, bp.
    pub price_volatility_30s_bp: Option<f64>,
    /// `pumpfun` / `pumpswap` / `mixed` / `unknown`.
    pub venue: String,
    /// `complete` when every price before the clock was finite, else `partial`.
    pub evidence_status: String,
    /// Momentum over the 5 s window, bp.
    pub ret_5s_bp: Option<f64>,
    /// Momentum over the 30 s window, bp.
    pub ret_30s_bp: Option<f64>,
    /// Momentum over the 300 s window, bp.
    pub ret_300s_bp: Option<f64>,
    /// Whether every window could be computed from a tape the ledger fully retained.
    /// **A caller must not build a prompt from an incomplete snapshot.**
    pub complete: bool,
    /// Whether every print in the concentration window carried a real trader id. False when
    /// any print came from a path that marks the trader unknown (the reserve-delta producer
    /// sets `buyer_entity: 0` by design), which makes `unique_traders` and the top-1/top-5
    /// shares NOT the corpus's numbers. **The builder must require this as well as
    /// `complete`.** The fix is to join the instruction print (which carries the wallet) with
    /// the reserve print (which carries the price) on `(mint, slot)`.
    pub identity_known: bool,
    /// Whether every print in the concentration window carried a token leg that could be
    /// cleared against [`MIN_TOKENS_RAW`].
    pub token_leg_known: bool,
}

#[derive(Debug, Default)]
struct MintLedger {
    /// Newest last; the oldest are dropped when the ring overflows.
    ring: VecDeque<StateTrade>,
    /// Trades ever seen (before any eviction), i.e. the tape's length.
    n_trades: u64,
    /// All-time buy/sell counts and volumes.
    cum_buy: u64,
    cum_sell: u64,
    cum_buy_vol: i64,
    cum_sell_vol: i64,
    /// All-time count of non-finite prices (drives `evidence_status`).
    cum_nonfinite: u64,
    /// All-time count of prints whose trader id was the unknown sentinel (`buyer_entity == 0`).
    cum_unknown_identity: u64,
    /// All-time count of prints whose token leg could not be cleared against the floor.
    cum_unknown_token_leg: u64,
    /// Prints refused for arriving out of order. Non-zero makes the tape not the corpus's tape,
    /// so it is folded into `complete` instead of vanishing.
    dropped_out_of_order: u64,
    /// First trade time of the mint (for `age_s`).
    first_t_ms: Option<i64>,
    /// Trades dropped by the ring bound, oldest first.
    evicted: u64,
}

/// The per-mint causal state ledger.
#[derive(Debug, Default)]
pub struct StateLedger {
    mints: BTreeMap<[u8; 32], MintLedger>,
    /// New mints refused because [`MAX_TRACKED_MINTS`] was reached.
    refused_mints: u64,
    /// Prints refused because their mint was not tracked.
    refused_prints: u64,
}

impl StateLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of mints tracked.
    #[must_use]
    pub fn tracked_mints(&self) -> usize {
        self.mints.len()
    }

    /// New mints refused at [`MAX_TRACKED_MINTS`]. Non-zero means the caller is not calling
    /// [`Self::forget`] when mints leave the watchlist.
    #[must_use]
    pub fn refused_mints(&self) -> u64 {
        self.refused_mints
    }

    /// Prints refused because their mint was not tracked, or because they arrived out of order.
    #[must_use]
    pub fn refused_prints(&self) -> u64 {
        self.refused_prints
            + self
                .mints
                .values()
                .map(|m| m.dropped_out_of_order)
                .sum::<u64>()
    }

    /// Drop a mint's history (used when a mint leaves the watchlist).
    pub fn forget(&mut self, mint: &[u8; 32]) -> bool {
        self.mints.remove(mint).is_some()
    }

    /// Feed one trade. Out-of-order arrivals for a mint are REFUSED (the corpus's tape is
    /// time-sorted), so the ledger never has to guess an ordering it cannot observe.
    pub fn on_trade(&mut self, mint: &[u8; 32], trade: StateTrade) -> bool {
        if trade.is_dust() {
            return false;
        }
        if !self.mints.contains_key(mint) && self.mints.len() >= MAX_TRACKED_MINTS {
            self.refused_mints = self.refused_mints.saturating_add(1);
            self.refused_prints = self.refused_prints.saturating_add(1);
            return false;
        }
        let l = self.mints.entry(*mint).or_default();
        if let Some(last) = l.ring.back() {
            if trade.recv_unix_ms < last.recv_unix_ms {
                l.dropped_out_of_order = l.dropped_out_of_order.saturating_add(1);
                return false;
            }
        }
        if l.first_t_ms.is_none() {
            l.first_t_ms = Some(trade.recv_unix_ms);
        }
        l.n_trades += 1;
        if trade.is_buy {
            l.cum_buy += 1;
            l.cum_buy_vol = l.cum_buy_vol.saturating_add(trade.volume_lamports());
        } else {
            l.cum_sell += 1;
            l.cum_sell_vol = l.cum_sell_vol.saturating_add(trade.volume_lamports());
        }
        if trade.price_sol_per_raw.is_none() {
            l.cum_nonfinite += 1;
        }
        if trade.trader == 0 {
            l.cum_unknown_identity = l.cum_unknown_identity.saturating_add(1);
        }
        if trade.base_qty.is_none() {
            l.cum_unknown_token_leg = l.cum_unknown_token_leg.saturating_add(1);
        }
        l.ring.push_back(trade);
        while l.ring.len() > RING_CAP {
            l.ring.pop_front();
            l.evicted += 1;
        }
        true
    }

    /// Serve the state block at `t_dec_ms`.
    ///
    /// `None` when the ledger has nothing usable (no trades before the clock, or no finite
    /// price before it) — the corpus's own builder has no honest answer there either, and a
    /// fabricated one would be out-of-distribution input.
    #[must_use]
    pub fn serve(&self, mint: &[u8; 32], t_dec_ms: i64) -> Option<StateSnapshot> {
        let l = self.mints.get(mint)?;
        // Trades strictly before the clock, in the retained ring.
        let before: Vec<&StateTrade> = l
            .ring
            .iter()
            .filter(|t| t.recv_unix_ms < t_dec_ms)
            .collect();
        if before.is_empty() {
            return None;
        }
        let n_prior = l.n_trades - (l.ring.len() - before.len()) as u64;
        // Eviction always removes the OLDEST trade, so the retained suffix is contiguous and
        // every evicted trade is older than the ring's front. The snapshot is exact when the
        // ring still covers every window this clock reads: the 300 s momentum window, and the
        // concentration window (the last `min(CONC_WINDOW, n_prior)` trades).
        let need_conc = CONC_WINDOW.min(n_prior as usize);
        let complete = match (l.evicted, l.ring.front()) {
            (0, _) => true,
            (_, None) => false,
            (_, Some(front)) => {
                front.recv_unix_ms <= t_dec_ms - RET_HORIZONS_S[2] * 1000
                    && before.len() >= need_conc
            }
        };
        let price = before
            .iter()
            .rev()
            .find_map(|t| t.price_sol_per_raw.filter(|p| p.is_finite()))?;
        if !(price > 0.0) {
            return None;
        }
        let last_t = before.last()?.recv_unix_ms;
        let first_t = l.first_t_ms?;

        // Cumulative totals minus the trades at or after the clock (which the ring holds,
        // because the ring always retains the newest suffix).
        let after_ring: Vec<&StateTrade> = l
            .ring
            .iter()
            .filter(|t| t.recv_unix_ms >= t_dec_ms)
            .collect();
        let after_buy = after_ring.iter().filter(|t| t.is_buy).count() as u64;
        let after_sell = after_ring.len() as u64 - after_buy;
        let buy_count = l.cum_buy - after_buy;
        let sell_count = l.cum_sell - after_sell;
        let buy_vol: i64 = l.cum_buy_vol
            - after_ring
                .iter()
                .filter(|t| t.is_buy)
                .map(|t| t.volume_lamports())
                .sum::<i64>();
        let sell_vol: i64 = l.cum_sell_vol
            - after_ring
                .iter()
                .filter(|t| !t.is_buy)
                .map(|t| t.volume_lamports())
                .sum::<i64>();
        let nonfinite_after = after_ring
            .iter()
            .filter(|t| t.price_sol_per_raw.is_none())
            .count() as u64;
        let nonfinite_before = l.cum_nonfinite - nonfinite_after;
        let unknown_identity_after = after_ring.iter().filter(|t| t.trader == 0).count() as u64;
        let unknown_identity_before = l.cum_unknown_identity - unknown_identity_after;
        let unknown_leg_after = after_ring.iter().filter(|t| t.base_qty.is_none()).count() as u64;
        let unknown_leg_before = l.cum_unknown_token_leg - unknown_leg_after;

        // Concentration: the last CONC_WINDOW trades before the clock, volume-weighted.
        let lo = before.len().saturating_sub(CONC_WINDOW);
        let mut per_trader: BTreeMap<u64, f64> = BTreeMap::new();
        for t in &before[lo..] {
            *per_trader.entry(t.trader).or_insert(0.0) += t.volume_lamports() as f64;
            // LINT-ALLOW(money_float_cast): the corpus accumulates volume shares in f64 (np.bincount weights)
        }
        let unique_traders = per_trader.values().filter(|v| **v > 0.0).count() as u64;
        let tot: f64 = per_trader.values().sum();
        let (top1, top5) = if tot > 0.0 {
            let mut shares: Vec<f64> = per_trader.values().copied().collect();
            shares.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            // `np.sort(agg)[::-1]` then `top[:k].sum()` — descending, summed in order.
            let t1: f64 = shares.iter().take(1).sum::<f64>() / tot;
            let t5: f64 = shares.iter().take(5).sum::<f64>() / tot;
            (Some(t1), Some(t5))
        } else {
            (None, None)
        };

        // Venue over the last VENUE_WINDOW trades before the clock.
        let vlo = before.len().saturating_sub(VENUE_WINDOW);
        let mut venues: Vec<VenueLabel> = before[vlo..].iter().map(|t| t.venue).collect();
        venues.sort();
        venues.dedup();
        let venue = match venues.len() {
            0 => "unknown".to_string(),
            1 => venues[0].as_str().to_string(),
            _ => "mixed".to_string(),
        };

        Some(StateSnapshot {
            n_prior_trades: n_prior,
            buy_count,
            sell_count,
            unique_traders,
            sol_volume_lamports: buy_vol.saturating_add(sell_vol),
            buy_volume_lamports: buy_vol,
            sell_volume_lamports: sell_vol,
            net_flow_lamports: buy_vol.saturating_sub(sell_vol),
            price_sol_per_raw: price,
            age_s: (t_dec_ms - first_t) as f64 / 1000.0, // LINT-ALLOW(money_float_cast): age_s is rendered by the corpus from (t_dec - t0) / 1000.0
            last_trade_age_s: (t_dec_ms - last_t) as f64 / 1000.0, // LINT-ALLOW(money_float_cast): last_trade_age_s, same division as the authority
            top1_trader_share: top1,
            top5_trader_share: top5,
            buyer_seller_ratio: if sell_count > 0 {
                Some(buy_count as f64 / sell_count as f64) // LINT-ALLOW(money_float_cast): buyer_seller_ratio is a corpus float division
            } else {
                None
            },
            price_volatility_30s_bp: volatility_30s(&before, price, t_dec_ms),
            venue,
            evidence_status: if nonfinite_before == 0 {
                "complete".to_string()
            } else {
                "partial".to_string()
            },
            ret_5s_bp: ret_bp(&before, price, t_dec_ms, 5),
            ret_30s_bp: ret_bp(&before, price, t_dec_ms, 30),
            ret_300s_bp: ret_bp(&before, price, t_dec_ms, 300),
            // A refused or dropped print means this is not the corpus's tape, so the snapshot
            // says so rather than serving numbers that look clean.
            complete: complete && l.dropped_out_of_order == 0,
            identity_known: unknown_identity_before == 0,
            token_leg_known: unknown_leg_before == 0,
        })
    }
}

/// `build_states_v2._ret_bp` — momentum against the OLDEST finite price in the window.
fn ret_bp(before: &[&StateTrade], price: f64, t_dec_ms: i64, win_s: i64) -> Option<f64> {
    let start = t_dec_ms - win_s * 1000;
    let seg: Vec<f64> = before
        .iter()
        .filter(|t| t.recv_unix_ms >= start)
        .filter_map(|t| t.price_sol_per_raw)
        .filter(|p| p.is_finite())
        .collect();
    let first = *seg.first()?;
    if !(first > 0.0) {
        return None;
    }
    Some((price - first) / first * 10_000.0)
}

/// `build_states_v2._episode`'s volatility block — population std of consecutive log returns.
fn volatility_30s(before: &[&StateTrade], _price: f64, t_dec_ms: i64) -> Option<f64> {
    if before.len() < MIN_TRADES_FOR_VOL {
        return None;
    }
    let start = t_dec_ms - VOL_WINDOW_S * 1000;
    let seg: Vec<f64> = before
        .iter()
        .filter(|t| t.recv_unix_ms >= start)
        .filter_map(|t| t.price_sol_per_raw)
        .filter(|p| p.is_finite())
        .collect();
    if seg.len() < 3 || seg.iter().any(|p| !(*p > 0.0)) {
        return None;
    }
    // np.diff(np.log(seg)) — consecutive differences in chronological order.
    let logs: Vec<f64> = seg.iter().map(|p| p.ln()).collect();
    let rets: Vec<f64> = logs.windows(2).map(|w| w[1] - w[0]).collect();
    if rets.len() < 2 {
        return None;
    }
    // np.std default is ddof=0 and numpy uses the two-pass formula for float input, so the
    // mean is removed first and the squared deviations are averaged — not E[x^2] - E[x]^2.
    let mean = rets.iter().sum::<f64>() / rets.len() as f64; // LINT-ALLOW(money_float_cast): the authority's mean of returns (np.mean)
    let var = rets.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / rets.len() as f64; // LINT-ALLOW(money_float_cast): the authority's population variance (np.var, ddof=0)
    Some(var.sqrt() * 10_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real print: 2.5e-8 SOL per raw token, 0.4 SOL bought, a 2M-token leg.
    fn buy_print(t: i64) -> Option<StateTrade> {
        StateTrade::from_market_trade(
            t,
            25_000_000_000,
            400_000_000,
            1,
            42,
            VenueLabel::Pumpfun,
            Some(2_000_000),
        )
    }

    #[test]
    fn a_real_print_converts_with_the_corpus_units() {
        let t = buy_print(1_700_000_000_000).expect("admitted");
        assert_eq!(
            t.price_sol_per_raw,
            Some(2.5e-8),
            "price_fp is lamports per raw token: / (PRICE_SCALE * LAMPORTS_PER_SOL)"
        );
        assert!(
            t.is_buy,
            "signed_base > 0 is a buy on the engine's convention"
        );
        assert!(
            t.sol_lamports_signed < 0,
            "the tape carries buys negative, as build_states_v2 abs()es them"
        );
        assert_eq!(t.volume_lamports(), 400_000_000);
        assert_eq!(t.trader, 42);
        assert!(!t.is_dust());
    }

    #[test]
    fn prints_that_cannot_honestly_contribute_are_refused() {
        let t = 1_700_000_000_000;
        // The LaserStream placeholder: price_fp is 0 until the reserve-delta step fills it.
        // A zero here is not a price, and admitting it drags every window's reference to 0.
        assert!(
            StateTrade::from_market_trade(t, 0, 400_000_000, 1, 42, VenueLabel::Pumpfun, None)
                .is_none()
        );
        // No base movement, or no quote paid: not a swap.
        assert!(StateTrade::from_market_trade(
            t,
            25_000_000_000,
            400_000_000,
            0,
            42,
            VenueLabel::Pumpfun,
            Some(2_000_000)
        )
        .is_none());
        assert!(StateTrade::from_market_trade(
            t,
            25_000_000_000,
            0,
            1,
            42,
            VenueLabel::Pumpfun,
            Some(2_000_000)
        )
        .is_none());
        // The unknown-trader sentinel is ADMITTED and flagged, never refused: the
        // reserve-delta path (the only producer with a real price) sets it by design.
        let unknown = StateTrade::from_market_trade(
            t,
            25_000_000_000,
            400_000_000,
            1,
            0,
            VenueLabel::Pumpfun,
            Some(2_000_000),
        )
        .expect("admitted, flagged");
        assert_eq!(unknown.trader, 0);
        assert!(
            !unknown.is_dust(),
            "an unknown IDENTITY is not unknown SIZE: it must still be tradeable"
        );
    }

    #[test]
    fn the_dust_floor_has_exactly_one_authority_and_clears_both_legs() {
        let mut ledger = StateLedger::new();
        let mint = [22u8; 32];
        // A healthy token leg against a sub-floor SOL leg: pass-through.
        let small_sol = StateTrade::from_market_trade(
            1_000,
            25_000_000_000,
            99_999,
            1,
            42,
            VenueLabel::Pumpswap,
            Some(2_000_000),
        )
        .expect("the adapter admits it");
        assert!(small_sol.is_dust(), "the ledger refuses it");
        assert!(!ledger.on_trade(&mint, small_sol));
        // A healthy SOL leg against a residue token leg is the same pass-through, and the
        // corpus clears BOTH legs. Bypassing the adapter does not bypass the floor.
        let residue = StateTrade::from_market_trade(
            2_000,
            25_000_000_000,
            400_000_000,
            1,
            42,
            VenueLabel::Pumpswap,
            Some(1_000),
        )
        .expect("the adapter admits it");
        assert!(residue.is_dust(), "the token leg fails the second floor");
        assert!(!ledger.on_trade(&mint, residue));
        assert!(
            ledger.serve(&mint, 3_000).is_none(),
            "a mint whose every print was dust has no snapshot to serve, not a zeroed one"
        );
    }

    #[test]
    fn a_refused_print_never_reaches_a_window() {
        let mut ledger = StateLedger::new();
        let mint = [7u8; 32];
        assert!(ledger.on_trade(&mint, buy_print(1_000).unwrap()));
        // The placeholder that the instruction path emits (price_fp still 0).
        assert!(StateTrade::from_market_trade(
            2_000,
            0,
            400_000_000,
            1,
            42,
            VenueLabel::Pumpfun,
            Some(2_000_000)
        )
        .is_none());
        assert!(ledger.on_trade(&mint, buy_print(2_500).unwrap()));
        let s = ledger.serve(&mint, 3_000).expect("snapshot");
        assert_eq!(s.n_prior_trades, 2, "the placeholder is not a trade");
        assert_eq!(
            s.price_sol_per_raw, 2.5e-8,
            "nor a price: the reference stays the last real print"
        );
        assert!(s.identity_known && s.token_leg_known);
    }

    #[test]
    fn an_unknown_identity_or_leg_is_reported_not_hidden() {
        let mut ledger = StateLedger::new();
        let mint = [8u8; 32];
        assert!(ledger.on_trade(&mint, buy_print(1_000).unwrap()));
        // What the reserve-delta producer actually emits: real price, unknown trader, and a
        // token leg it does carry.
        let delta = StateTrade::from_market_trade(
            2_000,
            25_000_000_000,
            400_000_000,
            2,
            0,
            VenueLabel::Pumpfun,
            Some(2_000_000),
        )
        .expect("admitted");
        assert!(ledger.on_trade(&mint, delta));
        let s = ledger.serve(&mint, 3_000).expect("snapshot");
        assert_eq!(s.n_prior_trades, 2);
        assert!(
            !s.identity_known,
            "unique_traders / top1 / top5 are NOT the corpus's numbers here"
        );
        assert!(s.token_leg_known);
        // The join that fixes it (instruction print carries the wallet, reserve print the
        // price, both carry the slot) is a junction-side job, and until it exists the builder
        // must refuse on `identity_known`.
        let no_leg = StateTrade::from_market_trade(
            3_000,
            25_000_000_000,
            400_000_000,
            3,
            42,
            VenueLabel::Pumpfun,
            None,
        )
        .expect("admitted");
        assert!(ledger.on_trade(&mint, no_leg));
        assert!(
            !ledger
                .serve(&mint, 4_000)
                .expect("snapshot")
                .token_leg_known
        );
    }

    #[test]
    fn a_dropped_out_of_order_print_makes_the_tape_inexact() {
        let mut ledger = StateLedger::new();
        let mint = [9u8; 32];
        assert!(ledger.on_trade(&mint, buy_print(2_000).unwrap()));
        assert!(
            !ledger.on_trade(&mint, buy_print(1_000).unwrap()),
            "older than the newest retained print: refused"
        );
        assert_eq!(ledger.refused_prints(), 1);
        let s = ledger.serve(&mint, 3_000).expect("snapshot");
        assert_eq!(s.n_prior_trades, 1);
        assert!(
            !s.complete,
            "a tape that lost a print is not the corpus's tape"
        );
    }

    #[test]
    fn the_mint_table_is_capped_and_refusals_are_visible() {
        let mut ledger = StateLedger::new();
        for i in 0..MAX_TRACKED_MINTS {
            let mut mint = [0u8; 32];
            mint[..8].copy_from_slice(&(i as u64).to_le_bytes());
            assert!(ledger.on_trade(&mint, buy_print(1_000).unwrap()));
        }
        let mut extra = [0u8; 32];
        extra[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(
            !ledger.on_trade(&extra, buy_print(1_000).unwrap()),
            "a new mint past the cap is refused, never an eviction"
        );
        assert_eq!(ledger.refused_mints(), 1);
        assert_eq!(ledger.refused_prints(), 1);
        assert_eq!(ledger.tracked_mints(), MAX_TRACKED_MINTS);
        // Freeing a TRACKED mint restores capacity (the refused one was never tracked).
        let mut tracked = [0u8; 32];
        tracked[..8].copy_from_slice(&7u64.to_le_bytes());
        assert_eq!(ledger.tracked_mints(), MAX_TRACKED_MINTS);
        ledger.forget(&tracked);
        assert_eq!(ledger.tracked_mints(), MAX_TRACKED_MINTS - 1);
        assert!(ledger.on_trade(&extra, buy_print(1_000).unwrap()));
    }
}
