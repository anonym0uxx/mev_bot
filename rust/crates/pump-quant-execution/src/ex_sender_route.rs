//! Leaf `ex_sender_route`: Helius Sender tier selection, tip budgeting and plan
//! composition.
//!
//! Helius Sender is a single submission endpoint that fans a transaction out to
//! Helius staked connections **and** the Jito block engine simultaneously (plus
//! additional builders on the Max tier). It therefore subsumes what this bot
//! currently reaches through three separate senders ([`Route::Rpc`],
//! [`Route::JitoBundle`], [`Route::Nozomi`]) behind one call, one tip and one
//! failure surface.
//!
//! ## Why this is a separate leaf rather than two more [`Route`] variants
//! Sender's cost model is structurally different from the legacy routes. Its tip
//! is a **fixed lamport charge per send**, mandatory on every submission, and an
//! exit may take several sends because of the sell ladder
//! ([`crate::ex_sell_ladder_escalate`]). A fixed per-send charge is regressive in
//! trade size, so the decision "is this tier affordable" has to be made against
//! the trade's own expected edge before any expected-value comparison is
//! meaningful. That budget check is this module's core responsibility, and it is
//! allowed to answer **no**.
//!
//! Keeping it here also holds the blast radius to zero: [`Route`], [`RouteCtx`]
//! and [`route_ev_lamports`] are untouched, so every existing caller and test
//! compiles unchanged.
//!
//! ## Known cost-model defect in the legacy path (documented, not fixed here)
//! [`route_ev_lamports`] charges a route's tip **once**. With the sell ladder an
//! exit can submit several tipped transactions, so the legacy model understates
//! fee cost by roughly the ladder depth. This module charges
//! `tip × expected_sends` and reports both numbers so the discrepancy is visible
//! rather than silently absorbed. Correcting the legacy function is a separate
//! change with a wider blast radius.
//!
//! ## Responsibility
//! 1. Size the tip budget as a fraction of the trade's expected edge.
//! 2. Pick the highest Sender tier whose **total** tip cost fits that budget, or
//!    report that no tier does — which means the trade is uneconomic to send.
//! 3. Score Sender against the best legacy route by the same integer EV shape.
//! 4. Build the endpoint query suffix and select a tip account deterministically.
//!
//! ## Constitution refs
//! - §22: all edge/slippage/fail terms in basis points, all sizes and EV in
//!   lamports, all EV math widened to `i128` / `u128`.
//! - Explicit overflow: `checked_*` / `saturating_*` / widened intermediates.
//! - Determinism: pure functions of the supplied context. No clock, no RNG, no
//!   network, no floats. Tip-account selection takes a caller-supplied seed.

use crate::ex_route_policy::{
    route_ev_lamports_with_sends, route_health_is_measured, Route, RouteCtx,
};
use crate::ex_tip_compute::compute_tip;

/// One whole unit in basis points (`1.0 == 10_000 bps`).
pub const BPS_ONE: u64 = 10_000;

/// Minimum tip for the SWQoS-only path, in lamports (0.000005 SOL).
///
/// Consistent across every Helius surface checked on 2026-08-01: the Sender docs
/// page and the zero-slot blog post both state 0.000005 SOL. This happens to
/// equal one signature's base fee, so on the SWQoS path the tip is effectively
/// free relative to the transaction it rides on.
pub const SWQOS_ONLY_MIN_TIP_LAMPORTS: u64 = 5_000;

/// Minimum tip for the Max (dual-route, Jito auction) tier as stated by the
/// **Sender documentation page**: 0.001 SOL.
pub const MAX_TIER_MIN_TIP_LAMPORTS_DOCS: u64 = 1_000_000;

/// Minimum tip for the Max tier as stated by the **dashboard and the zero-slot
/// blog post**: 0.0002 SOL.
///
/// # Unresolved discrepancy
/// Helius publishes two different Max-tier minimums — 0.001 SOL on the docs page
/// and 0.0002 SOL on the dashboard and blog. That is a 5x spread on the tier that
/// dominates cost at small trade sizes. Until it is resolved empirically by
/// submitting at each level and observing landing behaviour, the conservative
/// value ([`MAX_TIER_MIN_TIP_LAMPORTS_DOCS`]) is the default: over-reserving
/// budget declines a marginal trade, whereas under-tipping pays a tip **and**
/// fails to land, which is strictly worse.
pub const MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD: u64 = 200_000;

