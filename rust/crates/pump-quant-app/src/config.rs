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
    /// Fixed per-attempt cost in lamports (base tip + fee floor) the gate must clear.
    pub gate_base_fixed_lamports: u64,
    /// Expected send-failure rate, bps, inflating the effective fixed cost.
    pub gate_fail_rate_bps: u32,
    /// Round-trip protocol fee, bps.
    pub gate_protocol_bps: u32,
    /// Safety margin, bps, demanded on top of costs before admitting size.
    pub gate_margin_bps: u32,
    /// Linear impact-curve denominator: `impact_bps ≈ size / den`. Operator-fit to
    /// observed depth response; larger = deeper book = more size tolerated.
    pub gate_impact_den: u64,

    // ---- scalp / paper fill ----
    /// Fill semantics for the paper engine.
    pub fill_mode: FillModeCfg,
    /// Entry fee, bps, charged by the simulated venue.
    pub entry_fee_bps: u32,
    /// Exit fee, bps.
    pub exit_fee_bps: u32,
    /// Entry tip, lamports.
    pub entry_tip_lamports: u64,
    /// Exit tip, lamports.
    pub exit_tip_lamports: u64,
    /// Impact `k` (bps) for the simulator's constant-product impact model.
    pub sim_impact_k_bps: u32,

    // ---- bankroll / dynamic sizing (§33 Layer 1, delta-§1) ----
    /// Verified starting bankroll, lamports. ANY amount: every sizing limit below
    /// derives from `deployable = bankroll − survival_floor`, so the same config
    /// serves 0.75, 2, 10 or 100 SOL (scale-invariant until the per-market cost
    /// floor x_min carves out the venue-viability region).
    pub bankroll_initial_lamports: u64,
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
}

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

            gate_expected_move_bps: 300,
            gate_base_fixed_lamports: 50_000,
            gate_fail_rate_bps: 500,
            gate_protocol_bps: 100,
            gate_margin_bps: 50,
            gate_impact_den: 1_000_000,

            fill_mode: FillModeCfg::OptimisticCeiling,
            entry_fee_bps: 100,
            exit_fee_bps: 100,
            entry_tip_lamports: 10_000,
            exit_tip_lamports: 10_000,
            sim_impact_k_bps: 50,

            bankroll_initial_lamports: 2_000_000_000, // 2 SOL start; ANY amount works
            floor_fraction_bps: 5_000,                // floor = max(0.5 SOL, 50% of start)
            f_base_bp: 150,                           // 1.5% of deployable per position
            total_risk_cap_bp: 450,                   // ≤4.5% of deployable at risk total
            max_concurrent_positions: 3,              // 3 × 150bp = 450bp (consistent)
            x_min_promote_cap_bp: 400,                // promote to x_min only ≤4% of deployable
            promote_min_haircut_bp: 8_000,            // never promote a risk-faded trade
            dd_tier1_bp: 1_500,                       // −15% dd → half fraction
            dd_tier2_bp: 3_000,                       // −30% dd → quarter fraction
            dd_tier3_bp: 5_000,                       // −50% dd → probe-only survival
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

            meta_taxonomy_version: 0, // matches meta::TAXONOMY_V0
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
            "fill_mode" => {
                self.fill_mode = FillModeCfg::from_code(value)
                    .ok_or(ConfigError::OutOfRange(key.to_string(), value))?
            }
            "entry_fee_bps" => self.entry_fee_bps = bp(value)?,
            "exit_fee_bps" => self.exit_fee_bps = bp(value)?,
            "entry_tip_lamports" => self.entry_tip_lamports = nonneg(value)?,
            "exit_tip_lamports" => self.exit_tip_lamports = nonneg(value)?,
            "sim_impact_k_bps" => self.sim_impact_k_bps = bp(value)?,
            "bankroll_initial_lamports" => self.bankroll_initial_lamports = nonneg(value)?,
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
            "reflect_every_ticks" => self.reflect_every_ticks = nonneg(value)?.max(1),
            "reflect_weight_step_bp" => self.reflect_weight_step_bp = bp(value)?,
            "reflect_weight_floor_bp" => self.reflect_weight_floor_bp = bp(value)?,
            "reflect_weight_ceiling_bp" => self.reflect_weight_ceiling_bp = bp(value)?,
            other => return Err(ConfigError::UnknownKey(other.to_string())),
        }
        Ok(())
    }

    /// Parse a dependency-free config document over a `dev_portable()` base.
    ///
    /// Grammar: one `key = value` per line; `#` starts a comment; blank lines are
    /// ignored; `value` is a base-10 integer. Every override is validated.
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
        // The creator-fade trigger is a fraction of peak sold, so it lives in [0,100%].
        if self.creator_fade_sold_bps > 10_000 {
            return Err(ConfigError::Inconsistent(
                "creator_fade_sold_bps exceeds 100%",
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
    /// The envelope is internally inconsistent.
    Inconsistent(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Syntax(n) => write!(f, "config syntax error on line {n}"),
            ConfigError::UnknownKey(k) => write!(f, "unknown config key: {k}"),
            ConfigError::OutOfRange(k, v) => write!(f, "value {v} out of range for {k}"),
            ConfigError::Inconsistent(m) => write!(f, "inconsistent config: {m}"),
        }
    }
}

impl std::error::Error for ConfigError {}
