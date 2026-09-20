//! Live c11 `LIVE FLOW STATE` reduction over the confirmed trade stream.
//!
//! THE ONE JOB: produce the fifteen-line `LIVE FLOW STATE` block's thirteen values at a
//! decision clock, with the exact definitions the c11 corpus was built with. Those
//! definitions live in `/training/v2/code/src/v2/rl/build_c11_flow_enrichment.py`; this
//! module is a faithful, integer-only port of its `serve()` + event-application path, and
//! `tests/flow_oracle.rs` checks it against that script's own output on a shared micro-tape.
//! Any drift here is silent model damage — the model was trained on this block's text.
//!
//! WHY INTEGERS. Python's builder rounds to 6 decimal places (`round(x, 6)`) and to 1 dp for
//! `flow_lookback_d`. `round(x, 6)` of a lamport-derived value is exactly an integer number of
//! millionths, so this module carries `*_micro` (millionths) and `*_tenths` integers and
//! converts to `f64` only at the last moment ([`FlowAggregates::to_corpus_values`]). That
//! conversion yields the same double Python's `round()` produced, so the existing byte-parity
//! renderer reproduces the corpus text exactly. It also keeps the reducer free of float
//! equality questions and of float accumulation error over millions of events.
//!
//! CAUSALITY. `on_event` applies an event; `serve` answers for a clock. The driver must call
//! `serve` for every clock at or before the event time BEFORE applying that event — the same
//! discipline the builder uses ("clocks are served BEFORE the first event with recv >= t is
//! applied"). `serve` additionally filters to `recv < t`, so a driver that gets the ordering
//! wrong still cannot leak a later trade into an earlier decision; it can only leave a trade
//! out, which is the safe direction.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// A wallet identity (32-byte public key).
pub type Wallet = [u8; 32];
/// A mint identity (32-byte public key).
pub type MintId = [u8; 32];

/// Which side of the tape an event was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// A buy: SOL leaves the trader (the tape records it negative).
    Buy,
    /// A sell: SOL reaches the trader (recorded positive).
    Sell,
}

/// One confirmed trade, in the tape's own units.
///
/// `sol_lamports` is SIGNED and follows the tape's convention: negative for a buy, positive
/// for a sell. The builder's net-flow line is `-sum(sol_lamports)`, so a buy contributes
/// positive net flow; keeping the tape sign here means the port cannot drift from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlowEvent {
    /// The traded mint.
    pub mint: MintId,
    /// The trader.
    pub trader: Wallet,
    /// Which side.
    pub side: Side,
    /// Chain slot the trade landed in (used only for the sniper rule).
    pub slot: u64,
    /// Receive time in unix milliseconds — the only clock this reducer uses.
    pub recv_unix_ms: i64,
    /// Signed SOL in lamports (buy negative, sell positive).
    pub sol_lamports: i64,
    /// Priority+network fee paid, in lamports.
    pub fee_lamports: u64,
    /// Compute units consumed, when the tape recorded them.
    pub cu_consumed: Option<u64>,
}

/// The c11 window/percentile constants, as a parameter block so a caller can see them
/// (and a test can tighten them) rather than reading magic numbers out of the code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlowParams {
    /// The 300 s flow window, in ms.
    pub window_300_ms: i64,
    /// The 60 s entrant window, in ms.
    pub window_60_ms: i64,
    /// A wallet counts as fresh when its rolling first-activity is younger than this.
    pub fresh_ms: i64,
    /// The freshness lookback is bounded at 7 d so a live collector can reproduce it.
    pub lookback_ms: i64,
    /// Cumulative extracted SOL (lamports) for the smart-wallet rule.
    pub smart_sol_lamports: i64,
    /// Distinct mints traded for the smart-wallet rule.
    pub smart_mints: usize,
    /// First-N buyers of a mint form the co-entry candidates.
    pub early_n: usize,
    /// Shared early co-entries needed to link two wallets.
    pub coentry_min: u32,
    /// A first trade within this many slots of the mint's first is a snipe.
    pub sniper_slots: u64,
}

