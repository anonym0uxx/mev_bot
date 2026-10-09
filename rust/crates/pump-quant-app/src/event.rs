//! The deterministic input stream the nervous system consumes.
//!
//! In laptop (Phase-A) mode these events come from a replay journal — a recorded
//! or synthetic sequence — never from a live RPC socket. The engine's determinism
//! contract is: *the same `[AppEvent]` slice always yields the same decisions and
//! the same net-SOL*, which is what makes replay a correctness authority (§22, §54).
//!
//! Events carry integer/fixed-point payloads only. Wall-clock never appears; time
//! is the explicit `tick` logical clock advanced by [`AppEvent::Tick`].

use pump_quant_domain::ids::Mint;

/// The lane an observation belongs to. Mirrors `watchlist::Lane` but is the
/// engine-facing name; the four lanes are unioned, not intersected (§71).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LaneKind {
    /// On-chain numeric microstructure (flow, liquidity, velocity).
    Numeric,
    /// Narrative / attention-velocity signal (virality, meta emergence).
    Narrative,
    /// Social-source chatter (calls, mentions) — corroboration-tier only.
    Social,
    /// Smart-money / wallet-graph activity.
    Wallet,
}

impl LaneKind {
    /// All four lanes, in canonical order.
    pub const ALL: [LaneKind; 4] = [
        LaneKind::Numeric,
        LaneKind::Narrative,
        LaneKind::Social,
        LaneKind::Wallet,
    ];

    /// A lane whose evidence, on its own, may authorise capital. Only the on-chain
    /// numeric lane qualifies; narrative, social and wallet lanes are corroboration
    /// that can raise a candidate's rank but never trigger entry alone (§29 fade-
    /// first discipline, §71 corroboration tier).
    #[must_use]
    pub const fn is_self_authorizing(self) -> bool {
        matches!(self, LaneKind::Numeric)
    }
}

/// The specific creator-attributed action carried by an [`AppEvent::CreatorAction`].
///
/// Mirrors `pump_quant_market_state::creator::CreatorEvent` one-to-one **minus the
/// slot** — the carrying `AppEvent::CreatorAction` supplies the slot once — so the
/// engine translates it without interpretation. All integer, `Copy` (§22). Creator
/// measures are corroboration-tier behavioural-risk inputs: they only ever *reduce*
/// size within an already-admitted band, never authorise or veto (§22 behavioral-
/// risk clause — high creator ownership is never an automatic binary reject).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreatorActionKind {
    /// Create/initialize: the creator's starting allocation and the token supply.
    Init {
        /// Creator's initial token allocation (base units); may be zero.
        initial_tokens: u64,
        /// Total token supply (base units), for position-fraction math.
        total_supply: u64,
    },
    /// A creator buy (accumulation).
    Buy {
        /// Tokens acquired (base units).
        tokens: u64,
        /// Quote lamports spent.
        quote_lamports: u64,
    },
    /// A creator sell (distribution / potential extraction).
    Sell {
        /// Tokens sold (base units).
        tokens: u64,
        /// Quote lamports realized.
        quote_lamports: u64,
    },
    /// A buy by a creator-linked (funded/clustered) wallet, tracked separately from
    /// the creator's own actions (§28 entity dedup).
    LinkedBuy {
        /// The linked cluster id.
        cluster: u64,
        /// Tokens acquired (base units).
        tokens: u64,
    },
}

/// Which venue a print executed on, as the PRODUCER knows it (the instruction's program id).
/// Never inferred from reserve size: the corpus's `venue=` is the tape's own label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeVenue {
    /// pump.fun bonding curve.
    PumpFun,
    /// PumpSwap AMM.
    PumpSwap,
}

/// The TRAINED-feature basis of one trade, kept apart from the execution quantities on the same event
/// (`price_fp`/`quote_lamports`/`signed_base` stay the reserve price and swap amounts). It is the corpus's own
/// definition (`renormalize_raw.py` -> `build_states_v2.py`): the resolved trader's whole-transaction native+WSOL
/// balance delta, that trader's token delta, and that trader. It feeds the trained windows (state ledger, flow
/// reducer, enrichment) ONLY. It is NEVER an execution price: no sizing, min-out/limit, fill, inventory or
/// emergency path may read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureBasis {
    /// Trader balance delta, lamports; buy negative, sell positive (the tape's `sol_lamports`).
    pub sol_lamports: i64,
    /// Trader token delta, raw units; buy positive, sell negative (the tape's `tokens_raw`).
    pub tokens_raw: i64,
    /// The corpus-resolved trader (may differ from the event's `user` on router routes).
    pub trader: [u8; 32],
}