/// Default fraction of expected edge that may be spent on tips, in basis points.
/// `1_000 bps == 10%` of the trade's expected edge, across **all** sends.
pub const DEFAULT_TIP_BUDGET_BPS: u32 = 1_000;

/// The ten Helius Sender tip accounts (mainnet-beta).
///
/// Every entry was base58-decoded and confirmed to be exactly 32 bytes with a
/// leading-zero count matching its leading `'1'` count, and the set was checked
/// for duplicates, before being committed. `tip_accounts_are_valid_and_distinct`
/// re-proves that at test time so a future edit cannot quietly damage one.
///
/// Note the sixth entry is 43 characters rather than 44. That is correct, not a
/// truncation: a pubkey whose leading byte is small encodes to a shorter base58
/// string. Length alone is not a validity test, which is exactly why
/// [`is_valid_tip_account`] decodes instead of measuring.
///
/// A tip sent to a mistyped address is lost outright **and** the transaction
/// forfeits its priority, so nothing here may be edited from a screenshot.
pub const SENDER_TIP_ACCOUNTS: [&str; 10] = [
    "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE",
    "D2L6yPZ2FmmmTKPgzaMKdhu6EWZcTpLy1Vhx8uvZe7NZ",
    "9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta",
    "5VY91ws6B2hMmBFRsXkoAAdsPHBJwRfBht4DXox3xkwn",
    "2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD",
    "2q5pghRs6arqVjRvT5gfgWfWcHWmw1ZuCzphgd5KfWGJ",
    "wyvPkWjVZz1M8fHQnMMCDTQDbkManefNNhweYk5WkcF",
    "3KCKozbAaF75qEU33jtzozcJ29yJuaLJTy2jFdzUY8bT",
    "4vieeGHPYPG2MmyPRcYjdiDmmhN3ww7hsFNap8pVN3Ey",
    "4TQLFNWK8AovT1gFvda5jfw2oJeRMKEmw7aH6MGBJ3or",
];

/// Which Sender tier a submission uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderTier {
    /// Single SWQoS fast path. No Jito auction, no priority-tip buffer.
    /// Minimum tip [`SWQOS_ONLY_MIN_TIP_LAMPORTS`].
    SwqosOnly,
    /// Multi-pathway routing including the Jito auction and the priority tip
    /// buffer. Minimum tip is tier-dependent — see the discrepancy note on
    /// [`MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD`].
    Max,
}

/// The submission plan chosen for one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitPlan {
    /// One of the pre-existing senders, chosen by [`Route`] policy.
    Legacy(Route),
    /// Helius Sender at the given tier.
    Sender {
        /// Tier selected after the budget check.
        tier: SenderTier,
        /// Whether to request sandwich-resistant validator routing.
        mev_protect: bool,
    },
}

/// Everything the Sender decision needs. Health and configuration are explicit
/// fields so the decision stays a pure function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderCtx {
    // ── Trade / opportunity ────────────────────────────────────────────────
    /// Trade size in lamports.
    pub trade_size_lamports: u64,
    /// Expected edge in basis points of trade size.
    pub entry_edge_bps: u32,
    /// How many tipped submissions this position is expected to need. One for a
    /// buy; for an exit, the expected depth of the sell ladder. Must be >= 1;
    /// zero is treated as one.
    pub expected_sends: u32,
    /// Current observed slippage in basis points.
    pub current_slippage_bps: u32,

    // ── Policy switches ────────────────────────────────────────────────────
    /// Share of expected edge spendable on tips, in basis points.
    pub tip_budget_bps: u32,
    /// Minimum tip for the SWQoS-only path, in lamports.
    pub swqos_min_tip_lamports: u64,
    /// Minimum tip for the Max tier, in lamports.
    pub max_min_tip_lamports: u64,
    /// Request sandwich-resistant validator routing (`mev-protect=true`).
    pub mev_protect: bool,

    // ── Congestion / urgency (fed to [`compute_tip`]) ───────────────────────
    /// Network congestion as basis points of extra tip.
    pub congestion_bps: u32,
    /// Caller urgency level; each level adds 50% on top of the congestion-scaled
    /// tip.
    pub urgency: u8,

    // ── Route health ───────────────────────────────────────────────────────
    /// Whether the Sender endpoint is currently healthy.
    pub sender_healthy: bool,
    /// Recent average landing latency through Sender, in milliseconds.
    pub sender_latency_ms: u64,
    /// Recent failure rate through Sender, in basis points.
    pub sender_fail_bps: u32,
}