impl Default for FlowParams {
    /// The c11 constants, verbatim.
    fn default() -> Self {
        Self {
            window_300_ms: 300_000,
            window_60_ms: 60_000,
            fresh_ms: 86_400_000,
            lookback_ms: 604_800_000,
            smart_sol_lamports: 5_000_000_000,
            smart_mints: 5,
            early_n: 20,
            coentry_min: 2,
            sniper_slots: 2,
        }
    }
}

/// The thirteen aggregates, as exact integers (millionths / tenths).
///
/// Field order matches `inject_c11_flow.py::FIELDS`; `Option` is Python `None`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlowAggregates {
    /// Distinct buyers in the last 60 s.
    pub entrants_60s: u32,
    /// Distinct buyers in the last 300 s.
    pub entrants_300s: u32,
    /// Net SOL in, in millionths of SOL (may be negative).
    pub net_flow_sol_300s_micro: i64,
    /// Fresh share, millionths (None when there are no entrants).
    pub fresh_wallet_share_300s_micro: Option<u32>,
    /// Observed lookback depth in tenths of a day, capped at the 7 d bound.
    pub flow_lookback_d_tenths: u32,
    /// Sniper share, millionths (None when there are no entrants).
    pub sniper_share_300s_micro: Option<u32>,
    /// Repeated-exact-amount buy share, millionths (None when there are no buys).
    pub bot_uniform_share_300s_micro: Option<u32>,
    /// Entrants meeting the smart-wallet bar.
    pub smart_entrants_300s: u32,
    /// Net SOL from smart wallets, millionths.
    pub smart_net_flow_sol_300s_micro: i64,
    /// Entrants sharing >= `coentry_min` early co-entries with another entrant.
    pub coentry_wallets_300s: u32,
    /// The creator traded its own mint.
    pub creator_trading_own_mint: bool,
    /// p90 fee of the window's buys, lamports.
    pub entrant_fee_p90_lamports: Option<u64>,
    /// p50 compute units of the window's buys.
    pub entrant_cu_p50: Option<u64>,
}

/// The same numbers as `f64`, exactly as Python's `round(..., 6)` / `round(..., 1)` produced
/// them — ready to hand to the byte-parity renderer.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CorpusFlowValues {
    /// Net SOL in over 300 s.
    pub net_flow_sol_300s: f64,
    /// Fresh share over 300 s.
    pub fresh_wallet_share_300s: Option<f64>,
    /// Lookback depth, days.
    pub flow_lookback_d: f64,
    /// Sniper share over 300 s.
    pub sniper_share_300s: Option<f64>,
    /// Repeated-amount share over 300 s.
    pub bot_uniform_share_300s: Option<f64>,
    /// Smart-wallet net SOL over 300 s.
    pub smart_net_flow_sol_300s: f64,
}

impl FlowAggregates {
    /// Convert the integer carries to the exact `f64` values the corpus rendered.
    ///
    /// `micro as f64 / 1_000_000.0` is the nearest double to the correctly-rounded 6dp
    /// decimal — the same double Python's `round(x, 6)` returns, which is why the existing
    /// renderer reproduces the corpus bytes from these.
    #[must_use]
    pub fn to_corpus_values(&self) -> CorpusFlowValues {
        let micro = |v: i64| v as f64 / 1_000_000.0;
        CorpusFlowValues {
            net_flow_sol_300s: micro(self.net_flow_sol_300s_micro),
            fresh_wallet_share_300s: self
                .fresh_wallet_share_300s_micro
                .map(|v| f64::from(v) / 1e6),
            flow_lookback_d: f64::from(self.flow_lookback_d_tenths) / 10.0,
            sniper_share_300s: self.sniper_share_300s_micro.map(|v| f64::from(v) / 1e6),
            bot_uniform_share_300s: self
                .bot_uniform_share_300s_micro
                .map(|v| f64::from(v) / 1e6),
            smart_net_flow_sol_300s: micro(self.smart_net_flow_sol_300s_micro),
        }
    }
}

/// What `serve` returned: either the `no_prior_flow` marker or the thirteen values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlowOutcome {
    /// The mint has never appeared in the stream — the corpus renders the single
    /// `no_prior_flow=true` line instead of fabricating zeros.
    NoPriorFlow,
    /// The aggregates for this clock.
    Aggregates(FlowAggregates),
}

