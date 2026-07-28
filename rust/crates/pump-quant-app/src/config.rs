//! Runtime configuration for the nervous system.
//!
//! Every threshold, weight, cadence and cost that the discovery→gate→scalp loop
//! consults lives here as a **named, operator-supplied field** — never as a magic
//! number buried in a decision path. This is the constitution's hardcoded-parameter
//! law (§22 / no-hardcode): the engine's logic reads `cfg.<name>`, and the values
//! come from a config file the operator controls. A `dev_portable()` constructor is
//! provided for tests and laptop dry-runs; it is explicitly labelled a *starting
//! point*, not a baked-in constant, and every field it sets can be overridden by a
//! loaded file.
//!
//! Parsing is dependency-free (a tiny `key = value` integer reader) so this crate
//! stays free of serde/toml and remains trivially auditable.

use core::fmt;

/// Which fill semantics the paper engine uses when it simulates a scalp.
///
/// Mirrors `pump_quant_simulator::fill::FillMode` but is parsed from config so the
/// operator chooses the epistemic mode without recompiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillModeCfg {
    /// Mode A — causal signal replay, makes no profitability claim.
    SignalReplay,
    /// Mode B — deterministic optimistic mechanical ceiling.
    OptimisticCeiling,
    /// Mode C — calibrated adversarial execution at realistic severity.
    AdversarialRealistic,
    /// Mode C — calibrated adversarial execution at pessimistic (stress) severity.
    AdversarialPessimistic,
}

impl FillModeCfg {
    fn from_code(v: i64) -> Option<Self> {
        match v {
            0 => Some(Self::SignalReplay),
            1 => Some(Self::OptimisticCeiling),
            2 => Some(Self::AdversarialRealistic),
            3 => Some(Self::AdversarialPessimistic),
            _ => None,
        }
    }
}

/// §99/§102 bound on the operator-supplied brain persistence path. A fixed-size
/// inline buffer keeps [`Config`] `Copy` (the whole envelope is a value type, and
/// the §19 strategy-identity digest folds its `Debug` encoding), so a path can be
/// carried without dragging an allocation into the config plane.
pub const BRAIN_PATH_CAP: usize = 96;

/// An operator-supplied filesystem path, inline and `Copy`.
///
/// Only the brain's LAW B5 journal/snapshot base path uses this today. `Debug`
/// renders the path as a string rather than 96 bytes so the §19 config-identity
/// seed stays compact and human-auditable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CfgPath {
    bytes: [u8; BRAIN_PATH_CAP],
    len: u8,
}

impl CfgPath {
    /// The empty path — persistence disarmed.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            bytes: [0u8; BRAIN_PATH_CAP],
            len: 0,
        }
    }

    /// Build from a string, refusing anything longer than [`BRAIN_PATH_CAP`]
    /// (§18: a truncated path is a wrong path, so it fails loud instead).
    #[must_use]
    pub fn from_str_checked(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.len() > BRAIN_PATH_CAP {
            return None;
        }
        let mut bytes = [0u8; BRAIN_PATH_CAP];
        bytes[..b.len()].copy_from_slice(b);
        Some(Self {
            bytes,
            // `b.len() <= BRAIN_PATH_CAP <= u8::MAX`, so the cast is exact.
            len: b.len() as u8,
        })
    }

    /// Whether the path is unset.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The path as a string slice. Always valid UTF-8: the only constructor takes
    /// a `&str` and copies whole bytes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