/// The outcome of the tier/budget decision. Every intermediate is reported so the
/// decision is auditable rather than a bare enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderDecision {
    /// Tier selected. When `economic` is false this is [`SenderTier::SwqosOnly`],
    /// the cheapest tier that exists — not a recommendation to send.
    pub tier: SenderTier,
    /// Scaled tip for a single send, in lamports.
    pub tip_lamports_per_send: u64,
    /// Sends this decision was priced against (>= 1).
    pub expected_sends: u32,
    /// `tip_lamports_per_send * expected_sends`, saturated.
    pub total_tip_lamports: u64,
    /// Lamports of expected edge available for tips.
    pub tip_budget_lamports: u64,
    /// Whether `total_tip_lamports <= tip_budget_lamports`.
    ///
    /// **False means the trade is uneconomic to submit through Sender at any
    /// tier.** The cheapest possible tip already exceeds the share of edge the
    /// policy is willing to spend. Callers must treat this as a decline, not as
    /// a suggestion to send anyway on the cheap tier.
    pub economic: bool,
}

/// Expected edge of the trade in lamports: `entry_edge_bps * size / 10_000`.
///
/// Widened to `u128` before the multiply; the divide brings it back in range.
pub fn edge_lamports(ctx: &SenderCtx) -> u128 {
    u128::from(ctx.entry_edge_bps) * u128::from(ctx.trade_size_lamports) / u128::from(BPS_ONE)
}

/// Lamports of that edge which may be spent on tips across all sends.
pub fn tip_budget_lamports(ctx: &SenderCtx) -> u64 {
    let budget = edge_lamports(ctx) * u128::from(ctx.tip_budget_bps) / u128::from(BPS_ONE);
    if budget > u128::from(u64::MAX) {
        u64::MAX
    } else {
        budget as u64
    }
}

/// Effective send count, floored at one. A zero would make every tier look free.
fn sends_or_one(expected_sends: u32) -> u32 {
    if expected_sends == 0 {
        1
    } else {
        expected_sends
    }
}

/// Total tip cost of a tier across the expected number of sends, after
/// congestion and urgency scaling.
pub fn total_tip_for_tier(tier: SenderTier, ctx: &SenderCtx) -> (u64, u64) {
    let floor = match tier {
        SenderTier::SwqosOnly => ctx.swqos_min_tip_lamports,
        SenderTier::Max => ctx.max_min_tip_lamports,
    };
    let per_send = compute_tip(floor, ctx.congestion_bps, ctx.urgency);
    let total = per_send.saturating_mul(u64::from(sends_or_one(ctx.expected_sends)));
    (per_send, total)
}

/// Choose the highest affordable Sender tier, or report that none is affordable.
///
/// Order of operations:
/// 1. Size the budget from expected edge.
/// 2. Price Max across all expected sends; take it if the endpoint is healthy
///    and the total fits.
/// 3. Otherwise price SWQoS-only; take it if the total fits.
/// 4. Otherwise return SWQoS-only with `economic == false` — the trade cannot
///    carry even the floor tip.
pub fn decide(ctx: &SenderCtx) -> SenderDecision {
    let budget = tip_budget_lamports(ctx);
    let sends = sends_or_one(ctx.expected_sends);

    let (max_per_send, max_total) = total_tip_for_tier(SenderTier::Max, ctx);
    if ctx.sender_healthy && max_total <= budget {
        return SenderDecision {
            tier: SenderTier::Max,
            tip_lamports_per_send: max_per_send,
            expected_sends: sends,
            total_tip_lamports: max_total,
            tip_budget_lamports: budget,
            economic: true,
        };
    }

    let (swqos_per_send, swqos_total) = total_tip_for_tier(SenderTier::SwqosOnly, ctx);
    SenderDecision {
        tier: SenderTier::SwqosOnly,
        tip_lamports_per_send: swqos_per_send,
        expected_sends: sends,
        total_tip_lamports: swqos_total,
        tip_budget_lamports: budget,
        economic: swqos_total <= budget,
    }
}

