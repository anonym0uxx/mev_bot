//! The scalp stage: turn an admitted candidate into a simulated fill.
//!
//! On the laptop no capital moves — this stage drives the real
//! `pump_quant_simulator::fill::simulate_fill` model to produce a deterministic,
//! calibrated realized PnL for a scalp of a chosen size, then expresses it as a
//! `pump_quant_evaluator` reconciliation trade so the same net-SOL accounting the
//! evaluator uses everywhere applies here too.
//!
//! Live entry is a different world: it requires signing keys and fund movement,
//! which are Tier-0 human-gated and never reachable from this binary. This stage is
//! strictly the paper/replay executor (§54 fill modes A/B/C).

use crate::config::{Config, FillModeCfg};
use pump_quant_evaluator::evaluator_stats::{Lane as EvalLane, ReconTrade};
use pump_quant_simulator::fill::{
    simulate_fill, CostModel, ExitImpairment, FillMode, ImpairmentLevel, MarketState,
};
use pump_quant_simulator::terminal_loss::TerminalLossPolicy;
use pump_quant_strategy::economic_gate::SizeBand;
use pump_quant_watchlist::candidate::Lane as WlLane;

/// The result of paper-scalping one admitted candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalpResult {
    /// Size actually deployed, lamports (chosen within the admitted band).
    pub size_lamports: u64,
    /// Realized PnL from the calibrated fill model, lamports (signed).
    pub net_pnl_lamports: i128,
    /// The reconciliation trade handed to the evaluator (its net equals
    /// `net_pnl_lamports` by construction).
    pub recon: ReconTrade,
    /// Whether the simulated exit was claimable (false in signal-replay mode).
    pub claimable: bool,
}

/// Map the config's fill mode onto the simulator's.
const fn fill_mode(cfg: FillModeCfg) -> FillMode {
    match cfg {
        FillModeCfg::SignalReplay => FillMode::SignalReplay,
        FillModeCfg::OptimisticCeiling => FillMode::OptimisticCeiling,
        FillModeCfg::AdversarialRealistic => FillMode::Adversarial(ImpairmentLevel::Realistic),
        FillModeCfg::AdversarialPessimistic => FillMode::Adversarial(ImpairmentLevel::Pessimistic),
    }
}

/// Which evaluator lane a watchlist source lane reconciles into. Sniper/early
/// discovery is `Early`; active/graduation scalping is `Scalp`.
const fn eval_lane(lane: WlLane) -> EvalLane {
    match lane {
        WlLane::CreationSniper | WlLane::EarlyConfirmation => EvalLane::Early,
        WlLane::GraduationTransition | WlLane::ActiveMarketScalp => EvalLane::Scalp,
    }
}

/// Choose the deploy size within the admitted band. Uses the cost-minimising
/// `x_cost` point the size-band leaf already computed and clamped into `[x_min,
/// x_max]`; no fresh magic number is introduced here.
#[must_use]
pub fn choose_size(band: &SizeBand) -> u64 {
    band.x_cost
}

/// Full basis-point scale (10_000 = 1.0×): a size multiplier of this value is the
/// identity, applying no haircut.
pub const SIZE_MULT_ONE_BP: u32 = 10_000;

/// Apply a corroboration-tier size multiplier (bps of [`SIZE_MULT_ONE_BP`]) to the
/// chosen deploy size, then re-clamp **into the admitted band** `[x_min, x_max]`.
///
/// Callers pass `mult_bps <= 10_000`, so this can only ever *reduce* size — the
/// fade-first expression of creator-distribution / category-saturation risk as a
/// graded haircut, never a veto (§22 behavioral-risk clause). At exactly
/// `SIZE_MULT_ONE_BP` the result is byte-identical to [`choose_size`] (the golden
/// path): `x_cost × 10_000 / 10_000 == x_cost`, which already lies in the band.
/// `u128` intermediate so the scale never overflows (§22 explicit overflow).
#[inline]
#[must_use]
fn apply_size_mult(band: &SizeBand, mult_bps: u32) -> u64 {
    let x_cost = choose_size(band);
    let scaled = (u128::from(x_cost) * u128::from(mult_bps) / u128::from(SIZE_MULT_ONE_BP)) as u64;
    scaled.clamp(band.x_min, band.x_max)
}