#[derive(Clone, Copy, Debug)]
struct Ev {
    recv: i64,
    trader: Wallet,
    side: Side,
    lamports: i64,
    fee: u64,
    cu: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct WalletState {
    first_ms: i64,
    last_ms: i64,
    extracted_lamports: i64,
    /// Distinct mints traded, SATURATED at five. The smart-wallet rule only asks for
    /// ">= 5 distinct mints", so sampling the first five distinct mints answers it exactly
    /// in bounded memory instead of keeping an unbounded set per wallet.
    distinct_mints: u32,
    mint_samples: [MintId; 5],
}

/// The live reducer. Feed it every confirmed trade in receive order; ask it for a mint's
/// flow state at any clock.
#[derive(Debug)]
pub struct FlowReducer {
    p: FlowParams,
    tape_t0: Option<i64>,
    wallets: HashMap<Wallet, WalletState>,
    windows: HashMap<MintId, VecDeque<Ev>>,
    mint_first_slot: HashMap<MintId, u64>,
    mint_snipers: HashMap<MintId, BTreeSet<Wallet>>,
    mint_wallets: HashMap<MintId, BTreeSet<Wallet>>,
    early_buyers: HashMap<MintId, Vec<Wallet>>,
    copartners: HashMap<Wallet, BTreeMap<Wallet, u32>>,
    creator_of: HashMap<MintId, Wallet>,
    creator_traded: BTreeSet<MintId>,
    tracked: BTreeSet<MintId>,
    seen_mints: BTreeSet<MintId>,
}

impl FlowReducer {
    /// A reducer with the c11 parameters.
    #[must_use]
    pub fn new() -> Self {
        Self::with_params(FlowParams::default())
    }

    /// A reducer with explicit parameters.
    #[must_use]
    pub fn with_params(p: FlowParams) -> Self {
        Self {
            p,
            tape_t0: None,
            wallets: HashMap::new(),
            windows: HashMap::new(),
            mint_first_slot: HashMap::new(),
            mint_snipers: HashMap::new(),
            mint_wallets: HashMap::new(),
            early_buyers: HashMap::new(),
            copartners: HashMap::new(),
            creator_of: HashMap::new(),
            creator_traded: BTreeSet::new(),
            tracked: BTreeSet::new(),
            seen_mints: BTreeSet::new(),
        }
    }

    /// The parameters in force.
    #[must_use]
    pub fn params(&self) -> &FlowParams {
        &self.p
    }

    /// Register a mint as one the caller will serve.
    ///
    /// The offline builder retained a per-mint window, sniper set and first-slot for its clock
    /// mints only (it applied every event, but only kept clock-mint state), so tracking exactly
    /// the served set is the same rule with bounded memory — not a semantic change. The
    /// co-entry graph stays GLOBAL either way (a co-entry on an unserved mint legitimately
    /// links two wallets that later appear on a served one); see [`FlowReducer::prune_coentry`]
    /// for the memory lever.
    pub fn track_mint(&mut self, mint: MintId) {
        self.tracked.insert(mint);
    }

    /// Record the creator of a mint (from the launches feed) so `creator_trading_own_mint`
    /// can be answered.
    pub fn set_creator(&mut self, mint: MintId, creator: Wallet) {
        self.creator_of.insert(mint, creator);
    }