impl fmt::Debug for CfgPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// The full parameter envelope the engine runs under.
///
/// All fields are integers or fixed-point basis points — no floating point ever
/// reaches an outcome decision (§22). Fields are grouped by the stage that reads
/// them so the audit trail from config → decision is direct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    // ---- discovery / watchlist ----
    /// Hard cap on the watchlist size (§99 bounded state).
    pub watchlist_capacity: usize,
    /// Recency TTL, in logical ticks, after which a candidate decays out of ranking.
    pub watchlist_ttl_ticks: u64,
    /// How many top-ranked candidates are promoted to the gate each evaluation.
    pub promote_k: usize,
    /// Minimum rank a candidate must reach to be promoted at all.
    pub promote_min_rank: u64,

    // ---- discovery lane scoring ----
    /// Minimum order-flow imbalance (bps, 0..=10_000) the numeric lane requires
    /// before it will emit a self-authorizing candidate: real net-buy conviction,
    /// not marginal noise (§21.7 sign-agreement gate). Operator-tunable; higher =
    /// only stronger buy flow is discovered.
    pub numeric_ofi_min_bp: u32,
    /// Cross-lane scale applied to the wallet lane's cumulative-size score so it is
    /// comparable with the other lanes' score magnitudes. Operator-tunable weight —
    /// it governs which mints the wallet lane pushes toward promotion.
    pub wallet_score_scale: u64,
    /// Narrative virality band, in the narrative crate's fixed-point unit, at/above
    /// which a mint is classified `Virality` (the highest stage). Governs the
    /// narrative lane's discovery score, so it is operator-tunable, not baked in.
    pub narrative_stage_hi_fp: u64,
    /// Narrative virality band at/above which a mint is classified `Emergence`
    /// (below `hi`). Below this it is `Formation`. Operator-tunable.
    pub narrative_stage_lo_fp: u64,
    /// Confirmed-market set capacity as a multiple of `watchlist_capacity` (§99
    /// bound). Operator-tunable: too small and a valid on-chain confirmation can be
    /// evicted before its candidate clears the gate.
    pub confirmed_capacity_mult: usize,

    // ---- corroboration / gate ----
    /// Expected favourable move, in bps, credited to a confirmed candidate. Feeds
    /// the economic size-band. Operator-tuned, never inferred from a single trade.
    pub gate_expected_move_bps: u32,
    /// **DECISION-INERT since the cost-model unification (2026-07-28).** The gate's
    /// fixed per-attempt cost is now derived from
    /// [`crate::cost_model::FIXED_LAMPORTS_PER_LEG`] and `gate_exit_tranches`.
    /// Retained so an existing operator config file still parses.
    pub gate_base_fixed_lamports: u64,
    /// Expected send-failure rate, bps, inflating the effective fixed cost. STILL
    /// LIVE: it is a property of the sender, not of the venue's fee schedule.
    pub gate_fail_rate_bps: u32,
    /// **No longer the gate's protocol fee** — that is derived per candidate from the
    /// venue's tiered schedule ([`crate::cost_model::gate_protocol_bps`]). This field
    /// survives as the §38 Mode-C adversarial retry-slippage severity, which is the
    /// only thing that still reads it.
    pub gate_protocol_bps: u32,
    /// Safety margin, bps, demanded on top of costs before admitting size.
    pub gate_margin_bps: u32,
    /// **DECISION-INERT since the cost-model unification (2026-07-28).** The gate's
    /// impact denominator is now DERIVED per candidate from the market's own SOL-side
    /// reserve ([`crate::cost_model::impact_den_for`]), because a static denominator is
    /// right for exactly one pool depth and silently wrong for every other. Retained
    /// only so an existing operator config file still parses; nothing reads it.
    pub gate_impact_den: u64,
    /// How many transactions the gate assumes an exit is split across. The fixed
    /// per-signature cost is charged `1 + gate_exit_tranches` times
    /// ([`crate::cost_model::gate_base_fixed_lamports`]); fee and own-impact are
    /// tranche-invariant, so this scales the fixed term and nothing else. Default 3 —
    /// the §24 LAW 2 ladder's maximum rung count, which is the conservative end of the
    /// 1-to-4 range a real position takes (§54).
    pub gate_exit_tranches: u32,

    // ---- scalp / paper fill ----
    /// Fill semantics for the paper engine.
    pub fill_mode: FillModeCfg,
    /// **DECISION-INERT since the cost-model unification (2026-07-28).** Both legs'
    /// venue fee is the tiered per-market rate from
    /// [`crate::cost_model::venue_fee_bps_per_leg`]; both legs' fixed cost is
    /// [`crate::cost_model::FIXED_LAMPORTS_PER_LEG`]. These four fields were the
    /// lifecycle half of the two-model split — an operator could set them to disagree
    /// with the gate, and did (100 bps a leg against the gate's 450 round trip; 10_000
    /// lamports a tranche against the gate's ~100_000 a leg). Retained so an existing
    /// operator config file still parses; nothing on the decision path reads them.
    pub entry_fee_bps: u32,
    /// See [`Config::entry_fee_bps`] — decision-inert.
    pub exit_fee_bps: u32,
    /// See [`Config::entry_fee_bps`] — decision-inert.
    pub entry_tip_lamports: u64,
    /// See [`Config::entry_fee_bps`] — decision-inert.
    pub exit_tip_lamports: u64,
    /// Impact `k` (bps) for the simulator's constant-product impact model.
    pub sim_impact_k_bps: u32,

    // ---- backtest fidelity: exact curve fill + landing lag (criterion 103) ----
    /// Price fills through the EXACT constant-product curve math in
    /// [`crate::curve_fill`] instead of the last observed print.
    ///
    /// The engine today opens and closes positions at
    /// `numeric.latest_price_fp(mint)` — the marginal/spot price of the last swap it
    /// saw. On a pump.fun bonding curve (and the PumpSwap pool it migrates into) that
    /// price is unreachable by any order of non-zero size: our own order walks the
    /// curve and fills at the strictly worse AVERAGE price along it. Because we hold
    /// the reserves, that average is exactly computable, so modelling it as "spot"
    /// is not a conservative simplification — it is a systematic, size-proportional
    /// overstatement of every entry and every exit. A 0.1 SOL bite into the canonical
    /// 30 SOL virtual reserve fills 33 bps worse than spot on the way in and ~33 bps
    /// worse on the way out; a backtest that charges neither books ~65 bps of
    /// notional per round trip that the wallet would never have seen.
    ///
    /// **DEFAULT `false`.** The curve math is built, tested and pinned
    /// ([`crate::curve_fill`]) but is deliberately NOT yet read by any decision path.
    /// Arming it changes fill prices and therefore net SOL, so it is a separate gated
    /// change with its own A/B — leaving it off here keeps this addition a §19
    /// seed-only digest move with every golden decision number unchanged.
    pub curve_exact_fill_enable: bool,

    // ---- operator target band + per-candidate expected move (both default OFF) ----
    /// **SELECTION LAW (default OFF).** Restrict admission to markets whose
    /// bonding-curve market cap lies in `[mcap_band_lo_lamports, mcap_band_hi_lamports]`.
    /// Market cap is `vsol^2 / curve_state::MCAP_DIVISOR_LAMPORTS` — derivable from
    /// `liquidity_lamports` alone, so this costs no new ingestion (`curve_state`).
    pub mcap_band_enable: bool,
    /// Inclusive band floor, lamports of MARKET CAP (not reserve).
    ///
    /// Default 118.42 SOL = the operator's $9k at the SOL/USD conversion recorded in
    /// `docs/BAND_THESIS_2026-07-28.md`. **SOL-denominated deliberately**: the objective
    /// is net SOL, every venue cost is SOL-denominated, and a USD band would make the
    /// journal digest a function of an external price feed (§22). If SOL moves
    /// materially the operator re-pins this; the bot never guesses.
    pub mcap_band_lo_lamports: u64,
    /// Inclusive band ceiling, lamports of MARKET CAP. Default 263.16 SOL = $20k at the
    /// recorded conversion, which is 72% of the way to graduation and safely
    /// pre-migration.
    pub mcap_band_hi_lamports: u64,
    /// **BENEFIT-SIDE LAW (default OFF).** Price admission on the per-candidate
    /// stratified estimate from `expected_move` instead of the global constant
    /// `gate_expected_move_bps`. Ships DISARMED with an EMPTY table, so every lookup
    /// refuses and the constant is used — byte-identical to the pre-model engine.
    /// Arming requires a calibrated table and the full A-11 leg set.
    pub expected_move_model_enable: bool,
    /// Minimum episodes in a curve-progress stratum before it may answer (cf. §46).
    pub expected_move_min_sample: u32,
    /// Shrinkage pseudo-count toward the cold-start prior — the same hierarchical
    /// partial-pooling weight `conditional_edge_bps` uses.
    pub expected_move_prior_weight: u32,
    /// Slots of expected landing lag applied between OBSERVING a market state and
    /// FILLING against it (criterion 103).
    ///
    /// Criterion 103 requires every fill be evaluated at the expected LANDING state,
    /// never the observation state. The engine currently does the opposite: it
    /// observes a swap and fills at that same print, in the same slot — which is
    /// look-ahead, because the transaction that produced the print has already landed
    /// and ours has not been sent. Real landing is at least one slot (~400 ms) later,
    /// during which the reserves move against whichever side we are taking. `1` is
    /// the honest minimum for a same-block-submit assumption; larger values model a
    /// congested leader or a slower sender.
    ///
    /// **DEFAULT `0`.** Zero reproduces today's same-slot (look-ahead) behaviour
    /// byte-for-byte, so this field's arrival moves only the §19 config-identity
    /// digest seed and no decision. Raising it is a separate gated change.
    pub fill_landing_slots: u64,

    // ---- bankroll / dynamic sizing (§33 Layer 1, delta-§1) ----
    /// PAPER/REPLAY starting-bankroll SEED ONLY, lamports — NEVER the live bankroll.
    /// Live trading sources the bankroll base from the reconciled on-chain wallet
    /// balance (Phase-B; SERVER_BUILD_MANIFEST §7), armed through
    /// [`Engine::new_live_reconciled`](crate::engine::Engine::new_live_reconciled) /
    /// [`Engine::set_live_bankroll`](crate::engine::Engine::set_live_bankroll); this
    /// config constant can never back a live order. The engine makes that structural
    /// via [`BankrollOrigin`](crate::engine::BankrollOrigin): Paper/Replay carry a
    /// `PaperSeed(this)`, a live path carries a `LiveReconciled(wallet)`, and live
    /// sizing must pass the fail-closed `require_live_verified()` guard — which errors
    /// on a paper seed. ANY amount for paper/replay: every sizing limit derives from
    /// `deployable = bankroll − survival_floor`, so the same config serves 0.75, 2,
    /// 10 or 100 SOL (scale-invariant until the per-market cost floor x_min carves out
    /// the venue-viability region).
    pub bankroll_initial_lamports: u64,
    /// Operator-directed ABSOLUTE minimum size for EVERY individual order the engine
    /// emits — initial entry, each probe, and each probe→confirm→scale-in add
    /// (criterion 112 operator floor / Amendment A-6). No bet below this is ever
    /// placed: the economic band's `x_min` is lifted to `max(this, x_min)`, a
    /// risk/Kelly-arbitrated size below it is clamped UP to it (only if that still
    /// fits every hard cap — else the trade is REFUSED, never shrunk), and a target
    /// that cannot split into two ≥floor bites opens as a single ≥floor bite. Set to
    /// `0` to disable the floor entirely (restores the pre-A-6 sub-`x_min`
    /// paid-information probe path). NEVER hardcoded elsewhere — the engine reads
    /// this field, and all other limits derive from the live bankroll, not this.
    pub min_trade_size_lamports: u64,
    /// Survival-floor fraction of the verified starting balance, bps. The floor is
    /// `max(0.5 SOL, fraction × start)` (delta-§1) and is NEVER risked or spent.
    pub floor_fraction_bps: u32,
    /// Per-position fraction of deployable capital, bps (pre-haircut). Deep-
    /// fractional Kelly: ≈quarter-Kelly of the WORST still-positive calibration
    /// (research doc), so growth stays positive under every plausible live edge.
    pub f_base_bp: u32,
    /// Total at-risk cap across ALL open positions, bps of deployable capital.
    /// Correlated-cluster bound: memecoin positions rug together, so the total cap
    /// — not per-position f — is the binding instrument.
    pub total_risk_cap_bp: u32,
    /// Hard cap on concurrently open positions (jointly consistent with the two
    /// fractions above: max_concurrent × f_base ≈ total_risk_cap).
    pub max_concurrent_positions: usize,
    /// Small-bankroll escape valve: promotion of a sub-fraction size UP to x_min is
    /// permitted only when x_min ≤ this fraction (bps) of deployable capital —
    /// below the worst-calibration Kelly, far below its growth-zero crossing.
    pub x_min_promote_cap_bp: u32,
    /// Promotion to x_min is refused when the corroboration haircut is below this
    /// (a trade the risk tiers marked down must never be sized UP).
    pub promote_min_haircut_bp: u32,
    /// Drawdown ratchet tiers vs the realized high-water mark, bps: past tier1 the
    /// per-position fraction halves, past tier2 it quarters, past tier3 only the
    /// probe fraction trades (Grossman–Zhou surplus shape, step-quantized).
    pub dd_tier1_bp: u32,
    /// Second drawdown tier (see `dd_tier1_bp`).
    pub dd_tier2_bp: u32,
    /// Third drawdown tier: survival mode.
    pub dd_tier3_bp: u32,
    /// Probe-only fraction (bps of deployable) used in the deepest drawdown tier.
    pub probe_f_bp: u32,
    /// Probe fraction of the arbitrated size opened immediately (§33 probe→confirm→
    /// scale); the remainder scales in on deterministic confirmation.
    pub probe_frac_bp: u32,
    /// §23 arbitration floor: candidates whose conditional expected net SOL is
    /// below this never win a slot.
    pub arb_min_expected_net_lamports: i64,

    // ---- toxicity gate (VPIN-X, §21.7) ----
    /// Bucket-cap floor, lamports (dust spam cannot manufacture buckets).
    pub vpin_v_min_lamports: u64,
    /// Bucket-cap ceiling, lamports.
    pub vpin_v_max_lamports: u64,
    /// Completed buckets before VPIN is trusted.
    pub vpin_min_buckets: usize,
    /// Ticks without a completed bucket after which VPIN is absent.
    pub vpin_stale_ticks: u64,
    /// Graded-haircut onset (bps VPIN, sell-leaning flow only).
    pub vpin_warn_bp: u32,
    /// Deep-haircut tier (bps VPIN).
    pub vpin_toxic_bp: u32,
    /// Extreme tier: veto/exit-escalation with sell dominance (bps VPIN).
    pub vpin_veto_bp: u32,
    /// Sell share (bps) above which the extreme tier vetoes and escalates exits.
    pub vpin_sell_dom_bp: u32,

    // ---- tape regime (Roll-sign, §21.7) ----
    /// Regime deadband: rho (bps) at/above which the tape is TREND.
    pub roll_trend_bp: i64,
    /// Regime deadband: rho (bps) at/below which the tape is REVERT (negative).
    pub roll_revert_bp: i64,
    /// Raised numeric-lane OFI bar under REVERT (only a violent imbalance — the
    /// regime breaking — qualifies for a momentum entry on a mean-reverting tape).
    pub revert_ofi_min_bp: u32,
    /// Entry-size multiplier under REVERT (bps of 10_000; ≤ identity).
    pub revert_size_mult_bp: u32,

    // ---- evidence staleness (§29.6/§34.3) ----
    /// Discovery-lane evidence TTL, ticks: evidence older than this emits nothing
    /// (dead tapes and week-old calls must not keep ranking).
    pub lane_evidence_ttl_ticks: u64,
    /// On-chain confirmation TTL, ticks: a confirm older than this no longer
    /// authorizes entry (depth proven long ago is not depth now).
    pub confirm_ttl_ticks: u64,
    /// Discovery score granted to a fresh creation sighting (CreationSniper lane).
    pub creation_score: u64,
    /// Ticks a creation sighting stays discoverable before it must earn flow.
    pub creation_ttl_ticks: u64,

    // ---- held-position exit lifecycle (crit-102: every trigger operator-set) ----
    /// Catastrophic hard stop below entry, bps drawdown.
    pub lc_hard_sl_bps: u32,
    /// Minimum trailing width from peak, bps.
    pub lc_trail_base_bps: u32,
    /// Trail widening divisor: trail grows with (peak−1×)/k.
    pub lc_trail_k_div: u32,
    /// Maximum trailing width, bps.
    pub lc_trail_max_bps: u32,
    /// Principal-recovery tranche trigger, mult bps of entry.
    pub lc_tp1_bps: u32,
    /// Second tranche trigger, mult bps.
    pub lc_tp2_bps: u32,
    /// Second tranche size, bps of original position.
    pub lc_tp2_frac_bps: u32,
    /// Third tranche trigger, mult bps.
    pub lc_tp3_bps: u32,
    /// Third tranche size, bps of original position.
    pub lc_tp3_frac_bps: u32,
    /// Thesis-invalidation: exit when CVD falls to this fraction (bps) of its peak.
    pub lc_cvd_hold_frac_bps: u32,
    /// Runner stall window, ticks (no new high while in profit).
    pub lc_stall_ticks: u64,
    /// Conditional max hold, ticks (binds only when not advancing).
    pub lc_max_hold_ticks: u64,
    /// Rug-precursor single-swap drop trigger, bps.
    pub lc_precursor_drop_bps: u32,
    /// §32 thesis-exit FLOW PERSISTENCE: the number of CONSECUTIVE adverse
    /// order-flow observations required before the thesis force-exit fires.
    ///
    /// `1` reproduces the historical behaviour exactly — exit on the FIRST flow
    /// sign-flip. The research case for `> 1` (arXiv 2606.16269, the
    /// Lillo–Mike–Farmer sign-autocorrelation relation `γ = α − 1`) is that trade
    /// signs are long-memory because metaorder lengths are Pareto-distributed, so
    /// a SINGLE sign flip is close to the least informative read of the flow
    /// process — the information lives in a persistent same-signed RUN. Kaminski
    /// & Lo (*J. Financial Markets* 18:234–254) give the complementary result: a
    /// stop rule earns its keep only when the trigger predicts PERSISTENT adverse
    /// drift, and is otherwise a pure negative "stopping premium".
    ///
    /// Counted in ADVERSE OBSERVATIONS (event time), never wall-clock ticks — the
    /// persistence the cited literature measures is an event-time property, and
    /// wall-clock bucketing destroys it. Any non-adverse observation resets the
    /// run to zero, so this is a run-length gate, not a delay timer.
    pub thesis_persist_obs: u32,

    // ---- meta-rotation / creator-state (corroboration-tier) ----
    /// Taxonomy version the meta-rotation reducer stamps on its snapshots, and the
    /// version an incoming `TokenMetadata` must match to be merged into factual
    /// state (a mismatch is flagged UNKNOWN, never retroactively remapped — §81).
    pub meta_taxonomy_version: u32,
    /// Max distinct narrative categories tracked before overflow completeness (§99).
    pub meta_max_categories: usize,
    /// Max distinct creators retained per category (§99).
    pub meta_max_creators_per_cat: usize,
    /// Max distinct markets whose creator state is tracked before weakest-evict (§99).
    pub creator_track_cap: usize,
    /// Minimum launch share (bps) a category must hold before it is eligible for the
    /// saturation classification in `rotation_between` (§102, no silent magic).
    pub meta_min_share_bps: u64,
    /// Per-token attention-velocity threshold above which a token counts as
    /// *accelerating* for category meta-emergence breadth (§21.4).
    pub meta_accel_threshold: i64,
    /// Minimum count of accelerating tokens for a category to be attention-emergent
    /// (breadth, never a single token — §29.7c).
    pub meta_min_breadth: u32,
    /// Max multiplicative discovery-rank bonus (bps over a 10_000 base) an on-chain-
    /// emerging category grants its mints. Corroboration-tier: reorders promotion
    /// only; the gate still requires on-chain confirmation (§29/§71).
    pub meta_rank_bonus_bp: u32,
    /// Max multiplicative discovery-rank haircut (bps) a saturating category applies
    /// to its mints (fade-first). Bounded at 100% by `validate`.
    pub meta_saturation_haircut_bp: u32,
    /// Creator sold-fraction-of-peak (bps) above which a *graded* size haircut
    /// begins. Never a binary reject (§22 behavioral-risk clause). Bounded ≤100%.
    pub creator_fade_sold_bps: u64,

    // ---- reflection / adaptation ----
    /// Cadence, in ticks, at which the reflection pass runs (net-SOL → weights).
    pub reflect_every_ticks: u64,
    /// Maximum single-step lane-weight change, bps, the governance envelope allows.
    /// Bounds how fast reflection can move discovery emphasis (anti-overfit).
    pub reflect_weight_step_bp: u32,
    /// Floor a lane weight may never drop below (bps), so no lane is silently killed.
    pub reflect_weight_floor_bp: u32,
    /// Ceiling a lane weight may never exceed (bps).
    pub reflect_weight_ceiling_bp: u32,

    // ---- §21.6 bars + market structure (reduce-only consumption) ----
    /// Trades per bar on the per-mint trade-count clock (the volume-clock family:
    /// Easley/López de Prado/O'Hara 2012 — sample by activity, not seconds).
    pub bar_trades_per_bar: u64,
    /// Minimum closed bars before a swing-structure trend is defined at all
    /// (below this the factor is identity — structure never authorizes, §21.6).
    pub structure_min_bars: usize,
    /// Reduce-only size haircut (bps of 10_000) applied when swing structure
    /// CONTRADICTS the long entry (Downtrend). Confirmed/undefined structure is
    /// identity — no boost above the §33 envelope (§56.2).
    pub structure_downtrend_haircut_bp: u32,

    // ---- §21.5 active-market-universe promotion screen ----
    /// Token age (slots) below which a launch is exempt from the activity screen
    /// (a fresh creation legitimately has no history; the gate still demands
    /// numeric confirmation — earliness never bypasses corroboration).
    pub universe_age_exempt_slots: u32,
    /// Recent-activity window, logical ticks, for the promotion screen.
    pub universe_window_ticks: u64,
    /// Minimum trades within the window for a mature mint to stay promotable.
    pub universe_min_trades: u32,
    /// Minimum distinct buyer entities within the window (breadth, wash-resistant).
    pub universe_min_entities: u32,
    /// Maximum trades-per-entity ratio within the window (a crude wash guard:
    /// hyperactive single-entity tape is not organic activity, §28).
    pub universe_wash_ratio_max: u32,
    /// Minimum observed liquidity (lamports) for a mature mint to stay promotable.
    pub universe_min_liquidity_lamports: u64,

    // ---- §29.6 attention decay (narrative lane) ----
    /// Multiplicative per-step evidence decay, bps of 10_000 (< 10_000). At the
    /// research-grounded 9_330 the half-life is ~10 steps (memecoin attention
    /// decays in minutes, not hours).
    pub narrative_decay_bp: u32,
    /// Logical ticks per decay step (the step clock; TTL remains the hard cutoff).
    pub narrative_decay_step_ticks: u64,
    /// Absolute score floor below which decayed narrative evidence emits nothing.
    pub narrative_decay_floor: u64,

    // ---- §55 capacity curve (report-only) + §52 baseline destruction ----
    /// Landing-probability model base (bps) for the capacity-curve report.
    pub landing_base_bps: u32,
    /// Landing-probability penalty slope (bps) per unit size/depth for the report.
    pub landing_penalty_k_bps: u32,
    /// Required §52 margin (lamports) by which live must beat every baseline
    /// before the destruction verdict reads `defeats`.
    pub baseline_margin_lamports: i64,
    /// Minimum realized trades before a §52 destruction verdict is computed at
    /// all (small-n verdicts are noise, §46).
    pub baseline_min_trades: u32,

    // ---- §33 scale-in confirmation + §24 conditional expectancy ----
    /// Authenticity (bps) the flow screen must EVIDENCE (not merely default to)
    /// before the probe→full-target scale-in may add risk. The neutral prior a
    /// thin sample returns is NOT confirmation (§6.4): the engine additionally
    /// requires the screen's minimum swap sample before consulting this bar.
    pub scale_confirm_auth_min_bp: u32,
    /// §71 union-preservation quota: of `promote_k` promotion slots per tick,
    /// reserve up to this many for the highest-ranked CORROBORATION-tier
    /// candidates (non-numeric lanes) when raw rank would let the numeric lane
    /// monopolize the board. Discovery is a union, not an intersection — a lane
    /// that can never reach the gate is a lane that does not exist. Authority is
    /// unchanged: corroboration candidates still face the full gate (fade-first
    /// §29 — most reject without on-chain proof; promotion is cheap, entry
    /// is not).
    pub promote_corroboration_quota: usize,
    /// Minimum realized fills a lane must accumulate before its OWN realized
    /// per-trade return replaces the configured cold-start prior in conditional
    /// expectancy (§24 hierarchical partial pooling: below the gate the cell
    /// operates on the fixed-constant baseline; above it, cell estimates shrink
    /// toward the prior in proportion to sample size).
    pub expectancy_min_lane_trades: u32,

    // ---- §26 confirmed-creator-dump hard veto (operator-approved reversal) ----
    /// Master switch for the §26 confirmed-creator-dump law. When true, a market
    /// whose creator has distributed more than the configured fraction of peak is
    /// a HARD pre-entry veto (a NEW reject code) AND forces the exit of any held
    /// position attributed to that creator. Constitution reversal of the prior
    /// "creator distribution is fade-only, never a veto" behaviour (§22 clause is
    /// superseded by §26 for the *confirmed-dump* regime, operator-approved).
    pub creator_dump_veto_enable: bool,
    /// Sold-fraction-of-peak (bps of 10_000) at/above which a creator is a
    /// CONFIRMED dump — a hard veto, not merely a size fade. Strictly higher than
    /// `creator_fade_sold_bps` (the graded-fade trigger): fade below, veto above.
    pub creator_dump_veto_bp: u64,
    /// The stricter dump threshold (bps) applied when the §27 creator classifier
    /// labels the deployer `SerialRug` or `VolumeFarmer` — a known extractor earns
    /// a lower veto bar. Must be ≤ `creator_dump_veto_bp`.
    pub creator_dump_veto_strict_bp: u64,

    // ---- §24 cost-derived profit targets (Batch-2a LAW 2, operator-approved) ----
    /// Master switch for the §24 cost-derived take-profit ladder. When true the
    /// held position's tp1/tp2/tp3 multiples are DERIVED per-market from the
    /// gate's measured `round_trip_cost_bps` plus a margin multiple (via
    /// `pump_quant_strategy::exit_ladder::derive_target_bps`), and the tranche
    /// COUNT is the cost-priced rung count from `exit_ladder::ladder_rungs` —
    /// instead of the fixed 13_500/25_000/50_000 constants. Report-only until an
    /// operator flips it (§56.2 envelope). Off = byte-identical prior behaviour.
    pub derived_targets_enable: bool,
    /// The margin multiple (bps of 10_000) applied to the measured round-trip cost
    /// to size the profit margin ABOVE the cost floor: `margin = rt_cost ×
    /// mult/10_000`. The derived tp1 move is then `rt_cost + margin` (§24).
    pub target_margin_mult_bp: u32,
    /// Lower clamp of the derived tp1 multiple (mult bps of entry, 10_000 = entry).
    /// A tiny-cost market still aims for a real move — the §56.2 envelope floor.
    pub target_floor_bp: u32,
    /// Upper clamp of the derived tp1 multiple (mult bps of entry). A high-cost
    /// outlier can never demand an impossible target — the §56.2 envelope ceiling.
    pub target_ceiling_bp: u32,

    // ---- §24(d) exit-into-strength (Batch-2a LAW 5) ----
    /// Master switch for the §24(d) exit-into-strength law: while in profit, sell
    /// the remainder INTO an authentic buy-side burst CLIMAX (peaked, not yet
    /// exhausting) detected by `pump_quant_signals::microstructure::burst_phase`
    /// over the position's own swap-arrival stream. Off = prior behaviour.
    pub into_strength_exit_enable: bool,
    /// Climax-strength threshold: the burst arrival-rate elevation multiple (bps of
    /// 10_000) over baseline a recent window must clear before a plateau counts as
    /// a genuine climax — `20_000` = 2× baseline (§24(d), not routine flow).
    pub into_strength_climax_bp: u32,

    // ---- §24 volatility-scaled stops/trail (Batch-2a LAW 6) ----
    /// Master switch for the §24 vol-scaled stop/trail: the hard stop and trailing
    /// width widen with the market's `pump_quant_features::structure_ext::
    /// realized_vol_bps` over a fixed recent-bar window, always clamped INSIDE the
    /// position's existing `[trail_base_bps, trail_max_bps]` envelope (never
    /// outside floor/ceiling). Off = prior fixed stop/trail behaviour.
    pub vol_stop_enable: bool,
    /// Fraction (bps of 10_000) of the measured realized-vol bps added to the base
    /// stop/trail width: `extra = realized_vol × scale/10_000` (§24).
    pub vol_stop_scale_bp: u32,

    // ---- §25 setup-archetype classifier (Batch-2b LAW 4) ----
    /// Master switch for the §25 setup-archetype classifier. When true the engine
    /// replaces the hardcoded `archetype:0` stub at admit with the
    /// `pump_quant_signals::setup_classifier::classify_setup` output — the derived
    /// §24 named scalp family (BreakoutRetest / FailedBreakdownReversal / Reclaim /
    /// CompressionExpansion / ShortHorizonMeanReversion / OrderFlowDislocation) —
    /// reconstructed from the bar/flow state already folded per mint. The
    /// discriminator tags the entry thesis, the MFE/excursion samples, and the
    /// reject samples so analytics can group by real setup family instead of a
    /// single all-0 bucket. Default ON: this is a correctness wiring, `archetype:0`
    /// is a stub. It does not alter any capital decision (arbitration, gate, and
    /// exits do not read the archetype tag), only the analytics grouping.
    pub setup_classifier_enable: bool,

    // ---- §24 EntryMode leaves (Batch-2b LAW 11) ----
    /// Master switch for the §24 EntryMode detector leaves
    /// (`pump_quant_strategy::entry_mode_leaves`). When true, a candidate the
    /// 4-lane gate would reject for want of a fresh on-chain confirmation is
    /// admitted via the `detect_pullback_continuation` predicate — a controlled
    /// pullback that holds a retest inside an established uptrend maps onto
    /// active-market-scalp eligibility (the market is already live, its depth
    /// already observed) — while `detect_narrative_confirmation` stays a dormant,
    /// admission-gated predicate that never authorizes on its own. Off = prior
    /// 4-lane behaviour, byte-identical. §56.2 envelope.
    pub entry_mode_leaves_enable: bool,

    // ---- §70.1 composite money proxy (Batch-2c LAW 7) ----
    /// Master switch for the §70.1 composite money proxy M. When true the
    /// attention field's `money_of` level is the composite `M = distinct
    /// smart-wallet entry + holder-growth + net inflow` (folded BEFORE price
    /// momentum) instead of the on-chain buy-pressure alone: the smart-wallet /
    /// holder terms are ADDED to the existing OFI-derived buy-pressure, so a
    /// market whose genuine wallet-entry / holder-growth LEADS its buy-pressure
    /// registers rising money (and thus a Confirmed attention-money divergence)
    /// earlier than buy-pressure alone would show. Default ON: this is a
    /// correctness upgrade to the money proxy (§70.1), and it is a legitimate
    /// lamports-moving law — the golden net is re-pinned to its measured value.
    /// Off = the prior buy-pressure-only proxy, byte-identical.
    pub money_proxy_enable: bool,

    /// §70.1 holder term source: use the CONTINUOUS holder count folded from our
    /// own decoded swap flow ([`crate::holder_flow`]) instead of the
    /// `Features::unique_buyers` bitset popcount.
    ///
    /// The bitset it replaces is a 64-bit set indexed by `entity % 64`: it
    /// saturates at 64, it collides, and — decisively — it is **monotone
    /// non-decreasing**, because a bit once set is never cleared. It therefore
    /// cannot observe DISTRIBUTION at all. The folded holder count rises when
    /// holders broaden and FALLS when they exit, which is what makes an
    /// attention-vs-money divergence able to distinguish "the crowd is arriving"
    /// from "the crowd is being sold to".
    ///
    /// Read only when [`Self::money_proxy_enable`] is also true. The term is
    /// consulted at GROWTH tier ([`crate::holder_flow::HolderReading::growth_level`]),
    /// so `Exact` and `DeltaOnly` readings both qualify — §70.1 asks for holder
    /// *growth*, a derivative, which a delta-only basis supports — while an
    /// `Incomplete` (entity-cap-truncated) reading, an untracked mint, or a
    /// zero-evidence ledger falls back EXPLICITLY to the prior `unique_buyers`
    /// term rather than fabricating one.
    ///
    /// The term is clamped to
    /// [`crate::engine::MONEY_PROXY_HOLDER_TERM_CAP`] so its dynamic range is
    /// identical to the 0..64 bitset it replaces and
    /// `MONEY_PROXY_HOLDER_WEIGHT` stays calibrated against the 0..10_000
    /// buy-pressure momentum tail.
    ///
    /// **Default OFF.** This is the wave's one decision-affecting change and it
    /// did not clear its pre-registered A/B bar on the tapes tested — see
    /// `tests/holder_flow.rs::ab_*`. Off = the prior bitset term, byte-identical.
    pub money_proxy_holder_flow_enable: bool,

    // ---- §21.7/§70.1 holder-concentration (distribution-shape) law ----
    /// Master switch for the §21.7/§70.1 **holder distribution-shape** law
    /// ([`crate::holder_concentration`]): concentration, early-buyer capture,
    /// bundle/sniper presence and bump/wash flip behaviour, derived from the
    /// continuous holder ledger.
    ///
    /// When armed, three REDUCE-ONLY consumers wake up, and nothing else changes:
    ///
    /// 1. the §21.5 active-market-universe screen's `top_holder_concentration_bps`
    ///    field — dormant since inception because no source ever produced the
    ///    number — receives the real cumulative top-10 share, screened against the
    ///    named-const bar;
    /// 2. the sizing chain gains a concentration haircut multiplier
    ///    ([`crate::holder_concentration::CONCENTRATION_HAIRCUT_MULT_BPS`]) and,
    ///    CONJUNCTIVELY with an independent §21.7 fabrication corroboration, a
    ///    pre-entry refusal;
    /// 3. the flow-authenticity multiplier gains the bundle/flip evidence legs —
    ///    inside the existing single authenticity channel, never as a second
    ///    multiplier (§21.7).
    ///
    /// The law is **fail-open by construction**: a
    /// [`crate::holder_concentration::ConcentrationVerdict::Unknown`] (delta-only
    /// basis, truncated ledger, or too few tracked entities) yields the identity on
    /// every one of the three, so a market we cannot measure is treated exactly as
    /// it was before this law existed. Off = byte-identical to that same prior
    /// behaviour on every market.
    ///
    /// Default set by the pre-registered two-sided A/B in
    /// `tests/holder_concentration.rs::ab_*`.
    pub holder_concentration_enable: bool,

    // ---- §70.6/§70.8 narrative class + ceiling (Batch-2c LAW 8) ----
    /// Master switch for the §70.6/§70.8 narrative-class law. When true the
    /// attention emit path derives each mint's `NarrativeClass` (via
    /// `pump_quant_narrative::narrative::nv_class_classify` over the field's own
    /// spike/longevity/breadth/platform-led state), conditions the corroboration
    /// decay rate and the reach ceiling on that class (`nv_narrative_ceiling`),
    /// and feeds the class-conditioned ceiling into the §49 sizing conviction
    /// and the `nv_candidate_score`. Off = class-unconditioned scoring/sizing,
    /// byte-identical. Default OFF: a new scoring/sizing behaviour, report-only
    /// until an operator flips it (§56.2 envelope), matching the Batch-2a
    /// precedent.
    pub narrative_class_enable: bool,

    // ---- §70.7 platform-lead / crypto-social-lag (Batch-2c LAW 9) ----
    /// Master switch for the §70.7 platform-lead law (Signal-Horizon Matching,
    /// §46). When true the attention field tracks per-mint mainstream-vs-crypto
    /// first-mention instants (mainstream = TikTok/Web `SocialPlatform`s; crypto
    /// = X/Telegram) and feeds `nv_platform_lead`'s mainstream→crypto propagation
    /// front (`crypto_social_lag`) into the pre-legibility runway and the
    /// candidate score — a mint with a mainstream lead over crypto pickup earns a
    /// higher pre-legibility runway than one already crypto-saturated. Off =
    /// no platform-lead runway, byte-identical. Default OFF: situational (needs
    /// a mainstream-led mint) — report-only until an operator flips it.
    pub platform_lead_enable: bool,

    // ---- §70.9/§70.10 deployer credibility + fee-floor (Batch-2c LAW 10) ----
    /// Master switch for the §70.9 deployer-credibility screen. When true the
    /// pre-entry gate folds the wallet-graph `deployer_credibility`
    /// (prior-CA / serial-deploy occupancy, class-conditioned via the §27
    /// creator classifier) into a reduce-only size haircut at admit. Off = the
    /// prior credibility fold only, byte-identical. Default OFF: protective /
    /// situational — report-only until an operator flips it.
    pub deployer_screen_enable: bool,
    /// Master switch for the §70.10 anti-bundle first-slot fee-floor. When true
    /// the gate folds `pump_quant_signals::fee_plausibility::
    /// assess_first_slot_fee_floor` over the market's first-slot fee/tip record
    /// (threaded through the parallel creation-fee channel) as a reduce/veto: an
    /// implausibly-low cumulative fee footprint for the advertised activity fades
    /// size, and a fully-saturated (bundle/wash) signature vetoes pre-entry.
    /// Off = no fee-floor screen, byte-identical. Default OFF: protective —
    /// report-only until an operator flips it.
    pub fee_floor_enable: bool,
    /// Master switch for the §33/§43 sub-x_min probe-budget accounting. When true,
    /// a candidate sized BELOW the economic `x_min` cost floor is not opened as a
    /// normal position; instead its cost is routed through the
    /// `pump_quant_strategy::calibration_budget` ledger (per-route capped) and, if
    /// admitted, journalled as budgeted paid-information — a research probe that
    /// buys a measurement, never a profit claim. Once any calibration cap is
    /// exhausted the probe is refused. Off = the sub-x_min branch behaves exactly
    /// as before (small-bankroll promotion valve or hard refuse), byte-identical.
    /// Default OFF: a new spend channel, report-only until an operator flips it.
    pub probe_budget_enable: bool,

    // ---- §29 Discord paid-alpha lane (Wave-3 LAWs D1–D5) ----
    /// LAW D1 master switch: treat the `SocialPlatform::Discord` capture lane as a
    /// realtime designated-caller ALPHA channel. When true, a Discord-discovered
    /// mint routes to the independent `DiscoveryLane::AlphaCall` (index 5) instead
    /// of the open `SocialCaller` firehose — so reflection attributes a paid room's
    /// realized net SOL distinctly (§71 reflection integrity) — and the room is
    /// bound as the mint's alpha source for the §29.8 per-source outcome ledger
    /// (LAW D5). Off = Discord calls behave exactly like any other social caller,
    /// byte-identical. Default ON: a correctness/attribution wiring — it changes NO
    /// capital decision (the discovery lane never participates in ranking or the
    /// gate; alpha alone still cannot admit, LAW D4), only WHICH lane earns the
    /// attribution, so the golden net/counts are unchanged by it on any tape.
    pub alpha_call_lane_enable: bool,
    /// LAW D2 master switch: a mention from an `is_designated_caller` author — a
    /// paid Discord alpha room OR a curated followed key account (X) — carries an
    /// elevated attention weight. BREADTH-GATED like the live-broadcaster law
    /// (§29.6), never a blank multiplier: each DISTINCT designated caller adds
    /// `designated_caller_weight`, and echo/coordinated repeats add zero. Off =
    /// designated callers weigh like any other mention, byte-identical. Default ON:
    /// the "known paid-alpha caller / followed key account is high signal" law.
    pub designated_caller_enable: bool,
    /// LAW D2 weight: attention units each DISTINCT designated caller adds to the
    /// weighted level while the call is fresh. Defaulted to half the attention
    /// formation floor ([`DESIGNATED_CALLER_WEIGHT_DEFAULT`]) so a LONE caller is
    /// half-formation (below the emergence floor on its own) and only genuine
    /// distinct corroboration — a second independent designated caller, or organic
    /// breadth — completes formation (§29 fade-first, §102 rationale).
    pub designated_caller_weight: u64,
    /// LAW D3 master switch: a designated-caller call the sentiment brain marks
    /// BEARISH (a sell / exit / dump signal) on a HELD position raises reduce-only
    /// exit pressure (§29.5) — it halves the position's stall window and trail cap
    /// (the existing meta-saturation exit-pressure machinery), accelerating the
    /// exit. It NEVER adds, authorizes, or sizes up — alpha is actionable for EXITS
    /// too, within the reduce-only law. Off = a bearish alpha call informs nothing
    /// on a held position, byte-identical. Default ON: a protective correctness law.
    pub alpha_exit_pressure_enable: bool,

    // ---- brain: episodic recall memory (LAWs B1–B5) ----
    /// LAW B1/B2 master switch for the episodic memory plane. ON records an
    /// [`crate::brain::BrainEntry`] fingerprint at every admit, seals an `Episode`
    /// at every completed trade, and produces the reflection-cadence recall
    /// readout. It touches NO capital decision on its own — the digest moves only
    /// through the §19 config-identity seed, never through a different size, gate
    /// or exit. Off ⇒ the plane is never queried and never written.
    pub brain_enable: bool,
    /// §46 minimum matched episodes before recall may produce an estimate at all.
    /// Below it the verdict is structurally `Unknown` and LAW B4 pins that an
    /// `Unknown` changes nothing.
    pub brain_min_sample: u32,
    /// §102 stage-1 Hamming radius defining "a setup like this".
    pub brain_recall_max_distance: u32,
    /// LAW B3 master switch for the **reduce-only** recall haircut. ON lets a
    /// `Known` recall verdict over a historically-bleeding setup class shrink — or,
    /// past the veto bar, refuse — an entry the rest of the chain would have taken.
    /// It can NEVER enlarge size (§29.5): the verdict type has no boost variant.
    /// Off ⇒ recall is still computed and reported, but never acted on, which is
    /// what makes the A/B a clean isolation of the law rather than of the work.
    pub brain_haircut_enable: bool,
    /// LAW B3 haircut bar: a class whose decisive win rate is at or below this
    /// (with a negative median net) is faded. Bps.
    pub brain_haircut_win_rate_bp: u32,
    /// LAW B3 veto bar: at or below this decisive win rate the class is refused
    /// outright. Must not exceed [`Config::brain_haircut_win_rate_bp`] (validated).
    pub brain_veto_win_rate_bp: u32,
    /// LAW B3 reduce-only size multiplier applied to a faded class, bps of 10_000.
    /// Validated ≤ 10_000 — a "haircut" that grew size would be a contradiction.
    pub brain_haircut_mult_bp: u32,
    /// LAW B5: arm durable persistence of the episodic journal at
    /// [`Config::brain_path`]. Off (or an empty path) ⇒ the index is memory-only.
    pub brain_persist_enable: bool,
    /// LAW B5 base path for the episodic snapshot + append-only journal; the
    /// engine appends `.snapshot` / `.journal`. Empty ⇒ persistence disarmed.
    pub brain_path: CfgPath,

    // ---- brain: strategy-analysis export + brain-informed reflection ----
    /// LAW B6 master switch for the `brain_analysis_v1` strategy-analysis export
    /// ([`crate::brain_analysis`]). ON writes the bounded JSON artifact alongside
    /// `live_status.json` at the same info-time cadence. REPORT PLANE ONLY: the
    /// artifact is produced from already-realized state and is never read back by
    /// any decision, so the switch cannot move a fill.
    pub brain_analysis_enable: bool,
    /// LAW B6 path for the strategy-analysis export. Empty ⇒ no file is written
    /// (`Engine::brain_analysis_json` still works — the artifact is a pure
    /// function of engine state, and the filesystem is only one of its sinks).
    pub brain_analysis_path: CfgPath,
    /// LAW B7 master switch for the **reduce-only** brain-informed lane
    /// reweighting in [`crate::reflect`]. ON lets conditioned recall over a lane's
    /// *setups* apply an ADDITIONAL downweight inside the §56.2 envelope when
    /// those setups have decayed on our own realized evidence. There is no
    /// up-weight path (§29.5/§46 — recall may shrink conviction, never inflate
    /// it).
    ///
    /// **DEFAULT OFF, and the decision is pre-registered and two-sided.**
    /// `tests/brain_reflect_twosided.rs` states the acceptance rule BEFORE any
    /// measurement and then runs it: a happy path (genuine lane decay under real
    /// promotion-slot contention, with the decayed and healthy markets
    /// INDISTINGUISHABLE at admit), an unhappy path (the same tape with the two
    /// forward cohorts' shapes swapped, so the flag is a false positive), and the
    /// golden tape as the neutral control. Measured (re-pin #26, 2026-07-28): the
    /// happy-path gain is +88_208_992 lamports on a 555_444_680 base — still below
    /// the pre-registered materiality bar of one 0.1-SOL bite, but by only 12% —
    /// against a false-positive cost of −15_249_896, a 5.78× asymmetry that CLEARS
    /// the pre-registered 3× bar.
    ///
    /// **THIS DEFAULT IS UNDER REVIEW.** The retired figures (+26_697_249 /
    /// −21_009_674, 1.27×, with the sign inverting on neighbouring shapes) were taken
    /// on a tape declaring 0.2 SOL pools against a 0.1 SOL clip; under the derived
    /// impact model that tape admitted NOTHING and the verdict was arithmetic on
    /// zeros. At real depth the asymmetry leg passes, the whole rule passes at every
    /// step size above the shipped 250 bp, and the sign inversion is gone. The law
    /// stays OFF pending an A-11 study — arming on the strength of a corrected fixture
    /// is not a decision a test author may take. The structural argument below still
    /// explains why the effect is bounded, and is unaffected:
    /// §24 conditional expectancy already conditions §23 slot arbitration on each
    /// lane's realized mean return and activates at `expectancy_min_lane_trades` = 8
    /// — fewer trades than `brain_decay_min_sample` = 12 requires before this law
    /// may speak at all. See also `tests/brain_strategy.rs` for the mechanism-level
    /// unit A/B.
    pub brain_reflect_enable: bool,
    /// LAW B7 extra downweight step applied to a lane whose conditioned setups
    /// have decayed, bps. Bounded by the same §56.2 floor as the base step, so an
    /// armed reflection can still never drive a lane to zero.
    pub brain_reflect_step_bp: u32,
    /// LAW B7 §46 sample floor for a lane-decay flag: the SUM of matched episodes
    /// across a lane's conditioned setup classes must reach this before any
    /// downweight (or any [`crate::brain_analysis`] retirement flag) may bind.
    /// Fail-closed — below it there is no flag at all, not a weak one.
    pub brain_decay_min_sample: u32,
}