/// One unit of input to the engine.
///
/// `Copy` and small so a journal of millions of events replays without allocation
/// churn. Every mint-bearing event names the market it concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// A decoded on-chain swap on a market. Buy pressure is signed into
    /// `signed_base` (positive = net buy). This is the only lane that produces
    /// self-authorizing evidence.
    MarketTrade {
        /// The market this trade hit.
        mint: Mint,
        /// Reserve-derived execution price of this print, fixed-point in
        /// `pump_quant_features::types::PRICE_SCALE` (1e9) units. Feeds VWAP and
        /// CVD/price-divergence — the real microstructure the numeric lane scores on
        /// (§21.7). Integer/fixed-point only (§22).
        price_fp: i128,
        /// Quote (lamport) volume of this print. Signed by `signed_base`'s sign into
        /// CVD (cumulative volume delta) — the primary order-flow-intent proxy.
        quote_lamports: u64,
        /// Pool quote-reserve depth after the trade, in lamports.
        liquidity_lamports: u64,
        /// Signed base volume of this print (positive = buy, negative = sell).
        signed_base: i64,
        /// A stable per-entity id so distinct buyers can be counted without a float.
        buyer_entity: u64,
        /// Market age at this trade, in slots.
        age_slots: u32,
        /// The TRADER'S WALLET, when the producing path knows it.
        ///
        /// `buyer_entity` is a hash, and it is the right thing for the engine's bitsets — but it
        /// is the wrong thing for anything that reasons about an ADDRESS: the flow reducer's
        /// freshness, smart-wallet and co-entry rules are address-keyed, and the corpus's
        /// identity is the pubkey. Hashing first and comparing hashes afterwards would make
        /// collisions real where they are currently negligible.
        ///
        /// `None` on the reserve-delta path, which diffs account data and has no signer; the
        /// junction's `(mint, slot)` join supplies it from the instruction print.
        trader_pubkey: Option<[u8; 32]>,
        /// The producer's receive time for this print, unix milliseconds, when the wire
        /// carried one. `None` means "not recorded", and is NOT a licence to substitute a
        /// clock: the causal state ledger's windows are keyed on this quantity, and a
        /// replay-time or logical-tick substitute would fabricate a window the tape never
        /// had. The LaserStream sidecar emits it on every notification; the PumpPortal path
        /// has no such field yet, so it passes `None` and the ledger refuses to serve.
        recv_unix_ms: Option<i64>,
        /// Solana slot of the transaction that produced this print, when the producing path knows
        /// it. `None` is "not carried", never `0`. Orders prints on-chain; it is NOT a clock.
        slot: Option<u64>,
        /// TOTAL transaction fee (lamports): 5000/signature base + priority. A TRANSACTION-level
        /// quantity repeated on every trade row of the same signature, which is how the corpus
        /// counts it (per trade row). `None` = not carried; never defaulted to 0.
        fee_lamports: Option<u64>,
        /// Compute units CONSUMED (not requested). Same per-row counting. `None` = not carried.
        cu_consumed: Option<u64>,
        /// The venue the print executed on, when the producer knew it. `None` is unknown, never a
        /// default: the state ledger then labels the print `unknown` and the join refuses it.
        venue: Option<TradeVenue>,
        /// Stable identity of the underlying on-chain event: a 128-bit digest of (signature,
        /// instruction ordinal) set ONLY by the transaction-event producer. `None` = the producer
        /// has no exact identity (legacy/derived prints); the join then falls back to its
        /// heuristic key. Identity -- never price -- is what separates two distinct trades from
        /// one repeated delivery.
        event_id: Option<u128>,
        /// Corpus-definition basis for the trained windows. `None` = this trade is OUTSIDE the corpus population
        /// (no corpus-known instruction / corpus-rejected / no balances): it is counted, never put into the
        /// trained windows under their historical definition.
        feature: Option<FeatureBasis>,
    },

    /// One corpus-definition TRADE ROW for FEATURE HISTORY only (the frozen builder's `renormalize_raw.py` row of a
    /// corpus-known buy/sell instruction): resolved trader, trader native+WSOL delta, trader token delta, venue. It
    /// carries NO reserve price, swap amount or quote: it can never reach order sizing, min-out/PRICE LIMIT, fills,
    /// inventory valuation or emergency protection. Wallet-history participation is independent of whether the
    /// market is executable; `venue_supported` records only what the producer knows about the market.
    CorpusFlowRow {
        /// The row's mint (the corpus's resolved candidate mint; may be a quote-side mint on routed USDC txs).
        mint: Mint,
        /// Venue of the instruction that produced the row.
        venue: TradeVenue,
        /// The trained basis (trader, native+WSOL delta, token delta).
        feature: FeatureBasis,
        /// Wire receive time, unix ms. `None` is refused downstream by name.
        recv_unix_ms: Option<i64>,
        /// Slot.
        slot: Option<u64>,
        /// Whole-transaction fee / CU, repeated on every row of the signature as the corpus does.
        fee_lamports: Option<u64>,
        /// Compute units consumed.
        cu_consumed: Option<u64>,
        /// Stable identity: digest of (signature, instruction index) in the `pq-corpus-row-v1` domain.
        event_id: u128,
    },

    /// A narrative attention sample for a market: how many fresh mentions arrived
    /// against how many were already active. Feeds the virality coefficient.
    NarrativeSample {
        /// The market the narrative concerns.
        mint: Mint,
        /// Mentions already active in the prior window.
        prior_active: u64,
        /// New mentions this window.
        new_mentions: u64,
    },

    /// A social-source call/mention for a market from a scored source. Corroboration
    /// only — never sufficient for entry.
    SocialCall {
        /// The market called.
        mint: Mint,
        /// Source quality, bps (0..=10_000); higher = more historically reliable.
        source_quality_bp: u32,
    },

    /// A smart-money wallet action on a market.
    WalletAction {
        /// The market acted on.
        mint: Mint,
        /// CALLER-SUPPLIED classification, consumed at corroboration tier only
        /// (discovery ranking): it can raise a mint's rank but can never
        /// authorize entry, sizing, or scaling — the gate still demands
        /// independent numeric confirmation (§28/§29 anti-copy-trading law).
        /// The production event boundary will carry raw wallet ids instead
        /// (live-replay item in the connectivity ledger); until then this
        /// field is a research-tier conclusion, never trade authority.
        followable: bool,
        /// Size of the action, lamports.
        size_lamports: u64,
    },

    /// An explicit on-chain confirmation that a market is real and sellable: **one
    /// decode of the bonding-curve account, both SOL-side reserves.** The gate
    /// REQUIRES one of these before it will admit capital to a candidate, regardless
    /// of how loud the corroboration lanes are (§29, §71).
    ///
    /// The event used to carry a single `sellable_depth_lamports`, which three
    /// producers filled with three different quantities — an external assertion, the
    /// VIRTUAL reserve, and a hardcoded 0.2 SOL — and which nothing could reconcile
    /// because the number had no declared provenance. It now carries the pair the
    /// program actually stores, so [`crate::curve_depth::CurveDepth`] can cross-check
    /// them against the venue's own identity `real_sol = virtual_sol − 30 SOL`
    /// (`docs/DEPTH_AND_MOVE_PROVENANCE_PLAN_2026-07-28.md`).
    ///
    /// **Both fields must come from the SAME snapshot.** A `real_sol` read at one slot
    /// against a `virtual_sol` read at another is not a decoder check, it is a
    /// staleness check, and staleness is the §34.3 TTL laws' job.
    OnchainConfirm {
        /// The confirmed market.
        mint: Mint,
        /// `PumpCurve::virtual_sol` — the price reserve, lamports. Seeded at 30 SOL.
        virtual_sol_lamports: u64,
        /// `PumpCurve::real_sol` — the escrowed SOL a seller can actually receive,
        /// lamports. Seeded at 0. Decoded since the first commit and, until now,
        /// consumed by nothing outside the protocol crate's own tests.
        real_sol_lamports: u64,
    },

    /// A full bonding-curve reserve observation, as decoded from the account update. Additive to
    /// `OnchainConfirm` (which carries only the SOL side and no clock): the model lane's curve
    /// plane needs all four reserves plus WHEN the update was received, and a decision may only
    /// use an observation received at or before its clock.
    CurveObserved {
        /// The curve's mint.
        mint: Mint,
        /// `virtual_sol_reserves`, lamports.
        v_sol_lamports: u64,
        /// `virtual_token_reserves`, raw token units.
        v_tokens: u64,
        /// `real_sol_reserves`, lamports.
        real_sol_lamports: u64,
        /// `real_token_reserves`, raw token units.
        real_tokens: u64,
        /// The sidecar's receive time of the account update, unix ms. `None` is refused by the
        /// cache: a local clock would be a different quantity.
        recv_unix_ms: Option<i64>,
        /// The account update's slot.
        slot: u64,
    },

    /// The pump.fun BondingCurve's MODE flag (`is_mayhem_mode`, byte 81), decoded from a real account update by
    /// `pump_quant_protocol::decode::decode_pump_curve_mode`. Emitted ONLY when the byte decoded (a canonical
    /// bool on a verified BondingCurve that is long enough to carry it); a short/foreign/noncanonical account
    /// emits NOTHING, so "never observed" stays UNKNOWN in the engine, never "not Mayhem".
    ///
    /// Operator decision 2026-10-09: Mayhem-mode coins are SKIPPED by the paper model lane (their virtual-SOL
    /// offset is program-managed and moves between trades, so the canonical curve pricing does not hold). The
    /// engine refuses entry/ADD on a Mayhem curve AND on a curve whose mode is unknown (fail-closed). Armed-only:
    /// with the model lane off this event is a no-op.
    CurveModeObserved {
        /// The curve's mint.
        mint: Mint,
        /// `is_mayhem_mode` as decoded.
        mayhem: bool,
        /// The account update's slot.
        slot: u64,
    },

    /// One PumpSwap swap decoded from the pool's own event CPI, ORIENTED to the token: the token
    /// mint (never the pool), the pool it happened in, and that pool's PRE-trade reserves as the
    /// event reports them. Consumed ONLY by the paper-model lane (legacy numeric/gate paths never
    /// see it), so it can discover and price an AMM market with no legacy priced print.
    AmmSwap {
        /// The TOKEN mint (the non-quote side).
        mint: Mint,
        /// The pool the swap executed in.
        pool: [u8; 32],
        /// `true` when this is the pool the mint's canonical pump.fun migration derives
        /// (`pool-authority` PDA, index 0, WSOL quote). Only that pool feeds reserves.
        pool_is_canonical: bool,
        /// `true` when the pool's quote side is WSOL. A USDC-quoted pool is excluded by name.
        quote_is_wsol: bool,
        /// Pool token-side vault balance BEFORE this swap, raw token units.
        token_reserve_pre: u64,
        /// Pool quote-side vault balance BEFORE this swap, lamports (valid only if `quote_is_wsol`).
        quote_reserve_pre: u64,
        /// Total fee rate the event applied (lp + protocol + creator), basis points. `None` when
        /// the event predates the creator-fee tail: never defaulted to 0.
        fee_bps: Option<u32>,
        /// (lp, protocol, creator) bps as the event reported them, for per-component ceil rounding.
        fee_parts: Option<(u32, u32, u32)>,
        /// `Pool::virtual_quote_reserves` from the event (verified layouts only); `None` => the
        /// executable quote is unsupported. Never defaulted to 0.
        virtual_quote: Option<u64>,
        /// The event's `(cashback_fee_basis_points, cashback)` with its layout provenance: KNOWN (the layout
        /// carries it; zero is a known zero), MISSING (older layout), UNSUPPORTED (unknown layout) or
        /// NOT_RECORDED (an older event-stream schema never wrote it). Never an `Option` of zero.
        cashback: pump_quant_protocol::pumpswap_event::CashbackField,
        /// `true` when the trader BOUGHT the token.
        is_buy: bool,
        /// Token amount the trader received (buy) or gave (sell), raw units.
        token_amount: u64,
        /// Quote the trader paid (buy, all fees in) or received (sell, net of fees), lamports.
        quote_lamports: u64,
        /// The trader.
        trader: [u8; 32],
        /// Transaction fee / CU consumed (per trade row), as on `MarketTrade`.
        fee_lamports: Option<u64>,
        /// Compute units consumed.
        cu_consumed: Option<u64>,
        /// Wire receive time, unix ms. `None` is refused by the cache.
        recv_unix_ms: Option<i64>,
        /// Slot.
        slot: u64,
    },

    /// A token launch: who created the mint and when the launch was RECEIVED. First observation
    /// of a trade is not a launch; this is the only event that establishes creator history.
    LaunchObserved {
        /// The launched mint.
        mint: Mint,
        /// The creator's wallet address.
        creator: [u8; 32],
        /// The launch event's receive time, unix ms.
        launch_unix_ms: i64,
    },

    /// A launch established from CHAIN HISTORY (bootstrap RPC), not observed on the live feed.
    /// Kept apart from [`AppEvent::LaunchObserved`]: it does NOT feed the trained
    /// `creator_past_launches` registry (a capture-window receive-order count), and it never
    /// stands in for a first observed trade. `retrieved_unix_ms` is when WE learned it; a decision
    /// earlier than that cannot use it.
    LaunchFromChain {
        /// The launched mint (verified create of this mint by the pump.fun program).
        mint: Mint,
        /// Creator per the trained definition (tx account 0, verified equal to the create `user`).
        creator: [u8; 32],
        /// Slot of the create transaction.
        slot: u64,
        /// Chain block time, unix seconds; `None` when the source omitted it.
        block_time_s: Option<i64>,
        /// When the evidence was retrieved, unix ms.
        retrieved_unix_ms: i64,
    },

    /// A deterministic, **on-chain-led** category assignment for a market. The
    /// category classifier ran UPSTREAM on the token's decoded name/symbol (an
    /// `[S]`-boundary concern in `token_ingest`); the engine sees only the resolved
    /// integer `category_id` (0 = UNCLASSIFIED), never a string — factual category
    /// state is never populated by social interpretation (§21.4, criterion 83/§85).
    /// Feeds the per-category `MetaRotationState` measures (launches, then flow).
    TokenMetadata {
        /// The market this metadata describes.
        mint: Mint,
        /// Resolved category id (0 = UNCLASSIFIED).
        category_id: u64,
        /// Taxonomy version the id was assigned under (non-retroactive, criterion 81).
        taxonomy_version: u32,
        /// Creator/deployer entity id (upstream on-chain attribution).
        creator: u64,
        /// Slot at which the metadata was observed (caller time; no wall-clock).
        slot: u64,
    },

    /// A creator-attributed on-chain action for a market, feeding the `CreatorState`
    /// reducer. Corroboration-tier: creator measures only ever *reduce* size within
    /// an admitted band, never authorise or veto (§22 behavioral-risk clause).
    CreatorAction {
        /// The market acted on.
        mint: Mint,
        /// The specific creator action (mirrors `creator::CreatorEvent`).
        kind: CreatorActionKind,
        /// Slot of the action (caller-supplied time).
        slot: u64,
    },

    /// The market migrated from its bonding curve to a pool (graduation). Flips the
    /// market's venue-mechanics **phase** (§21.7 phase asymmetry / §24 hold-horizon:
    /// curve and pool are never pooled into one model): exit-cost pricing, hazard
    /// conditioning, and lifecycle parameters consult the phase from this point on.
    Migration {
        /// The graduated market.
        mint: Mint,
        /// Slot of the migration (caller-supplied time).
        slot: u64,
    },

    /// **Rev-14 wangr intelligence** — auxiliary on-chain facts about a market
    /// that are NOT part of the trade stream or the metadata category system.
    /// Carries the token-standard (Legacy SPL vs Mayhem/token2022) and the
    /// symbol string length, both sourced from decoded account/metadata data
    /// upstream. The engine stores these per-mint and enriches the gate's
    /// `Features` snapshot at gate_evaluate time. Integer-only (§22), never
    /// wall-clock. A market that never receives this event leaves the fields
    /// at their zero-sentinel defaults — the gate filters are no-ops.
    MarketAuxiliary {
        /// The market this auxiliary data describes.
        mint: Mint,
        /// Token standard: 1=legacy SPL, 2=mayhem/token2022.
        /// Wangr study: legacy tokens graduate 5× more often.
        token_standard: u8,
        /// Symbol string length (character count).
        /// Wangr study: 4-6 char symbols most common among graduated tokens.
        symbol_len: u8,
    },

    /// **Narrative precondition** (operator ruling 2026-09-26) — the resolved
    /// narrative verdict for a market's NAME. Produced off the decision path by
    /// the resolution lane (`dynamic_lexicon` + the alias stage counters, or the
    /// model lane for names the vocabulary has never seen) and carried here so
    /// the engine can enrich the gate's `Features` at gate_evaluate time.
    ///
    /// Integer-only (§22), never wall-clock. A market that never receives this
    /// event leaves the gate fields at their zero sentinel, which the gate treats
    /// as UNOBSERVED and ADMITS — the precondition can only act on a verdict that
    /// exists. `lexicon_version` travels with the verdict so an assignment stays
    /// auditable against the vocabulary that produced it (criterion 81).
    NarrativeResolved {
        /// The market this verdict describes.
        mint: Mint,
        /// `NarrativeVerdict` discriminant: 1=Eligible, 2=Saturated, 3=NoAttach,
        /// 4=Throwaway, 5=Unresolved. (0 is never sent — it means unobserved.)
        verdict: u8,
        /// `AliasStage` discriminant: 1=Novel, 2=Rising, 3=Cresting,
        /// 4=Saturated. 0 = the stage was not observed.
        stage: u8,
        /// `NarrativeFamily` discriminant: 1..=8. 0 = unclassified.
        family: u8,
        /// The lexicon version the verdict was resolved against. 0 = unobserved.
        lexicon_version: u32,
    },

    /// **Rev-14 wangr intelligence** — a wall-clock time signal. The engine is
    /// a pure tick-based state machine (§22) and NEVER reads wall-clock itself;
    /// this event is the sole channel through which the caller can inform the
    /// engine of the current day-of-week and hour-of-day in UTC. The engine
    /// stores the latest values and uses them to enrich the gate's `Features`
    /// snapshot. A tape that never feeds this event leaves `dow=0, hour_utc=255`
    /// (sentinel) and all time-based filters are no-ops — byte-identical to
    /// the prior behavior (golden-tape safe).
    TimeSignal {
        /// Day of week: 1=Mon … 7=Sun (ISO-8601). 0 is never fed.
        dow: u8,
        /// Hour of day in UTC: 0-23.
        hour_utc: u8,
    },

    /// **Rev-19 on-chain feedback**: our own buy transaction landed on-chain.
    /// Fed by the daemon's `getSignaturesForAddress` poller when a pending buy
    /// signature is confirmed. The engine uses this to reconcile the paper
    /// position with on-chain reality and mark it as on-chain confirmed.
    /// Execution evidence for ONE paper-model order (report ingestion). Bound to the order id, the
    /// attempt and the quantity; `filled` is `Some((entry_price_fp, reserve_sol_lamports))` for a
    /// fill and `None` for not-filled. Mint is for routing only and is cross-checked against the log.
    ModelOrderEvidence {
        mint: Mint,
        order_id: u64,
        attempt: u32,
        clip_lamports: u64,
        filled: Option<(u64, u64)>,
    },
    /// Cumulative management-sell report (REDUCE / EXIT, or ADD with `value` = cumulative notional spent): the
    /// TOTAL the order has filled, so applying it is idempotent. Bound to order id and mint.
    ModelMgmtReport {
        mint: Mint,
        order_id: u64,
        /// 0 = reduce, 1 = exit, 2 = add. Must match the issued order.
        action: u8,
        /// The order's issued intended quantity, restated by the executor. Must match.
        intended: u64,
        cumulative_tokens: u64,
        /// Cumulative gross proceeds (lamports); for an ADD, cumulative notional spent.
        cumulative_gross: u64,
        /// Cumulative all-in fees (lamports); must be 0 for an ADD.
        cumulative_fees: u64,
        /// AUTHORITATIVE TERMINAL evidence from the executor: the order is FINAL at these totals and nothing more
        /// can execute (definitively no execution when the totals are 0; a definitively cancelled remainder
        /// otherwise). `false` = execution of any remainder is still unknown. A timeout or a model verdict never sets it.
        terminal: bool,
    },
    OurBuyConfirmed {
        /// The mint that was bought.
        mint: Mint,
        /// The on-chain transaction signature (raw 64 bytes).
        signature: [u8; 64],
        /// Slot of the confirmation (caller-supplied).
        slot: u64,
    },

    /// **Rev-19 on-chain feedback**: our own buy transaction failed on-chain.
    /// The engine reverses the paper position (closes it) and records the
    /// irrecoverable fee loss. Tokens were NOT received.
    OurBuyFailed {
        /// The mint that failed to buy.
        mint: Mint,
        /// The failed transaction signature.
        signature: [u8; 64],
        /// Compact error classification: 0=unknown, 1=simulation_failure,
        /// 2=program_error (incl. 6062 BuybackVault), 3=timeout, 4=insufficient_funds.
        err_code: u8,
        /// Slot of the failure (caller-supplied).
        slot: u64,
    },

    /// **Rev-19 on-chain feedback**: our own sell transaction landed on-chain.
    /// The exit is now real — SOL was recovered. The paper PnL recorded in
    /// `book_exit` is confirmed as on-chain truth.
    OurSellConfirmed {
        /// The mint that was sold.
        mint: Mint,
        /// The on-chain transaction signature.
        signature: [u8; 64],
        /// Slot of the confirmation.
        slot: u64,
    },

    /// **Rev-19 on-chain feedback**: our own sell transaction failed on-chain.
    /// Tokens remain in the wallet. The paper exit was recorded but the SOL
    /// was NOT recovered. The daemon logs this for manual recovery or retry.
    OurSellFailed {
        /// The mint that failed to sell.
        mint: Mint,
        /// The failed transaction signature.
        signature: [u8; 64],
        /// Compact error classification (same as OurBuyFailed).
        err_code: u8,
        /// Slot of the failure.
        slot: u64,
    },

    /// Advance the logical clock by one tick. Recency decay, TTL pruning and the
    /// reflection cadence are all measured in ticks — never wall-clock.
    Tick,
}