/// Paper-scalp an admitted candidate.
///
/// `expected_move_bps` is the same favourable move the gate priced the band on,
/// applied by the fill model as the realized mark move. `depth_lamports` is the
/// confirmed on-chain sellable depth. The impairment envelope is derived from the
/// config's fee/tip parameters — nothing here is hard-coded.
#[must_use]
pub fn scalp(
    lane: WlLane,
    band: &SizeBand,
    expected_move_bps: u32,
    depth_lamports: u64,
    size_mult_bps: u32,
    cfg: &Config,
) -> ScalpResult {
    // Corroboration-tier size haircut: at SIZE_MULT_ONE_BP this is exactly
    // `choose_size(band)` (golden path unchanged); below it, a graded reduction
    // clamped back into the admitted band — never a veto.
    let size = apply_size_mult(band, size_mult_bps);

    let market = MarketState {
        notional_lamports: size,
        // The fill model takes a signed move; a scalp is entered long, so the
        // favourable move is positive. Saturate into i32 defensively.
        move_bps: i32::try_from(expected_move_bps).unwrap_or(i32::MAX),
        depth_lamports,
        impact_k_bps: cfg.sim_impact_k_bps,
    };
    let costs = CostModel {
        entry_fee_bps: cfg.entry_fee_bps,
        exit_fee_bps: cfg.exit_fee_bps,
        entry_tip_lamports: cfg.entry_tip_lamports,
        exit_tip_lamports: cfg.exit_tip_lamports,
    };
    // Impairment magnitudes reuse the fee/tip envelope so the adversarial modes are
    // parameterised by the same operator config, never by inline constants.
    let imp = ExitImpairment {
        first_sell_penalty_bps: cfg.exit_fee_bps,
        retry_slippage_bps: cfg.gate_protocol_bps,
        fee_escalation_bps: cfg.entry_fee_bps,
        retry_tip_lamports: cfg.exit_tip_lamports,
        unexitable: false,
    };

    let fill = simulate_fill(
        &market,
        &costs,
        &imp,
        fill_mode(cfg.fill_mode),
        &TerminalLossPolicy::WriteToZero,
    );

    // Express the realized PnL as a reconciliation trade. Tips are the explicit,
    // exactly-known cost class; venue fees and impact are already netted into the
    // fill model's proceeds, so `gross = net + tips` keeps the evaluator's net equal
    // to the fill model's net (single net authority, §26 reconciliation).
    let tips = (cfg.entry_tip_lamports as u128).saturating_add(cfg.exit_tip_lamports as u128);
    let gross = fill.net_pnl_lamports.saturating_add(tips as i128);
    let recon = ReconTrade {
        lane: eval_lane(lane),
        gross_lamports: gross,
        fees: 0,
        tips,
        failed_costs: 0,
        mint: [0u8; 32],
        entry_price_fp: 0,
        exit_price_fp: 0,
        size_lamports: size,
        archetype: 0,
        exit_reason_code: 0,
        mfe_bps: 0,
        mae_bps: 0,
        entry_tick: 0,
    };

    ScalpResult {
        size_lamports: size,
        net_pnl_lamports: fill.net_pnl_lamports,
        recon,
        claimable: fill.claimable,
    }
}

/// §55 capacity-curve report over the mandated size grid, priced by the SAME
/// fill/cost/impairment models the paper scalp path uses — so the curve is an
/// honest projection of this config's economics, not a separate optimistic model.
/// Report-only: nothing in the decision path reads this.
#[must_use]
pub fn capacity_report(
    cfg: &Config,
    depth_lamports: u64,
) -> Vec<pump_quant_simulator::capacity::CapacityPoint> {
    let market = MarketState {
        // The base notional is overridden per grid size by `capacity_curve`.
        notional_lamports: 0,
        move_bps: i32::try_from(cfg.gate_expected_move_bps).unwrap_or(i32::MAX),
        depth_lamports,
        impact_k_bps: cfg.sim_impact_k_bps,
    };
    let costs = CostModel {
        entry_fee_bps: cfg.entry_fee_bps,
        exit_fee_bps: cfg.exit_fee_bps,
        entry_tip_lamports: cfg.entry_tip_lamports,
        exit_tip_lamports: cfg.exit_tip_lamports,
    };
    let imp = ExitImpairment {
        first_sell_penalty_bps: cfg.exit_fee_bps,
        retry_slippage_bps: cfg.gate_protocol_bps,
        fee_escalation_bps: cfg.entry_fee_bps,
        retry_tip_lamports: cfg.exit_tip_lamports,
        unexitable: false,
    };
    let landing = pump_quant_simulator::capacity::LandingModel {
        base_bps: cfg.landing_base_bps,
        penalty_k_bps: cfg.landing_penalty_k_bps,
    };
    pump_quant_simulator::capacity::capacity_curve(
        &market,
        &costs,
        &imp,
        fill_mode(cfg.fill_mode),
        &TerminalLossPolicy::WriteToZero,
        &landing,
    )
}
