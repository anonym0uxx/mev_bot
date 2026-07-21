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
    cfg: &Config,
) -> ScalpResult {
    let size = choose_size(band);

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
    };

    ScalpResult {
        size_lamports: size,
        net_pnl_lamports: fill.net_pnl_lamports,
        recon,
        claimable: fill.claimable,
    }
}