/// LAW D2 default designated-caller attention weight: half the standard attention
/// formation floor (`formation_level = 100`), so one caller is half-formation and
/// genuine distinct corroboration completes it. Mirrors the live-broadcaster
/// half-formation choice (§29.6/§102) — a documented scale, not fake precision.
pub const DESIGNATED_CALLER_WEIGHT_DEFAULT: u64 = 50;

/// §21.4 / criterion 81 default meta-taxonomy version: `1`, matching
/// [`pump_quant_market_state::meta::TAXONOMY_V1`].
///
/// v0 matched every lexical needle as a naive substring and therefore assigned
/// "Fair Launch"→AI (`ai` in `fair`), "Catalyst"→Animal (`cat`), "Bottom
/// Signal"→AI (`bot`), "Bullish Chain"→Animal (`bull`), "Starter Pack"→Celebrity
/// (`star`) and "Magazine"→Political (`maga`). `category_id` is a **brain recall
/// filter key**, so each of those pools a token with the wrong meta's episodes
/// and silently corrupts every conditioned recall estimate keyed on it. v1 adopts
/// the word-boundary discipline. The fix is FORWARD only: v0 stays frozen as the
/// historical record, and an incoming assignment stamped `0` no longer matches
/// this version so it is left UNKNOWN rather than retroactively remapped (§81).
pub const META_TAXONOMY_VERSION_DEFAULT: u32 = pump_quant_market_state::meta::TAXONOMY_VERSION_V1;

