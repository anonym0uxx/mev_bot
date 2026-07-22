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