    /// Apply one confirmed trade.
    ///
    /// Callers must filter to confirmed (`status == "success"`) events; the builder does.
    pub fn on_event(&mut self, e: &FlowEvent) {
        let t = e.recv_unix_ms;
        let w = e.trader;
        let ws = self.wallets.entry(w).or_insert(WalletState {
            first_ms: t,
            last_ms: t,
            ..WalletState::default()
        });
        // Rolling-window first activity: idle for longer than the lookback and the wallet is
        // fresh again. (Bounded lookback is the c11 fix; unbounded first-seen drifts.)
        if t.saturating_sub(ws.last_ms) > self.p.lookback_ms {
            ws.first_ms = t;
        }
        ws.last_ms = t;
        ws.extracted_lamports = ws.extracted_lamports.saturating_add(e.sol_lamports);
        let known = ws.mint_samples[..ws.distinct_mints as usize].contains(&e.mint);
        if !known && ws.distinct_mints < 5 {
            ws.mint_samples[ws.distinct_mints as usize] = e.mint;
            ws.distinct_mints += 1;
        }
        if self.tape_t0.is_none() {
            self.tape_t0 = Some(t);
        }
        self.mint_seen_mint(e.mint);
        let first = *self.mint_first_slot.entry(e.mint).or_insert(e.slot);

        // Co-entry graph: the first `early_n` distinct buyers of a mint, linked pairwise.
        if e.side == Side::Buy {
            let eb = self.early_buyers.entry(e.mint).or_default();
            if eb.len() < self.p.early_n && !eb.contains(&w) {
                for other in eb.iter() {
                    *self
                        .copartners
                        .entry(w)
                        .or_default()
                        .entry(*other)
                        .or_insert(0) += 1;
                    *self
                        .copartners
                        .entry(*other)
                        .or_default()
                        .entry(w)
                        .or_insert(0) += 1;
                }
                eb.push(w);
            }
        }

        if self.tracked.contains(&e.mint) {
            if self.mint_wallets.entry(e.mint).or_default().insert(w) {
                if e.slot <= first.saturating_add(self.p.sniper_slots) {
                    self.mint_snipers.entry(e.mint).or_default().insert(w);
                }
            }
            if self.creator_of.get(&e.mint) == Some(&w) {
                self.creator_traded.insert(e.mint);
            }
            let dq = self.windows.entry(e.mint).or_default();
            dq.push_back(Ev {
                recv: t,
                trader: w,
                side: e.side,
                lamports: e.sol_lamports,
                fee: e.fee_lamports,
                cu: e.cu_consumed,
            });
            while let Some(front) = dq.front() {
                if front.recv < t - self.p.window_300_ms {
                    dq.pop_front();
                } else {
                    break;
                }
            }
        }
    }

    fn mint_seen_mint(&mut self, mint: MintId) {
        self.seen_mints.insert(mint);
    }

    /// Drop co-entry links whose owners have been idle past `horizon_ms`, bounding the global
    /// graph on a long-running process. Not called by the reducer itself: pruning is a memory
    /// decision the operator makes, never a silent reduction in fidelity.
    pub fn prune_coentry(&mut self, now_ms: i64, horizon_ms: i64) {
        let stale: Vec<Wallet> = self
            .wallets
            .iter()
            .filter(|(_, s)| now_ms.saturating_sub(s.last_ms) > horizon_ms)
            .map(|(w, _)| *w)
            .collect();
        for w in stale {
            self.copartners.remove(&w);
        }
    }

