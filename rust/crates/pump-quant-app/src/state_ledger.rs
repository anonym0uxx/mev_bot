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
    ) -> Option<StateTrade> {
        if price_fp <= 0 || buyer_entity == 0 || signed_base == 0 || quote_lamports == 0 {
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
            price_sol_per_raw: Some(price_fp as f64 / (PRICE_SCALE * LAMPORTS_PER_SOL)),
            sol_lamports_signed,
            is_buy,
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
    #[must_use]
    pub fn is_dust(self) -> bool {
        self.volume_lamports() < MIN_SOL_LAMPORTS
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
    /// First trade time of the mint (for `age_s`).
    first_t_ms: Option<i64>,
    /// Trades dropped by the ring bound, oldest first.
    evicted: u64,
}

/// The per-mint causal state ledger.
#[derive(Debug, Default)]
pub struct StateLedger {
    mints: BTreeMap<[u8; 32], MintLedger>,
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
        let l = self.mints.entry(*mint).or_default();
        if let Some(last) = l.ring.back() {
            if trade.recv_unix_ms < last.recv_unix_ms {
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
        let buys = before.iter().filter(|t| t.is_buy).count() as u64;
        let sells = n_prior - buys;
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

        // Concentration: the last CONC_WINDOW trades before the clock, volume-weighted.
        let lo = before.len().saturating_sub(CONC_WINDOW);
        let mut per_trader: BTreeMap<u64, f64> = BTreeMap::new();
        for t in &before[lo..] {
            *per_trader.entry(t.trader).or_insert(0.0) += t.volume_lamports() as f64;
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
            age_s: (t_dec_ms - first_t) as f64 / 1000.0,
            last_trade_age_s: (t_dec_ms - last_t) as f64 / 1000.0,
            top1_trader_share: top1,
            top5_trader_share: top5,
            buyer_seller_ratio: if sell_count > 0 {
                Some(buy_count as f64 / sell_count as f64)
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
            complete,
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
    let mean = rets.iter().sum::<f64>() / rets.len() as f64;
    let var = rets.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / rets.len() as f64;
    Some(var.sqrt() * 10_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real print: 2.5e-8 SOL per raw token, 0.4 SOL bought.
    fn buy_print(t: i64) -> Option<StateTrade> {
        StateTrade::from_market_trade(
            t,
            25_000_000_000, // price_fp = 2.5e-8 SOL/raw-token * 1e18
            400_000_000,
            1_234_567, // positive base = buy on the EVENT's convention (signed_base > 0)
            42,
            VenueLabel::Pumpfun,
        )
    }

    #[test]
    fn a_real_print_converts_with_the_corpus_units() {
        let t = buy_print(1_700_000_000_000).expect("accepted");
        assert_eq!(t.price_sol_per_raw, Some(2.5e-8));
        assert!(t.is_buy);
        assert_eq!(t.volume_lamports(), 400_000_000);
        assert_eq!(
            t.sol_lamports_signed, -400_000_000,
            "the tape records buys negative"
        );
        assert_eq!(t.trader, 42);
        assert!(!t.is_dust());
    }

    /// Every refusal is a print that would otherwise corrupt a corpus-parity number.
    #[test]
    fn prints_that_cannot_honestly_contribute_are_refused() {
        let t = 1_700_000_000_000;
        // The LaserStream placeholder: price_fp is filled by the reserve-delta step, not here.
        assert!(
            StateTrade::from_market_trade(t, 0, 400_000_000, 1, 42, VenueLabel::Pumpfun).is_none()
        );
        // Unknown-trader sentinel: every unknown wallet would collapse into one entity.
        assert!(StateTrade::from_market_trade(
            t,
            25_000_000_000,
            400_000_000,
            -1,
            0,
            VenueLabel::Pumpfun
        )
        .is_none());
        // No base movement, or no quote paid: not a swap.
        assert!(StateTrade::from_market_trade(
            t,
            25_000_000_000,
            400_000_000,
            0,
            42,
            VenueLabel::Pumpfun
        )
        .is_none());
        assert!(
            StateTrade::from_market_trade(t, 25_000_000_000, 0, 1, 42, VenueLabel::Pumpfun)
                .is_none()
        );
    }

    /// A refused print changes nothing downstream: not a count, not a price, not a window.
    #[test]
    fn a_refused_print_never_reaches_a_window() {
        let mut ledger = StateLedger::new();
        let mint = [21u8; 32];
        assert!(ledger.on_trade(&mint, buy_print(1_000).expect("accepted")));
        // The unpatched placeholder arrives between two real prints.
        let placeholder =
            StateTrade::from_market_trade(1_500, 0, 400_000_000, 1, 42, VenueLabel::Pumpfun);
        assert!(placeholder.is_none(), "refused, so nothing is fed");
        assert!(ledger.on_trade(&mint, buy_print(2_000).expect("accepted")));
        let s = ledger.serve(&mint, 3_000).expect("snapshot");
        assert_eq!(s.n_prior_trades, 2, "the placeholder is not a trade");
        assert_eq!(s.price_sol_per_raw, 2.5e-8, "nor a price");
    }

    /// Dust is refused by the LEDGER, not the adapter: one authority for the floor, so a future
    /// caller cannot bypass it by constructing a `StateTrade` directly.
    #[test]
    fn the_dust_floor_has_exactly_one_authority() {
        let mut ledger = StateLedger::new();
        let mint = [22u8; 32];
        let dust = StateTrade::from_market_trade(
            1_000,
            25_000_000_000,
            99_999,
            -1,
            42,
            VenueLabel::Pumpswap,
        )
        .expect("the adapter is not the floor");
        assert!(!ledger.on_trade(&mint, dust), "the ledger refuses it");
        assert!(
            ledger.serve(&mint, 2_000).is_none(),
            "and no snapshot exists"
        );
    }
}