/// Expected net value of submitting through Sender at `tier`, in lamports.
///
/// Deliberately mirrors the term-for-term shape of [`route_ev_lamports`] so the
/// two are directly comparable:
///
/// ```text
/// gross         = entry_edge_bps * size / 10_000
/// fee           = total tip across ALL expected sends   (legacy charges one)
/// slippage_adj  = (current_slippage_bps * size / 10_000) / 2   (Max tier only)
/// latency_decay = latency_ms * size / 1_000_000
/// failure_cost  = fail_bps * size / (10_000 * 20)
/// EV = gross - fee + slippage_adj - latency_decay - failure_cost
/// ```
///
/// # Why `mev_protect` earns no credit
/// `mev-protect=true` routes around validators statistically associated with
/// sandwiching, which should reduce realised slippage. No credit is modelled for
/// it because that benefit has not been measured on this bot's own flow.
/// Crediting an unmeasured benefit is how an expected-value model learns to
/// prefer the option it was never tested on. Measure it, then model it.
///
/// The Max tier does earn the same `slippage / 2` credit the Jito route earns,
/// because it routes through the same Jito auction.
pub fn sender_ev_lamports(tier: SenderTier, ctx: &SenderCtx) -> i128 {
    let size = i128::from(ctx.trade_size_lamports);
    let gross = i128::from(ctx.entry_edge_bps) * size / i128::from(BPS_ONE);

    let (_, total_tip) = total_tip_for_tier(tier, ctx);
    let fee = i128::from(total_tip);

    let slippage_adj = if matches!(tier, SenderTier::Max) {
        (i128::from(ctx.current_slippage_bps) * size / i128::from(BPS_ONE)) / 2
    } else {
        0
    };

    let latency_decay = i128::from(ctx.sender_latency_ms) * size / 1_000_000;
    let failure_cost = i128::from(ctx.sender_fail_bps) * size / (i128::from(BPS_ONE) * 20);

    gross - fee + slippage_adj - latency_decay - failure_cost
}

/// Full result of composing the legacy route policy with the Sender decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanOutcome {
    /// The submission plan that won.
    pub plan: SubmitPlan,
    /// The Sender tier/budget decision, reported whether or not it won.
    pub sender: SenderDecision,
    /// Expected value of the best legacy route, in lamports, charged for all
    /// `expected_sends` so it is comparable with `sender_ev_lamports`.
    pub legacy_ev_lamports: i128,
    /// Expected value of the Sender plan, in lamports. Only meaningful when
    /// `sender.economic` is true, the endpoint is healthy, and
    /// `health_measured` is true.
    pub sender_ev_lamports: i128,
    /// The legacy route that scored best, for comparison and logging.
    pub legacy_route: Route,
    /// Whether the route-health inputs carry any measurement at all.
    ///
    /// When `false`, every latency and failure input was zero, which makes
    /// [`Route::Rpc`] unbeatable for the reason documented on
    /// [`route_health_is_measured`] — RPC is charged no fee, so its entire
    /// disadvantage lives in inputs that are not present. The comparison is then
    /// meaningless and Sender is **not** selected regardless of its own numbers.
    ///
    /// This is a fail-closed default, not a verdict about Sender. A caller that
    /// sees `health_measured == false` should treat the route-health feed as
    /// broken and say so, rather than reading the plan as a considered choice.
    pub health_measured: bool,
}

/// Compose the existing route policy with the Sender decision and return the
/// winner, along with every input to that comparison.
///
/// Three gates, all of which must pass before Sender can be selected:
/// 1. **Health measured.** If every latency and failure input is zero the
///    comparison is uninformative — see [`PlanOutcome::health_measured`]. Fails
///    closed to the legacy route.
/// 2. **Endpoint healthy.**
/// 3. **Within the edge budget.** [`SenderDecision::economic`].
///
/// The legacy side is priced with [`route_ev_lamports_with_sends`] using the
/// same `expected_sends` as the Sender side, so both routes are charged for the
/// full sell ladder rather than one submission. Comparing a once-charged legacy
/// route against an all-sends Sender route would have made Sender look
/// artificially expensive by exactly the ladder depth.
///
/// A strictly-greater expected value is required to displace the legacy winner,
/// so a tie preserves today's behaviour — a new submission path has to prove
/// itself, not merely match.
pub fn choose_submit_plan(route_ctx: &RouteCtx, sender_ctx: &SenderCtx) -> PlanOutcome {
    let sends = sends_or_one(sender_ctx.expected_sends);
    let legacy_route = crate::ex_route_policy::choose_route(*route_ctx);
    let legacy_ev = route_ev_lamports_with_sends(legacy_route, route_ctx, sends);

    let decision = decide(sender_ctx);
    let health_measured = route_health_is_measured(route_ctx);
    let eligible = health_measured && sender_ctx.sender_healthy && decision.economic;

    let sender_ev = if eligible {
        sender_ev_lamports(decision.tier, sender_ctx)
    } else {
        i128::MIN
    };

    let plan = if eligible && sender_ev > legacy_ev {
        SubmitPlan::Sender {
            tier: decision.tier,
            mev_protect: sender_ctx.mev_protect,
        }
    } else {
        SubmitPlan::Legacy(legacy_route)
    };

    PlanOutcome {
        plan,
        sender: decision,
        legacy_ev_lamports: legacy_ev,
        sender_ev_lamports: sender_ev,
        legacy_route,
        health_measured,
    }
}