/// §24 LAW 2 default: profit margin over the measured cost floor is 1.5× the
/// round-trip cost. Named const (§102).
pub const TARGET_MARGIN_MULT_BP_DEFAULT: u32 = 15_000;
/// §24/§56.2 LAW 2 default: derived tp1 never below +10% of entry (envelope floor).
pub const TARGET_FLOOR_BP_DEFAULT: u32 = 11_000;
/// §24/§56.2 LAW 2 default: derived tp1 never above 6× entry (envelope ceiling).
pub const TARGET_CEILING_BP_DEFAULT: u32 = 60_000;
/// §24(d) LAW 5 default: a genuine climax needs the recent arrival rate at ≥2×
/// baseline. Named const (§102).
pub const INTO_STRENGTH_CLIMAX_BP_DEFAULT: u32 = 20_000;
/// §24 LAW 6 default: add 0.5× of the realized-vol bps to the base stop/trail,
/// inside the envelope. Named const (§102).
pub const VOL_STOP_SCALE_BP_DEFAULT: u32 = 5_000;

/// §26 default: 60% of peak distributed is a confirmed dump (well above the
/// graded-fade trigger). Named const, §102.
pub const CREATOR_DUMP_VETO_BP_DEFAULT: u64 = 6_000;
/// §26/§27 default: a classified serial-rug / volume-farmer deployer vetoes at
/// 35% of peak distributed — a known extractor gets a lower bar. §102.
pub const CREATOR_DUMP_VETO_STRICT_BP_DEFAULT: u64 = 3_500;