    /// Serve the aggregates for `mint` at clock `t_dec_ms`.
    #[must_use]
    pub fn serve(&self, mint: &MintId, t_dec_ms: i64) -> FlowOutcome {
        let empty: VecDeque<Ev> = VecDeque::new();
        let dq = self.windows.get(mint).unwrap_or(&empty);
        let lo = t_dec_ms - self.p.window_300_ms;
        let events: Vec<&Ev> = dq
            .iter()
            .filter(|e| e.recv < t_dec_ms && e.recv >= lo)
            .collect();

        if events.is_empty() && !self.seen_mints.contains(mint) {
            return FlowOutcome::NoPriorFlow;
        }

        let buys: Vec<&Ev> = events
            .iter()
            .copied()
            .filter(|e| e.side == Side::Buy)
            .collect();
        // Distinct buyers. The builder's `ent_set` is a set and no output depends on entrant
        // order, so a set is both exact and immune to the quadratic membership scan.
        let buyers: BTreeSet<Wallet> = buys.iter().map(|e| e.trader).collect();
        let entrants_60s = buys
            .iter()
            .filter(|e| e.recv >= t_dec_ms - self.p.window_60_ms)
            .map(|e| e.trader)
            .collect::<BTreeSet<Wallet>>()
            .len();

        let net_flow: i64 = 0i64.saturating_sub(
            events
                .iter()
                .fold(0i64, |a, e| a.saturating_add(e.lamports)),
        );
        let fresh = buyers
            .iter()
            .filter(|w| {
                let first = self.wallets.get(*w).map(|s| s.first_ms).unwrap_or(t_dec_ms);
                t_dec_ms.saturating_sub(first) < self.p.fresh_ms
            })
            .count();
        let lookback_ms = self
            .tape_t0
            .map(|t0| t_dec_ms.saturating_sub(t0).min(self.p.lookback_ms))
            .unwrap_or(0);
        let snipers = self.mint_snipers.get(mint);
        let snip = buyers
            .iter()
            .filter(|w| snipers.is_some_and(|s| s.contains(*w)))
            .count();
        // Repeated exact SOL amounts among the window's buys.
        let mut amt: BTreeMap<i64, u32> = BTreeMap::new();
        for e in &buys {
            *amt.entry(e.lamports).or_insert(0) += 1;
        }
        let uniform = buys
            .iter()
            .filter(|e| amt.get(&e.lamports).copied().unwrap_or(0) >= 3)
            .count();
        let smart: Vec<Wallet> = buyers
            .iter()
            .copied()
            .filter(|w| {
                self.wallets.get(w).is_some_and(|s| {
                    s.extracted_lamports >= self.p.smart_sol_lamports
                        && s.distinct_mints as usize >= self.p.smart_mints
                })
            })
            .collect();
        let smart_flow: i64 = 0i64.saturating_sub(
            events
                .iter()
                .filter(|e| smart.contains(&e.trader))
                .fold(0i64, |a, e| a.saturating_add(e.lamports)),
        );
        let coent = buyers
            .iter()
            .filter(|w| {
                self.copartners.get(*w).is_some_and(|cp| {
                    cp.iter()
                        .any(|(p, c)| *c >= self.p.coentry_min && buyers.contains(p))
                })
            })
            .count();
        let mut fees: Vec<u64> = buys.iter().map(|e| e.fee).collect();
        fees.sort_unstable();
        let mut cus: Vec<u64> = buys.iter().filter_map(|e| e.cu).collect();
        cus.sort_unstable();

        let n = buyers.len();
        let nb = buys.len();
        FlowOutcome::Aggregates(FlowAggregates {
            entrants_60s: entrants_60s as u32,
            entrants_300s: n as u32,
            net_flow_sol_300s_micro: round_div_half_even(
                i128::from(net_flow) * 1_000_000,
                1_000_000_000,
            ) as i64,
            fresh_wallet_share_300s_micro: if n == 0 {
                None
            } else {
                Some(round_div_half_even(i128::from(fresh as i64) * 1_000_000, n as i128) as u32)
            },
            flow_lookback_d_tenths: round_div_half_even(i128::from(lookback_ms) * 10, 86_400_000)
                as u32,
            sniper_share_300s_micro: if n == 0 {
                None
            } else {
                Some(round_div_half_even(i128::from(snip as i64) * 1_000_000, n as i128) as u32)
            },
            bot_uniform_share_300s_micro: if nb == 0 {
                None
            } else {
                Some(round_div_half_even(i128::from(uniform as i64) * 1_000_000, nb as i128) as u32)
            },
            smart_entrants_300s: smart.len() as u32,
            smart_net_flow_sol_300s_micro: round_div_half_even(
                i128::from(smart_flow) * 1_000_000,
                1_000_000_000,
            ) as i64,
            coentry_wallets_300s: coent as u32,
            creator_trading_own_mint: self.creator_traded.contains(mint),
            entrant_fee_p90_lamports: pct90(&fees),
            entrant_cu_p50: pct50(&cus),
        })
    }
}

impl Default for FlowReducer {
    fn default() -> Self {
        Self::new()
    }
}

/// `round_half_even(num / den)` on exact integers — Python's `round()` rule, so the reducer
/// and the builder agree on every tie (the corpus's own `round(x, 6)` is a tie machine).
fn round_div_half_even(num: i128, den: i128) -> i128 {
    debug_assert!(den > 0);
    let q = num.div_euclid(den);
    let r = num.rem_euclid(den);
    let twice = r * 2;
    if twice > den || (twice == den && q % 2 != 0) {
        q + 1
    } else {
        q
    }
}

/// The builder's `pct(sorted, 0.90)`: index = clamp(round_half_even((len-1) * 9 / 10), 0, len-1).
fn pct90(sorted: &[u64]) -> Option<u64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let idx = round_div_half_even((n as i128 - 1) * 9, 10).clamp(0, n as i128 - 1) as usize;
    Some(sorted[idx])
}