impl AppEvent {
    /// The market this event concerns, if any (`Tick` concerns none).
    #[must_use]
    pub const fn mint(&self) -> Option<Mint> {
        match self {
            AppEvent::MarketTrade { mint, .. }
            | AppEvent::NarrativeSample { mint, .. }
            | AppEvent::SocialCall { mint, .. }
            | AppEvent::WalletAction { mint, .. }
            | AppEvent::OnchainConfirm { mint, .. }
            | AppEvent::CurveObserved { mint, .. }
            | AppEvent::CurveModeObserved { mint, .. }
            | AppEvent::AmmSwap { mint, .. }
            | AppEvent::CorpusFlowRow { mint, .. }
            | AppEvent::LaunchObserved { mint, .. }
            | AppEvent::LaunchFromChain { mint, .. }
            | AppEvent::TokenMetadata { mint, .. }
            | AppEvent::CreatorAction { mint, .. }
            | AppEvent::Migration { mint, .. }
            | AppEvent::MarketAuxiliary { mint, .. }
            | AppEvent::NarrativeResolved { mint, .. }
            | AppEvent::ModelOrderEvidence { mint, .. }
            | AppEvent::ModelMgmtReport { mint, .. }
            | AppEvent::OurBuyConfirmed { mint, .. }
            | AppEvent::OurBuyFailed { mint, .. }
            | AppEvent::OurSellConfirmed { mint, .. }
            | AppEvent::OurSellFailed { mint, .. } => Some(*mint),
            AppEvent::TimeSignal { .. } | AppEvent::Tick => None,
        }
    }
}