/// Operator-directed absolute minimum trade size: 0.1 SOL in lamports (criterion
/// 112 operator floor / Amendment A-6). Every individual order — initial entry,
/// each probe, each scale-in add — is ≥ this; the engine NEVER emits a sub-0.1-SOL
/// bet. Named const, §102. A `0` value disables the floor (legacy sub-`x_min`
/// probe path re-enabled). This is a hard policy minimum layered ON TOP of the
/// per-market economic `x_min`; it is a *starting default*, operator-overridable.
pub const MIN_TRADE_SIZE_LAMPORTS_DEFAULT: u64 = 100_000_000;

impl Config {
    /// A portable starting point for laptop dry-runs and tests.
    ///
    /// NOTE: these are *defaults an operator overrides*, not decision constants.
    /// The engine never hard-codes any of them; it reads the fields. Values are
    /// deliberately generic (round bps, no venue-specific magic) so that no test
    /// passes by coincidence of a tuned number.
    #[must_use]
    pub fn dev_portable() -> Self {
        Self {
            watchlist_capacity: 64,
            watchlist_ttl_ticks: 100,
            promote_k: 8,
            promote_min_rank: 1,

            numeric_ofi_min_bp: 1_000, // ≥10% net-buy imbalance to discover on flow
            wallet_score_scale: 100,
            narrative_stage_hi_fp: 2 * pump_quant_narrative::narrative::FP_ONE,
            narrative_stage_lo_fp: pump_quant_narrative::narrative::FP_ONE,
            confirmed_capacity_mult: 4,

            // COST-MODEL UNIFICATION (2026-07-28). The old 300 bps prior was viable
            // only against the old 100 bps modelled protocol cost. The venue's REAL
            // schedule is 125 bps a leg — 250 bps round trip — so 300 bps of expected
            // move minus 250 of fee minus the 50 bps margin leaves ZERO budget for
            // impact, and `size_band` refuses every candidate at every size. The floor
            // of coherence is ~397 bps (250 fee + 50 margin + 64 fail-inflated fixed +
            // 33 impact, at the 0.1 SOL operator clip against a 30 SOL launch curve);
            // 1_800 is the venue-realistic figure every tape fixture in this repo
            // already overrides to, and it is now the default rather than a constant
            // fifteen fixtures had to correct by hand.
            gate_expected_move_bps: 1_800,
            gate_base_fixed_lamports: 50_000,
            gate_fail_rate_bps: 500,
            gate_protocol_bps: 100,
            gate_margin_bps: 50,
            gate_impact_den: 1_000_000,
            gate_exit_tranches: 3,

            fill_mode: FillModeCfg::OptimisticCeiling,
            entry_fee_bps: 100,
            exit_fee_bps: 100,
            entry_tip_lamports: 10_000,
            exit_tip_lamports: 10_000,
            sim_impact_k_bps: 50,

            // Criterion 103 backtest-fidelity leaves: BOTH default-inert. The exact
            // curve-fill math and the landing-lag knob exist and are tested, but
            // arming either changes fill prices and therefore net SOL, so each is a
            // separate gated change with its own A/B (§56: no decision-plane law is
            // armed before it has paid for itself). Off/zero reproduces today's
            // behaviour byte-for-byte.
            curve_exact_fill_enable: false,
            mcap_band_enable: false,
            mcap_band_lo_lamports: 118_420_000_000,
            mcap_band_hi_lamports: 263_160_000_000,
            expected_move_model_enable: false,
            expected_move_min_sample: 30,
            expected_move_prior_weight: 30,
            fill_landing_slots: 0,

            bankroll_initial_lamports: 2_000_000_000, // 2 SOL start; ANY amount works
            // A-6 small-bankroll recalibration (criterion 112): on a 2 SOL start the
            // survival floor is max(0.5 SOL, 25%×2) = 0.5 SOL ⇒ deployable 1.5 SOL,
            // and a full-confidence base bite (f_base) is ≈0.1 SOL — the operator
            // floor is the NATURAL base bite, and deep-fractional Kelly modulates
            // ABOVE it (differentiating naturally as the bankroll compounds past ~3
            // SOL). See MIN_TRADE_SIZE_LAMPORTS_DEFAULT and the A/B in the golden tape.
            min_trade_size_lamports: MIN_TRADE_SIZE_LAMPORTS_DEFAULT, // 0.1 SOL hard floor
            floor_fraction_bps: 2_500, // floor = max(0.5 SOL, 25% of start)
            f_base_bp: 667,            // base bite ≈0.1 SOL on 1.5 deployable
            total_risk_cap_bp: 2_100,  // fits 3× floor notional+fees (~0.303 SOL)
            max_concurrent_positions: 3, // 3 × 667bp ≈ 2000bp; cap adds fee headroom
            x_min_promote_cap_bp: 800, // 0.1 SOL = 6.67% deployable ⇒ cap must exceed
            promote_min_haircut_bp: 8_000, // never promote a risk-faded trade
            dd_tier1_bp: 1_500,        // −15% dd → half fraction
            dd_tier2_bp: 3_000,        // −30% dd → quarter fraction
            dd_tier3_bp: 5_000,        // −50% dd → probe-only survival
            probe_f_bp: 50,
            probe_frac_bp: 4_000, // open 40% as the probe; scale to full on confirmation
            arb_min_expected_net_lamports: 0,

            vpin_v_min_lamports: 250_000_000, // ≈ one retail clip (0.25 SOL)
            vpin_v_max_lamports: 20_000_000_000, // ≤ ~25% of a full curve per bucket
            vpin_min_buckets: 8,
            vpin_stale_ticks: 150,
            vpin_warn_bp: 6_500,
            vpin_toxic_bp: 8_000,
            vpin_veto_bp: 9_000,
            vpin_sell_dom_bp: 6_000,

            roll_trend_bp: 1_500,
            roll_revert_bp: -1_500,
            revert_ofi_min_bp: 2_500, // 2.5× the baseline OFI bar under REVERT
            revert_size_mult_bp: 5_000, // half size under REVERT

            lane_evidence_ttl_ticks: 100, // matches the watchlist TTL
            confirm_ttl_ticks: 200,
            creation_score: 1_000,
            creation_ttl_ticks: 50,

            lc_hard_sl_bps: 3_500,
            lc_trail_base_bps: 2_200,
            lc_trail_k_div: 4,
            lc_trail_max_bps: 12_000,
            lc_tp1_bps: 13_500,
            lc_tp2_bps: 25_000,
            lc_tp2_frac_bps: 3_000,
            lc_tp3_bps: 50_000,
            lc_tp3_frac_bps: 3_000,
            lc_cvd_hold_frac_bps: 4_500,
            lc_stall_ticks: 25,
            lc_max_hold_ticks: 300,
            lc_precursor_drop_bps: 3_000,
            // 1 == exit on the first adverse flow observation (historical behaviour).
            // Any change here must be earned on the two-sided flow-noise tape.
            thesis_persist_obs: 1,

            // Matches `meta::TAXONOMY_V1` — the word-boundary-disciplined lexicon.
            // v0's naive substring matching mis-assigned ordinary English into the
            // brain's recall filter key; the fix ships FORWARD under a bumped version
            // and v0 stays frozen as the historical record (criterion 81).
            meta_taxonomy_version: crate::config::META_TAXONOMY_VERSION_DEFAULT,
            meta_max_categories: 64,
            meta_max_creators_per_cat: 256,
            creator_track_cap: 4_096,          // matches the lane track cap
            meta_min_share_bps: 1_000,         // ≥10% launch share to count as "meaningful"
            meta_accel_threshold: 0, // any strictly-positive attention velocity accelerates
            meta_min_breadth: 3,     // ≥3 accelerating tokens = category breadth
            meta_rank_bonus_bp: 2_000, // up to +20% rank for an emerging category
            meta_saturation_haircut_bp: 2_000, // up to −20% rank for a saturating one
            creator_fade_sold_bps: 5_000, // fade size once creator has sold >50% of peak

            reflect_every_ticks: 50,
            reflect_weight_step_bp: 250,
            reflect_weight_floor_bp: 2_000,
            reflect_weight_ceiling_bp: 40_000,

            bar_trades_per_bar: 8, // trade-count clock (volume-clock family)
            structure_min_bars: 3, // left+right+1 at neighborhood 1
            structure_downtrend_haircut_bp: 7_000, // −30% size against structure

            universe_age_exempt_slots: 64, // fresh launches are exempt (§21.5/§23)
            universe_window_ticks: 24,     // recent-activity window
            universe_min_trades: 3,
            universe_min_entities: 2,
            universe_wash_ratio_max: 6, // > 6 trades/entity ⇒ wash-suspect tape
            universe_min_liquidity_lamports: 10_000_000, // 0.01 SOL floor

            narrative_decay_bp: 9_330, // half-life ≈ 10 steps
            narrative_decay_step_ticks: 5,
            narrative_decay_floor: 4,

            landing_base_bps: 9_500, // §55 report landing model
            landing_penalty_k_bps: 2_000,
            baseline_margin_lamports: 100_000, // §52: beat baselines by ≥0.0001 SOL
            baseline_min_trades: 32,           // no small-n verdicts (§46)

            scale_confirm_auth_min_bp: 8_000, // evidence-backed authenticity bar
            promote_corroboration_quota: 2,   // §71: 2 of 8 slots for non-numeric evidence
            expectancy_min_lane_trades: 8,    // §24 minimum-effective-sample gate

            // §26 confirmed-creator-dump hard veto (operator-approved reversal).
            creator_dump_veto_enable: true,
            creator_dump_veto_bp: CREATOR_DUMP_VETO_BP_DEFAULT,
            creator_dump_veto_strict_bp: CREATOR_DUMP_VETO_STRICT_BP_DEFAULT,

            // Batch-2a exit/sizing mechanics. §24 cost-derived profit targets
            // (LAW 2) is DEFAULT ON per the operator's "constitution wins" ruling
            // on the §24 reversal (defect #3): fixed global TP constants
            // (13_500/25_000/50_000) are FORBIDDEN as the live default — cost-
            // derived targets MUST be the live behaviour. LAWs 5/6 (§24(d)
            // exit-into-strength, vol-scaled stops) stay DEFAULT OFF (situational/
            // protective — report-only until an operator flips them through the
            // §56.2 envelope, per golden-arc discipline). Each law's causal value
            // is proven on its own hazard tape in audit_wave2_laws.rs.
            derived_targets_enable: true,
            target_margin_mult_bp: TARGET_MARGIN_MULT_BP_DEFAULT,
            target_floor_bp: TARGET_FLOOR_BP_DEFAULT,
            target_ceiling_bp: TARGET_CEILING_BP_DEFAULT,
            into_strength_exit_enable: false,
            into_strength_climax_bp: INTO_STRENGTH_CLIMAX_BP_DEFAULT,
            vol_stop_enable: false,
            vol_stop_scale_bp: VOL_STOP_SCALE_BP_DEFAULT,

            // Batch-2b: §25 archetype classifier ON (correctness wiring — the
            // stub archetype:0 is replaced by the real derived family; no capital
            // decision changes). §24 EntryMode leaves OFF (a new admission path —
            // report-only until an operator flips it, exactly like the shadow
            // tournament adoptions). Each law's causal value is proven on its own
            // hazard tape in audit_wave2_laws.rs.
            setup_classifier_enable: true,
            entry_mode_leaves_enable: false,

            // Batch-2c: §70.1 composite money proxy ON (correctness upgrade to
            // the money proxy — a legitimate lamports-moving law; the golden net
            // is re-pinned to its measured value). §70.6/§70.8 narrative class,
            // §70.7 platform-lead, and §70.9/§70.10 deployer/fee-floor screens
            // DEFAULT OFF (new scoring/sizing or protective behaviours —
            // report-only until an operator flips them, matching the Batch-2a
            // precedent). Each law's causal value is proven on its own hazard
            // tape in audit_wave2_laws.rs.
            money_proxy_enable: true,
            money_proxy_holder_flow_enable: false,
            holder_concentration_enable: false,
            narrative_class_enable: false,
            platform_lead_enable: false,
            deployer_screen_enable: false,
            fee_floor_enable: false,
            probe_budget_enable: false,

            // Wave-3 §29 Discord paid-alpha lane (LAWs D1–D5). All three switchable
            // laws are DEFAULT ON — they are correctness/attribution (D1: route a
            // paid room to its own AlphaCall lane; the discovery lane never ranks,
            // so no capital decision changes) and protective/high-signal laws (D2:
            // a known paid-alpha caller is high signal, breadth-gated; D3: a bearish
            // alpha sell call accelerates a HELD exit, reduce-only). D4 (alpha alone
            // can never admit) is a pinned invariant with no toggle; D5 (per-room
            // net-SOL ledger) rides on D1's binding. Each law's causal effect is
            // proven on its own hazard tape in alpha_laws.rs.
            alpha_call_lane_enable: true,
            designated_caller_enable: true,
            designated_caller_weight: DESIGNATED_CALLER_WEIGHT_DEFAULT,
            alpha_exit_pressure_enable: true,

            // Brain (LAWs B1–B5). B1/B2/B5 are DEFAULT ON: they are pure
            // record/readout laws that touch no capital decision, and the operator
            // asked for a memory that is actually populated rather than an
            // opt-in that is never switched on.
            //
            // B3 — the only law here that can move lamports — is now DEFAULT ON as
            // of re-pin #21, and the decision is the OUTPUT of a pre-registered
            // rule rather than an opinion. `tests/law_permutation_sweep.rs` measures
            // ALL EIGHT combinations of {B3, B7, §21.7 concentration} on ten tapes
            // and B3-alone clears every leg (re-measured at re-pin #26 on hazard
            // tapes with real pump.fun depth; the retired figures are in parentheses):
            //   * union tape (all three hazards on one engine): +650_761_435
            //     (was +296_536_625) against a 100_000_000 materiality bar;
            //   * WORST delta across all nine hazard tapes: exactly 0 — B3 does not
            //     lose a lamport on any tape measured, including both sides of
            //     every other law's two-sided pair;
            //   * its own two-sided pair: +414_992_045 (was +391_932_566) on the
            //     hazard tape against a NEGATIVE loss (+392_297_119) on the maximal
            //     false-positive mirror, so the 3× asymmetry bar passes without
            //     needing the ratio;
            //   * the golden tape is EXACTLY neutral — net / admitted / rejected /
            //     promoted / universe_filtered all unchanged (12 admits generate too
            //     few episodes to clear the §46 sample floor, so every admit-time
            //     recall there is `Unknown` and LAW B4 makes that a structural no-op).
            // NOTE (re-pin #26): B3-alone is no longer the UNIQUE winner. {B3, B7}
            // also clears the rule now that LAW B7's tape is not vacuous. B7 is NOT
            // armed here — its marginal union contribution is 33_426_226, a third of
            // the materiality bite, and the decision belongs to an operator via A-11.
            // See `tests/law_permutation_sweep.rs` and `tests/brain_reflect_twosided.rs`.
            // The law is reduce-only (§29.5 — the verdict type has no boost variant)
            // and fail-closed (LAW B4), so arming it is bounded on the downside by
            // construction as well as by measurement.
            brain_enable: true,
            brain_min_sample: crate::brain::BRAIN_MIN_SAMPLE_DEFAULT,
            brain_recall_max_distance: crate::brain::BRAIN_RECALL_MAX_DISTANCE_DEFAULT,
            brain_haircut_enable: true,
            brain_haircut_win_rate_bp: crate::brain::BRAIN_HAIRCUT_WIN_RATE_BP_DEFAULT,
            brain_veto_win_rate_bp: crate::brain::BRAIN_VETO_WIN_RATE_BP_DEFAULT,
            brain_haircut_mult_bp: crate::brain::BRAIN_HAIRCUT_MULT_BP_DEFAULT,
            brain_persist_enable: false,
            brain_path: CfgPath::empty(),
            // LAW B6: the export is report-plane and inert, so it is ON by
            // default; the PATH is empty, so nothing is written unless an
            // operator names a sink. `Engine::brain_analysis_json()` is always
            // available regardless (tests and the evaluator take that seam).
            brain_analysis_enable: true,
            brain_analysis_path: CfgPath::empty(),
            // LAW B7: DEFAULT OFF. The A/B on the golden tape and on the
            // purpose-built decayed-lane tape did not earn (see
            // `tests/brain_strategy.rs`), and §56 forbids arming a decision-plane
            // law that has not paid for itself.
            brain_reflect_enable: false,
            brain_reflect_step_bp: crate::reflect::BRAIN_REFLECT_STEP_BP_DEFAULT,
            brain_decay_min_sample: crate::brain_analysis::BRAIN_DECAY_MIN_SAMPLE_DEFAULT,
        }
    }