/// Query suffix to append to the Sender endpoint for a given tier and MEV
/// setting. Returns a `'static` string so the hot path allocates nothing.
///
/// The endpoint **host** is deliberately not built here. Sender's regional
/// endpoints are plaintext `http://`, intended for colocated callers; submitting
/// a signed transaction over plaintext from outside the datacenter exposes it to
/// any on-path observer before it lands. Host selection is a configuration
/// decision and belongs with the transport, not with this policy leaf.
pub fn query_suffix(tier: SenderTier, mev_protect: bool) -> &'static str {
    match (tier, mev_protect) {
        (SenderTier::SwqosOnly, false) => "?swqos_only=true",
        (SenderTier::SwqosOnly, true) => "?swqos_only=true&mev-protect=true",
        (SenderTier::Max, false) => "",
        (SenderTier::Max, true) => "?mev-protect=true",
    }
}

/// Base58 alphabet used by Solana addresses. Note the deliberate absence of
/// `0`, `O`, `I` and `l` — the characters most often confused when a value is
/// read off a screen.
const B58_ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Decode a base58 string into exactly 32 bytes, or fail.
///
/// Long multiplication into a fixed 32-byte buffer, so no big-integer dependency
/// is needed and the crate stays std-only. A carry out of the buffer means the
/// value exceeds 32 bytes and the input is rejected.
fn b58_decode_32(s: &str) -> Option<[u8; 32]> {
    if s.is_empty() {
        return None;
    }
    let mut out = [0u8; 32];
    for c in s.bytes() {
        let digit = B58_ALPHABET.iter().position(|&a| a == c)? as u32;
        let mut carry = digit;
        for byte in out.iter_mut().rev() {
            let v = u32::from(*byte) * 58 + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
        // Anything carried past the most-significant byte does not fit in 32.
        if carry != 0 {
            return None;
        }
    }
    Some(out)
}

/// Whether `s` is a well-formed 32-byte Solana address.
///
/// Two conditions, and the second is the one that matters:
/// 1. Every character is in the base58 alphabet and the value fits in 32 bytes.
/// 2. The number of leading zero bytes in the decoded value equals the number of
///    leading `'1'` characters in the string.
///
/// Condition 2 is what makes this a real check rather than a plausibility guess.
/// A **truncated** address — the exact damage a screenshot transcription causes —
/// decodes to a smaller number that still fits in 32 bytes, producing leading
/// zero bytes that its string has no leading `'1'`s to justify. It is rejected.
///
/// This still cannot tell a genuine tip account from any other valid address.
/// It proves the value is a real pubkey, not that it is the right one.
pub fn is_valid_tip_account(s: &str) -> bool {
    let decoded = match b58_decode_32(s) {
        Some(d) => d,
        None => return false,
    };
    let leading_ones = s.bytes().take_while(|&c| c == b'1').count();
    let leading_zeros = decoded.iter().take_while(|&&b| b == 0).count();
    leading_ones == leading_zeros
}

/// Deterministically select a tip account from a caller-supplied set.
///
/// Returns `None` if the set is empty or contains an implausible address —
/// failing closed, because a tip to a mistyped address is lost outright and the
/// transaction also forfeits its priority.
///
/// # Why spread across accounts
/// A tip transfer takes a write lock on the destination account. Every bot
/// tipping the same account serialises against every other. Rotating by a
/// caller-supplied seed — a slot number or blockhash bytes, never a random
/// value, so the choice stays reproducible in replay — spreads that contention.
pub fn tip_account_from<'a>(accounts: &[&'a str], seed: u64) -> Option<&'a str> {
    if accounts.is_empty() {
        return None;
    }
    if !accounts.iter().all(|a| is_valid_tip_account(a)) {
        return None;
    }
    let idx = (seed % accounts.len() as u64) as usize;
    Some(accounts[idx])
}