/// The builder's `pct(sorted, 0.50)`.
fn pct50(sorted: &[u64]) -> Option<u64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let idx = round_div_half_even(n as i128 - 1, 2).clamp(0, n as i128 - 1) as usize;
    Some(sorted[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mint(b: u8) -> MintId {
        [b; 32]
    }
    fn wallet(b: u8) -> Wallet {
        [b; 32]
    }
    fn ev(m: u8, w: u8, side: Side, t: i64, lamports: i64, slot: u64) -> FlowEvent {
        FlowEvent {
            mint: mint(m),
            trader: wallet(w),
            side,
            slot,
            recv_unix_ms: t,
            sol_lamports: lamports,
            fee_lamports: 5_000,
            cu_consumed: Some(100_000),
        }
    }

    #[test]
    fn half_even_rounding_matches_python_on_ties() {
        assert_eq!(round_div_half_even(5, 2), 2, "2.5 -> 2");
        assert_eq!(round_div_half_even(7, 2), 4, "3.5 -> 4");
        assert_eq!(round_div_half_even(-5, 2), -2, "-2.5 -> -2");
        assert_eq!(round_div_half_even(-7, 2), -4, "-3.5 -> -4");
        assert_eq!(round_div_half_even(1, 3), 0);
        assert_eq!(round_div_half_even(2, 3), 1);
    }

    #[test]
    fn an_unseen_mint_is_no_prior_flow_not_zeros() {
        let r = FlowReducer::new();
        assert_eq!(r.serve(&mint(9), 1_000), FlowOutcome::NoPriorFlow);
    }

    #[test]
    fn a_seen_mint_with_an_empty_window_is_not_no_prior_flow() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        r.on_event(&ev(1, 7, Side::Buy, 0, -1_000_000_000, 5));
        // A clock 400 s later: the window is empty, but the mint WAS seen.
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 400_000) else {
            panic!("expected aggregates, not no_prior_flow");
        };
        assert_eq!(a.entrants_300s, 0);
        assert_eq!(a.net_flow_sol_300s_micro, 0);
        assert_eq!(a.fresh_wallet_share_300s_micro, None);
        assert_eq!(a.bot_uniform_share_300s_micro, None);
        assert_eq!(a.entrant_fee_p90_lamports, None);
    }

    #[test]
    fn net_flow_is_the_negated_tape_sum_and_shares_are_six_dp() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        r.on_event(&ev(1, 1, Side::Buy, 0, -1_000_000_000, 1));
        r.on_event(&ev(1, 2, Side::Buy, 1_000, -500_000_000, 1));
        r.on_event(&ev(1, 3, Side::Sell, 2_000, 250_000_000, 1));
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 3_000) else {
            panic!()
        };
        assert_eq!(a.entrants_300s, 2);
        assert_eq!(a.entrants_60s, 2);
        assert_eq!(
            a.net_flow_sol_300s_micro, 1_250_000,
            "1.25 SOL net in = 1_250_000 millionths"
        );
        assert_eq!(
            a.smart_net_flow_sol_300s_micro, 0,
            "nobody qualifies as smart"
        );
        assert_eq!(
            a.fresh_wallet_share_300s_micro,
            Some(1_000_000),
            "both fresh"
        );
        assert_eq!(
            a.bot_uniform_share_300s_micro,
            Some(0),
            "no repeated amounts"
        );
        assert_eq!(a.entrant_fee_p90_lamports, Some(5_000));
        assert_eq!(a.entrant_cu_p50, Some(100_000));
    }

    #[test]
    fn a_window_strictly_excludes_events_at_or_after_the_clock() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        r.on_event(&ev(1, 1, Side::Buy, 1_000, -1_000_000_000, 1));
        // The clock sits exactly on the next event: it must not be counted.
        r.on_event(&ev(1, 2, Side::Buy, 2_000, -1_000_000_000, 1));
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 2_000) else {
            panic!()
        };
        assert_eq!(a.entrants_300s, 1, "only the 1s trade is causal at 2s");
        assert_eq!(a.net_flow_sol_300s_micro, 1_000_000, "1 SOL in millionths");
    }

    #[test]
    fn sniper_share_uses_the_two_slot_rule() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        r.on_event(&ev(1, 1, Side::Buy, 0, -1_000_000, 100)); // first trade, slot 100
        r.on_event(&ev(1, 2, Side::Buy, 1, -1_000_000, 101)); // slot 101 <= 102: sniper
        r.on_event(&ev(1, 3, Side::Buy, 2, -1_000_000, 103)); // slot 103 > 102: not
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 3) else {
            panic!()
        };
        assert_eq!(a.sniper_share_300s_micro, Some(666_667), "2 of 3");
    }

    #[test]
    fn bot_uniform_share_counts_buys_whose_amount_repeats_three_times() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        for (i, w) in [1u8, 2, 3].iter().enumerate() {
            r.on_event(&ev(1, *w, Side::Buy, i as i64, -777, 1));
        }
        r.on_event(&ev(1, 4, Side::Buy, 9, -1, 1));
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 10) else {
            panic!()
        };
        assert_eq!(a.bot_uniform_share_300s_micro, Some(750_000), "3 of 4 buys");
    }

    #[test]
    fn lookback_is_capped_at_seven_days_and_reported_in_tenths() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        r.on_event(&ev(1, 1, Side::Buy, 0, -1, 1));
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 86_400_000) else {
            panic!()
        };
        assert_eq!(a.flow_lookback_d_tenths, 10, "1.0 day");
        let FlowOutcome::Aggregates(b) = r.serve(&mint(1), 30 * 86_400_000) else {
            panic!()
        };
        assert_eq!(b.flow_lookback_d_tenths, 70, "capped at 7.0 days");
    }

    #[test]
    fn the_rolling_lookback_makes_an_idle_wallet_fresh_again() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        r.on_event(&ev(1, 1, Side::Buy, 0, -1_000, 1));
        // 8 days idle (> 7 d lookback) then trades again: fresh again, per the c11 fix.
        r.on_event(&ev(1, 1, Side::Buy, 8 * 86_400_000, -1_000, 1));
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 8 * 86_400_000 + 1) else {
            panic!()
        };
        assert_eq!(a.fresh_wallet_share_300s_micro, Some(1_000_000));
    }

    #[test]
    fn coentry_requires_two_shared_early_buys() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        // Two shared early buys on another mint link wallets 1 and 2.
        r.on_event(&ev(5, 1, Side::Buy, 0, -1, 1));
        r.on_event(&ev(5, 2, Side::Buy, 1, -1, 1));
        r.on_event(&ev(5, 1, Side::Buy, 2, -1, 1));
        r.on_event(&ev(5, 2, Side::Buy, 3, -1, 1));
        // Both then appear on the served mint.
        r.on_event(&ev(1, 1, Side::Buy, 10, -1, 1));
        r.on_event(&ev(1, 2, Side::Buy, 11, -1, 1));
        let FlowOutcome::Aggregates(a) = r.serve(&mint(1), 12) else {
            panic!()
        };
        assert_eq!(a.coentry_wallets_300s, 2);
    }

    #[test]
    fn creator_trading_own_mint_is_reported_once_the_creator_trades() {
        let mut r = FlowReducer::new();
        r.track_mint(mint(1));
        r.set_creator(mint(1), wallet(42));
        r.on_event(&ev(1, 1, Side::Buy, 0, -1, 1));
        let FlowOutcome::Aggregates(before) = r.serve(&mint(1), 5) else {
            panic!()
        };
        assert!(!before.creator_trading_own_mint);
        r.on_event(&ev(1, 42, Side::Sell, 10, 5, 1));
        let FlowOutcome::Aggregates(after) = r.serve(&mint(1), 11) else {
            panic!()
        };
        assert!(after.creator_trading_own_mint);
    }
}