    /// Apply a single `key = value` override. Returns `Err` on an unknown key or a
    /// value outside the field's domain, so a malformed config fails loud (§18).
    pub fn apply(&mut self, key: &str, value: i64) -> Result<(), ConfigError> {
        let nonneg = |v: i64| -> Result<u64, ConfigError> {
            u64::try_from(v).map_err(|_| ConfigError::OutOfRange(key.to_string(), value))
        };
        let bp = |v: i64| -> Result<u32, ConfigError> {
            u32::try_from(v).map_err(|_| ConfigError::OutOfRange(key.to_string(), value))
        };
        let sz = |v: i64| -> Result<usize, ConfigError> {
            usize::try_from(v).map_err(|_| ConfigError::OutOfRange(key.to_string(), value))
        };
        match key {
            "watchlist_capacity" => self.watchlist_capacity = sz(value)?,
            "watchlist_ttl_ticks" => self.watchlist_ttl_ticks = nonneg(value)?,
            "promote_k" => self.promote_k = sz(value)?,
            "promote_min_rank" => self.promote_min_rank = nonneg(value)?,
            "numeric_ofi_min_bp" => self.numeric_ofi_min_bp = bp(value)?,
            "wallet_score_scale" => self.wallet_score_scale = nonneg(value)?,
            "narrative_stage_hi_fp" => self.narrative_stage_hi_fp = nonneg(value)?,
            "narrative_stage_lo_fp" => self.narrative_stage_lo_fp = nonneg(value)?,
            "confirmed_capacity_mult" => self.confirmed_capacity_mult = sz(value)?.max(1),
            "gate_expected_move_bps" => self.gate_expected_move_bps = bp(value)?,
            "gate_base_fixed_lamports" => self.gate_base_fixed_lamports = nonneg(value)?,
            "gate_fail_rate_bps" => self.gate_fail_rate_bps = bp(value)?,
            "gate_protocol_bps" => self.gate_protocol_bps = bp(value)?,
            "gate_margin_bps" => self.gate_margin_bps = bp(value)?,
            "gate_impact_den" => self.gate_impact_den = nonneg(value)?.max(1),
            "gate_exit_tranches" => {
                self.gate_exit_tranches = u32::try_from(nonneg(value)?.max(1))
                    .map_err(|_| ConfigError::OutOfRange(key.to_string(), value))?
            }
            "fill_mode" => {
                self.fill_mode = FillModeCfg::from_code(value)
                    .ok_or(ConfigError::OutOfRange(key.to_string(), value))?
            }
            "entry_fee_bps" => self.entry_fee_bps = bp(value)?,
            "exit_fee_bps" => self.exit_fee_bps = bp(value)?,
            "entry_tip_lamports" => self.entry_tip_lamports = nonneg(value)?,
            "exit_tip_lamports" => self.exit_tip_lamports = nonneg(value)?,
            "sim_impact_k_bps" => self.sim_impact_k_bps = bp(value)?,
            "curve_exact_fill_enable" => self.curve_exact_fill_enable = value != 0,
            "mcap_band_enable" => self.mcap_band_enable = value != 0,
            "mcap_band_lo_lamports" => self.mcap_band_lo_lamports = nonneg(value)?,
            "mcap_band_hi_lamports" => self.mcap_band_hi_lamports = nonneg(value)?,
            "expected_move_model_enable" => self.expected_move_model_enable = value != 0,
            "expected_move_min_sample" => self.expected_move_min_sample = bp(value)?,
            "expected_move_prior_weight" => self.expected_move_prior_weight = bp(value)?,
            "fill_landing_slots" => self.fill_landing_slots = nonneg(value)?,
            "bankroll_initial_lamports" => self.bankroll_initial_lamports = nonneg(value)?,
            "min_trade_size_lamports" => self.min_trade_size_lamports = nonneg(value)?,
            "floor_fraction_bps" => self.floor_fraction_bps = bp(value)?,
            "f_base_bp" => self.f_base_bp = bp(value)?,
            "total_risk_cap_bp" => self.total_risk_cap_bp = bp(value)?,
            "max_concurrent_positions" => self.max_concurrent_positions = sz(value)?.max(1),
            "x_min_promote_cap_bp" => self.x_min_promote_cap_bp = bp(value)?,
            "promote_min_haircut_bp" => self.promote_min_haircut_bp = bp(value)?,
            "dd_tier1_bp" => self.dd_tier1_bp = bp(value)?,
            "dd_tier2_bp" => self.dd_tier2_bp = bp(value)?,
            "dd_tier3_bp" => self.dd_tier3_bp = bp(value)?,
            "probe_f_bp" => self.probe_f_bp = bp(value)?,
            "probe_frac_bp" => self.probe_frac_bp = bp(value)?.max(1),
            "arb_min_expected_net_lamports" => self.arb_min_expected_net_lamports = value,
            "vpin_v_min_lamports" => self.vpin_v_min_lamports = nonneg(value)?.max(1),
            "vpin_v_max_lamports" => self.vpin_v_max_lamports = nonneg(value)?.max(1),
            "vpin_min_buckets" => self.vpin_min_buckets = sz(value)?.max(1),
            "vpin_stale_ticks" => self.vpin_stale_ticks = nonneg(value)?,
            "vpin_warn_bp" => self.vpin_warn_bp = bp(value)?,
            "vpin_toxic_bp" => self.vpin_toxic_bp = bp(value)?,
            "vpin_veto_bp" => self.vpin_veto_bp = bp(value)?,
            "vpin_sell_dom_bp" => self.vpin_sell_dom_bp = bp(value)?,
            "roll_trend_bp" => self.roll_trend_bp = value,
            "roll_revert_bp" => self.roll_revert_bp = value,
            "revert_ofi_min_bp" => self.revert_ofi_min_bp = bp(value)?,
            "revert_size_mult_bp" => self.revert_size_mult_bp = bp(value)?,
            "lane_evidence_ttl_ticks" => self.lane_evidence_ttl_ticks = nonneg(value)?.max(1),
            "confirm_ttl_ticks" => self.confirm_ttl_ticks = nonneg(value)?.max(1),
            "creation_score" => self.creation_score = nonneg(value)?,
            "creation_ttl_ticks" => self.creation_ttl_ticks = nonneg(value)?,
            "lc_hard_sl_bps" => self.lc_hard_sl_bps = bp(value)?,
            "lc_trail_base_bps" => self.lc_trail_base_bps = bp(value)?,
            "lc_trail_k_div" => self.lc_trail_k_div = bp(value)?.max(1),
            "lc_trail_max_bps" => self.lc_trail_max_bps = bp(value)?,
            "lc_tp1_bps" => self.lc_tp1_bps = bp(value)?,
            "lc_tp2_bps" => self.lc_tp2_bps = bp(value)?,
            "lc_tp2_frac_bps" => self.lc_tp2_frac_bps = bp(value)?,
            "lc_tp3_bps" => self.lc_tp3_bps = bp(value)?,
            "lc_tp3_frac_bps" => self.lc_tp3_frac_bps = bp(value)?,
            "lc_cvd_hold_frac_bps" => self.lc_cvd_hold_frac_bps = bp(value)?,
            "lc_stall_ticks" => self.lc_stall_ticks = nonneg(value)?.max(1),
            "lc_max_hold_ticks" => self.lc_max_hold_ticks = nonneg(value)?.max(1),
            "lc_precursor_drop_bps" => self.lc_precursor_drop_bps = bp(value)?,
            "thesis_persist_obs" => {
                self.thesis_persist_obs = u32::try_from(nonneg(value)?).unwrap_or(1).max(1);
            }
            "meta_taxonomy_version" => self.meta_taxonomy_version = bp(value)?,
            "meta_max_categories" => self.meta_max_categories = sz(value)?.max(1),
            "meta_max_creators_per_cat" => self.meta_max_creators_per_cat = sz(value)?.max(1),
            "creator_track_cap" => self.creator_track_cap = sz(value)?.max(1),
            "meta_min_share_bps" => self.meta_min_share_bps = nonneg(value)?,
            "meta_accel_threshold" => self.meta_accel_threshold = value,
            "meta_min_breadth" => self.meta_min_breadth = bp(value)?,
            "meta_rank_bonus_bp" => self.meta_rank_bonus_bp = bp(value)?,
            "meta_saturation_haircut_bp" => self.meta_saturation_haircut_bp = bp(value)?,
            "creator_fade_sold_bps" => self.creator_fade_sold_bps = nonneg(value)?,
            "creator_dump_veto_enable" => self.creator_dump_veto_enable = value != 0,
            "creator_dump_veto_bp" => self.creator_dump_veto_bp = nonneg(value)?,
            "creator_dump_veto_strict_bp" => self.creator_dump_veto_strict_bp = nonneg(value)?,
            "derived_targets_enable" => self.derived_targets_enable = value != 0,
            "target_margin_mult_bp" => self.target_margin_mult_bp = bp(value)?,
            "target_floor_bp" => self.target_floor_bp = bp(value)?,
            "target_ceiling_bp" => self.target_ceiling_bp = bp(value)?,
            "into_strength_exit_enable" => self.into_strength_exit_enable = value != 0,
            "into_strength_climax_bp" => self.into_strength_climax_bp = bp(value)?,
            "vol_stop_enable" => self.vol_stop_enable = value != 0,
            "vol_stop_scale_bp" => self.vol_stop_scale_bp = bp(value)?,
            "setup_classifier_enable" => self.setup_classifier_enable = value != 0,
            "entry_mode_leaves_enable" => self.entry_mode_leaves_enable = value != 0,
            "money_proxy_enable" => self.money_proxy_enable = value != 0,
            "money_proxy_holder_flow_enable" => {
                self.money_proxy_holder_flow_enable = value != 0;
            }
            "holder_concentration_enable" => {
                self.holder_concentration_enable = value != 0;
            }
            "narrative_class_enable" => self.narrative_class_enable = value != 0,
            "platform_lead_enable" => self.platform_lead_enable = value != 0,
            "deployer_screen_enable" => self.deployer_screen_enable = value != 0,
            "fee_floor_enable" => self.fee_floor_enable = value != 0,
            "probe_budget_enable" => self.probe_budget_enable = value != 0,
            "alpha_call_lane_enable" => self.alpha_call_lane_enable = value != 0,
            "designated_caller_enable" => self.designated_caller_enable = value != 0,
            "designated_caller_weight" => self.designated_caller_weight = nonneg(value)?,
            "alpha_exit_pressure_enable" => self.alpha_exit_pressure_enable = value != 0,
            "brain_enable" => self.brain_enable = value != 0,
            "brain_min_sample" => self.brain_min_sample = bp(value)?.max(1),
            "brain_recall_max_distance" => self.brain_recall_max_distance = bp(value)?,
            "brain_haircut_enable" => self.brain_haircut_enable = value != 0,
            "brain_haircut_win_rate_bp" => self.brain_haircut_win_rate_bp = bp(value)?,
            "brain_veto_win_rate_bp" => self.brain_veto_win_rate_bp = bp(value)?,
            "brain_haircut_mult_bp" => self.brain_haircut_mult_bp = bp(value)?,
            "brain_persist_enable" => self.brain_persist_enable = value != 0,
            "reflect_every_ticks" => self.reflect_every_ticks = nonneg(value)?.max(1),
            "reflect_weight_step_bp" => self.reflect_weight_step_bp = bp(value)?,
            "reflect_weight_floor_bp" => self.reflect_weight_floor_bp = bp(value)?,
            "reflect_weight_ceiling_bp" => self.reflect_weight_ceiling_bp = bp(value)?,
            "bar_trades_per_bar" => self.bar_trades_per_bar = nonneg(value)?.max(1),
            "structure_min_bars" => self.structure_min_bars = sz(value)?.max(3),
            "structure_downtrend_haircut_bp" => self.structure_downtrend_haircut_bp = bp(value)?,
            "universe_age_exempt_slots" => self.universe_age_exempt_slots = bp(value)?,
            "universe_window_ticks" => self.universe_window_ticks = nonneg(value)?.max(1),
            "universe_min_trades" => self.universe_min_trades = bp(value)?,
            "universe_min_entities" => self.universe_min_entities = bp(value)?,
            "universe_wash_ratio_max" => self.universe_wash_ratio_max = bp(value)?.max(1),
            "universe_min_liquidity_lamports" => {
                self.universe_min_liquidity_lamports = nonneg(value)?;
            }
            "narrative_decay_bp" => self.narrative_decay_bp = bp(value)?,
            "narrative_decay_step_ticks" => {
                self.narrative_decay_step_ticks = nonneg(value)?.max(1);
            }
            "narrative_decay_floor" => self.narrative_decay_floor = nonneg(value)?,
            "landing_base_bps" => self.landing_base_bps = bp(value)?,
            "landing_penalty_k_bps" => self.landing_penalty_k_bps = bp(value)?,
            "baseline_margin_lamports" => self.baseline_margin_lamports = value,
            "baseline_min_trades" => self.baseline_min_trades = bp(value)?,
            "scale_confirm_auth_min_bp" => self.scale_confirm_auth_min_bp = bp(value)?,
            "promote_corroboration_quota" => self.promote_corroboration_quota = sz(value)?,
            "expectancy_min_lane_trades" => {
                self.expectancy_min_lane_trades = bp(value)?.max(1);
            }
            "brain_analysis_enable" => self.brain_analysis_enable = value != 0,
            "brain_reflect_enable" => self.brain_reflect_enable = value != 0,
            "brain_reflect_step_bp" => self.brain_reflect_step_bp = bp(value)?,
            "brain_decay_min_sample" => self.brain_decay_min_sample = bp(value)?.max(1),
            other => return Err(ConfigError::UnknownKey(other.to_string())),
        }
        Ok(())
    }

    /// Apply a single `key = <path>` override for the small set of PATH-valued
    /// keys. Returns `Err` on an unknown key or a path longer than
    /// [`BRAIN_PATH_CAP`] (a truncated path is a wrong path — it fails loud, §18).
    pub fn apply_path(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        match key {
            "brain_path" => {
                self.brain_path = CfgPath::from_str_checked(value)
                    .ok_or_else(|| ConfigError::PathTooLong(key.to_string()))?;
            }
            "brain_analysis_path" => {
                self.brain_analysis_path = CfgPath::from_str_checked(value)
                    .ok_or_else(|| ConfigError::PathTooLong(key.to_string()))?;
            }
            other => return Err(ConfigError::UnknownKey(other.to_string())),
        }
        Ok(())
    }

    /// Whether `key` is one of the PATH-valued keys handled by
    /// [`Config::apply_path`] rather than the integer [`Config::apply`].
    #[must_use]
    pub fn is_path_key(key: &str) -> bool {
        matches!(key, "brain_path" | "brain_analysis_path")
    }

    /// Parse a dependency-free config document over a `dev_portable()` base.
    ///
    /// Grammar: one `key = value` per line; `#` starts a comment; blank lines are
    /// ignored; `value` is a base-10 integer, EXCEPT for the small closed set of
    /// path-valued keys ([`Config::is_path_key`]) whose value is taken verbatim
    /// after trimming. Every override is validated.
    pub fn from_str_over_default(text: &str) -> Result<Self, ConfigError> {
        let mut cfg = Self::dev_portable();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or(ConfigError::Syntax(lineno + 1))?;
            let key = k.trim();
            if Self::is_path_key(key) {
                cfg.apply_path(key, v.trim())?;
                continue;
            }
            let val: i64 = v
                .trim()
                .parse()
                .map_err(|_| ConfigError::Syntax(lineno + 1))?;
            cfg.apply(key, val)?;
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject internally inconsistent envelopes before they can drive a decision.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.reflect_weight_floor_bp > self.reflect_weight_ceiling_bp {
            return Err(ConfigError::Inconsistent("weight floor exceeds ceiling"));
        }
        if self.promote_k == 0 {
            return Err(ConfigError::Inconsistent("promote_k must be positive"));
        }
        if self.narrative_stage_lo_fp > self.narrative_stage_hi_fp {
            return Err(ConfigError::Inconsistent(
                "narrative stage lo band exceeds hi band",
            ));
        }
        if self.confirmed_capacity_mult == 0 {
            return Err(ConfigError::Inconsistent(
                "confirmed_capacity_mult must be positive",
            ));
        }
        if self.watchlist_capacity == 0 {
            return Err(ConfigError::Inconsistent(
                "watchlist_capacity must be positive",
            ));
        }
        // A rank/size haircut can never remove more than the whole score/size (100%).
        if self.meta_saturation_haircut_bp > 10_000 {
            return Err(ConfigError::Inconsistent(
                "meta_saturation_haircut_bp exceeds 100%",
            ));
        }
        // Structure is reduce-only (§21.6/§56.2): the haircut lives in [0,100%].
        if self.structure_downtrend_haircut_bp > 10_000 {
            return Err(ConfigError::Inconsistent(
                "structure_downtrend_haircut_bp exceeds 100%",
            ));
        }
        // Decay is multiplicative shrinkage: > 100% would GROW stale evidence.
        if self.narrative_decay_bp > 10_000 {
            return Err(ConfigError::Inconsistent("narrative_decay_bp exceeds 100%"));
        }
        // A landing probability is a probability.
        if self.landing_base_bps > 10_000 {
            return Err(ConfigError::Inconsistent("landing_base_bps exceeds 100%"));
        }
        // The creator-fade trigger is a fraction of peak sold, so it lives in [0,100%].
        if self.creator_fade_sold_bps > 10_000 {
            return Err(ConfigError::Inconsistent(
                "creator_fade_sold_bps exceeds 100%",
            ));
        }
        // §26 confirmed-dump veto: a fraction of peak sold, in [0,100%]; the strict
        // (classified-extractor) bar must not exceed the base bar, and the veto
        // must sit strictly above the graded fade (fade below, veto above).
        if self.creator_dump_veto_bp > 10_000 {
            return Err(ConfigError::Inconsistent(
                "creator_dump_veto_bp exceeds 100%",
            ));
        }
        if self.creator_dump_veto_strict_bp > self.creator_dump_veto_bp {
            return Err(ConfigError::Inconsistent(
                "creator_dump_veto_strict_bp exceeds creator_dump_veto_bp",
            ));
        }
        // Bankroll-fraction sanity: fractions are of-deployable and must be ≤ 100%;
        // promotion must stay inside the total risk budget; drawdown tiers ascend.
        if self.f_base_bp > 10_000 || self.total_risk_cap_bp > 10_000 {
            return Err(ConfigError::Inconsistent("bankroll fraction exceeds 100%"));
        }
        if self.x_min_promote_cap_bp > self.total_risk_cap_bp {
            return Err(ConfigError::Inconsistent(
                "x_min_promote_cap_bp exceeds total_risk_cap_bp",
            ));
        }
        if !(self.dd_tier1_bp <= self.dd_tier2_bp && self.dd_tier2_bp <= self.dd_tier3_bp) {
            return Err(ConfigError::Inconsistent("drawdown tiers must ascend"));
        }
        // Toxicity tiers ascend and stay in bps range.
        if !(self.vpin_warn_bp <= self.vpin_toxic_bp && self.vpin_toxic_bp <= self.vpin_veto_bp)
            || self.vpin_veto_bp > 10_000
        {
            return Err(ConfigError::Inconsistent(
                "vpin tiers must ascend within bps",
            ));
        }
        if self.vpin_v_min_lamports > self.vpin_v_max_lamports {
            return Err(ConfigError::Inconsistent(
                "vpin bucket floor exceeds ceiling",
            ));
        }
        // Regime deadband must be a real band; REVERT can only reduce size.
        if self.roll_revert_bp >= self.roll_trend_bp {
            return Err(ConfigError::Inconsistent("roll regime deadband inverted"));
        }
        if self.revert_size_mult_bp > 10_000 {
            return Err(ConfigError::Inconsistent(
                "revert_size_mult_bp exceeds 100%",
            ));
        }
        // Take-profit ladder must ascend.
        if !(self.lc_tp1_bps <= self.lc_tp2_bps && self.lc_tp2_bps <= self.lc_tp3_bps) {
            return Err(ConfigError::Inconsistent("take-profit ladder must ascend"));
        }
        // §24 LAW 2 derived-target envelope must be a real band (floor ≤ ceiling).
        if self.target_floor_bp > self.target_ceiling_bp {
            return Err(ConfigError::Inconsistent(
                "target floor exceeds target ceiling",
            ));
        }
        // LAW B3 is REDUCE-ONLY (§29.5/§56.2): a recall "haircut" that grew size
        // would invert the whole law, so the multiplier lives in [0, 100%].
        if self.brain_haircut_mult_bp > 10_000 {
            return Err(ConfigError::Inconsistent(
                "brain_haircut_mult_bp exceeds 100% (LAW B3 is reduce-only)",
            ));
        }
        // The veto bar is strictly harsher evidence than the haircut bar: a class
        // bad enough to refuse is by definition bad enough to fade.
        if self.brain_veto_win_rate_bp > self.brain_haircut_win_rate_bp {
            return Err(ConfigError::Inconsistent(
                "brain_veto_win_rate_bp exceeds brain_haircut_win_rate_bp",
            ));
        }
        // Win rates are rates.
        if self.brain_haircut_win_rate_bp > 10_000 {
            return Err(ConfigError::Inconsistent(
                "brain_haircut_win_rate_bp exceeds 100%",
            ));
        }
        // §46: an estimate over fewer than one episode is not an estimate.
        if self.brain_min_sample == 0 {
            return Err(ConfigError::Inconsistent(
                "brain_min_sample must be positive (§46 fail-closed)",
            ));
        }
        // §46/§56 LAW B7: a decay flag over a zero sample is not evidence, it is a
        // coin flip with a §-citation. The floor is structural, not advisory.
        if self.brain_decay_min_sample == 0 {
            return Err(ConfigError::Inconsistent(
                "brain_decay_min_sample must be positive (§46 fail-closed)",
            ));
        }
        // §56.2 LAW B7: the brain downweight lives INSIDE the reflection envelope.
        // A step wider than the envelope itself could jump the floor in one pass,
        // which is exactly the unbounded adaptation the envelope exists to forbid.
        if self.brain_reflect_step_bp
            > self
                .reflect_weight_ceiling_bp
                .saturating_sub(self.reflect_weight_floor_bp)
        {
            return Err(ConfigError::Inconsistent(
                "brain_reflect_step_bp exceeds the §56.2 reflection envelope width",
            ));
        }
        Ok(())
    }
}

/// Why a config document was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// A line did not parse as `key = <integer>`; carries the 1-based line number.
    Syntax(usize),
    /// A key the engine does not recognise.
    UnknownKey(String),
    /// A value outside the field's representable/allowed range.
    OutOfRange(String, i64),
    /// A path-valued key's value exceeded [`BRAIN_PATH_CAP`]. Refused rather than
    /// truncated: a truncated path is a *different* path (§18 fail-loud).
    PathTooLong(String),
    /// The envelope is internally inconsistent.
    Inconsistent(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Syntax(n) => write!(f, "config syntax error on line {n}"),
            ConfigError::UnknownKey(k) => write!(f, "unknown config key: {k}"),
            ConfigError::OutOfRange(k, v) => write!(f, "value {v} out of range for {k}"),
            ConfigError::PathTooLong(k) => {
                write!(f, "path value for {k} exceeds {BRAIN_PATH_CAP} bytes")
            }
            ConfigError::Inconsistent(m) => write!(f, "inconsistent config: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}
