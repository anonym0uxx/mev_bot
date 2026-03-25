/**
 * @module entry/engine
 * Final Entry Engine per spec section 9.
 *
 * QUANT REDESIGN based on:
 * - Power law return distribution (Gabaix 2009, Cont 2001)
 * - Information asymmetry in token launches (Easley/O'Hara)
 * - Alpha decay in high-frequency markets (Budish/Cramton/Shim 2015)
 * - Adverse selection framework for meme coins
 *
 * Entry doctrine: enter ONLY when EV_enter_now > 0 AND EntryEdge > 0
 * AND breadth confirms velocity AND execution quality acceptable.
 */

import { createLogger } from '../utils/logger';
import { nowMs, ageS } from '../utils/time';
import {
  CandidatePacket, EntryEVCalculation, PositionSizing,
  ProbabilityOutputs, Regime,
} from '../types/state';
import { FeatureSnapshot } from '../types/features';
import { PumpQuantConfig } from '../types/config';

const log = createLogger('entry-engine');

export interface EntryDecision {
  shouldEnter: boolean;
  reason: string;
  ev: EntryEVCalculation;
  sizing: PositionSizing | null;
  hardFilterRejection: string | null;
}

/**
 * Evaluate whether to enter a position for a candidate token.
 *
 * Non-negotiable: EV_enter_now > 0 AND EntryEdge > 0
 */
export function evaluateEntry(
  packet: CandidatePacket,
  probabilities: ProbabilityOutputs,
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  currentPositionCount: number,
  dailyLossSol: number,
  isPaper: boolean
): EntryDecision {
  // ====== 9.2 HARD ENTRY FILTERS ======
  const hardFilter = checkHardFilters(packet, features, config, currentPositionCount, dailyLossSol);
  if (hardFilter) {
    if (features.breadth_topology.unique_buyers_total >= 2) {
      log.info(`Hard filter reject ${packet.symbol || packet.mint.slice(0,8)}: ${hardFilter} (buyers=${features.breadth_topology.unique_buyers_total})`);
    }
    return {
      shouldEnter: false,
      reason: `Hard filter: ${hardFilter}`,
      ev: emptyEV(),
      sizing: null,
      hardFilterRejection: hardFilter,
    };
  }

  // ====== 9.3 ENTRY FORMULAS (Quant Redesign) ======

  const horizon = config.entry.ev_enter_horizon_s;
  const P_continuation = horizon <= 5
    ? probabilities.P_continuation_5s
    : probabilities.P_continuation_15s;
  const P_reversal = horizon <= 5
    ? probabilities.P_reversal_5s
    : probabilities.P_reversal_15s;
  const P_manipulation = probabilities.P_manipulation_event;

  const positionSize = config.risk.quick_spend_sol;

  // ---- Round-trip friction (computed ONCE) ----
  const roundTripFriction = computeRoundTripFriction(features, config, packet.regime);
  const routeEvAdjustment = features.friction_execution.route_ev_adjustment;

  // ---- Payoff estimates using power law distribution ----
  // Pump.fun empirical distribution: right-skewed with fat tail (Gabaix 2009)
  // E[return | continuation] ≈ 80% GROSS (mean of right tail: 50-200%+ range)
  // E[return | organic reversal] ≈ -stop_loss GROSS (bounded by stop)
  // E[return | manipulation reversal] ≈ -1.5x stop GROSS (faster/harder dump)
  //
  // CRITICAL: friction is a FLAT cost (always paid on entry+exit regardless of outcome)
  // so it's subtracted ONCE at the end, NOT embedded in per-scenario returns.
  // Previous version double-counted friction (in returns AND as flat cost).
  const E_return_continuation = 0.80;
  const E_return_organic_reversal = -config.risk.raw_stop_pct;
  const E_return_manipulation_reversal = -(config.risk.raw_stop_pct * 1.5);

  // ---- Probability decomposition ----
  // P_manipulation is a CONDITIONAL: given reversal, probability it's manipulation-driven
  // This avoids the triple-counting bug
  // Total: P_cont + P_organic_rev + P_manip_rev = 1
  const P_organic_reversal = P_reversal * (1 - P_manipulation);
  const P_manipulation_reversal = P_reversal * P_manipulation;

  // ---- EV_enter_now ----
  // Single clean formula: expected return across all scenarios minus round-trip friction
  const EV_enter_now =
    P_continuation * positionSize * E_return_continuation +
    P_organic_reversal * positionSize * E_return_organic_reversal +
    P_manipulation_reversal * positionSize * E_return_manipulation_reversal -
    roundTripFriction +
    routeEvAdjustment;

  // ---- EV_wait ----
  // On Pump.fun, alpha decays fast. Waiting has negative expected value when
  // there's positive velocity (you miss the move).
  const EV_wait = computeEVWait(packet, features, probabilities, config);

  const EntryEdge = EV_enter_now - Math.max(0, EV_wait);

  const ev: EntryEVCalculation = {
    EV_enter_now,
    EV_wait,
    EntryEdge,
    upside_net: P_continuation * positionSize * E_return_continuation,
    downside_net: -(P_organic_reversal * positionSize * E_return_organic_reversal +
                    P_manipulation_reversal * positionSize * E_return_manipulation_reversal),
    manipulation_cost: -P_manipulation_reversal * positionSize * E_return_manipulation_reversal,
    friction_cost_now: roundTripFriction,
    route_ev_adjustment: routeEvAdjustment,
  };

  // Log all evaluations
  log.info(`Entry eval ${packet.symbol || packet.mint.slice(0,8)}: EV=${EV_enter_now.toFixed(6)} Edge=${EntryEdge.toFixed(6)} P_cont=${P_continuation.toFixed(3)} P_rev=${P_reversal.toFixed(3)} P_manip=${P_manipulation.toFixed(3)} friction=${roundTripFriction.toFixed(6)} breadth=${features.breadth_topology.breadth_score.toFixed(3)} buyers=${features.breadth_topology.unique_buyers_total}`);

  // ====== 9.4 OBSERVATION PREMIUM ======
  // Reduced window: alpha decays fast on Pump.fun (Budish/Cramton/Shim 2015)
  const tokenAge = ageS(packet.created_at);
  const dynamicObsWindow = features.flow_momentum.buy_notional_velocity_5s > 0.2
    ? 3  // Fast-moving token: 3s observation
    : config.entry.observation_window_s;

  if (tokenAge < dynamicObsWindow) {
    if (EV_enter_now > 0 && EntryEdge > config.entry.min_entry_edge * 2) {
      log.info(`Observation override for ${packet.symbol || packet.mint.slice(0,8)}: age=${tokenAge.toFixed(1)}s Edge=${EntryEdge.toFixed(6)}`);
    } else {
      if (EV_enter_now > 0) {
        log.info(`Obs window block ${packet.symbol || packet.mint.slice(0,8)}: age=${tokenAge.toFixed(1)}s < ${dynamicObsWindow}s (EV=${EV_enter_now.toFixed(6)} but edge ${EntryEdge.toFixed(6)} < ${(config.entry.min_entry_edge * 2).toFixed(6)})`);
      }
      return {
        shouldEnter: false,
        reason: `Observation: ${tokenAge.toFixed(1)}s < ${dynamicObsWindow}s`,
        ev,
        sizing: null,
        hardFilterRejection: null,
      };
    }
  }

  // Log when we pass observation
  if (EV_enter_now > 0) {
    log.info(`POST-OBS ${packet.symbol || packet.mint.slice(0,8)}: age=${tokenAge.toFixed(1)}s EV=${EV_enter_now.toFixed(6)} Edge=${EntryEdge.toFixed(6)} breadth=${features.breadth_topology.breadth_score.toFixed(3)}`);
  }

  // ====== CORE ENTRY DECISION ======
  if (EV_enter_now <= 0) {
    return {
      shouldEnter: false,
      reason: `EV_enter_now <= 0: ${EV_enter_now.toFixed(6)}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  if (EntryEdge <= config.entry.min_entry_edge) {
    return {
      shouldEnter: false,
      reason: `EntryEdge ${EntryEdge.toFixed(6)} below threshold ${config.entry.min_entry_edge}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  // Breadth confirms velocity
  if (features.breadth_topology.breadth_score < config.entry.min_breadth_for_entry) {
    return {
      shouldEnter: false,
      reason: `Breadth ${features.breadth_topology.breadth_score.toFixed(3)} below ${config.entry.min_breadth_for_entry}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  // Execution quality
  if (features.friction_execution.route_score < 0.3) {
    return {
      shouldEnter: false,
      reason: `Route score too low: ${features.friction_execution.route_score.toFixed(3)}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  // ====== 9.5 POSITION SIZING ======
  const sizing = computePositionSizing(config, features, roundTripFriction);

  if (sizing.position_size <= 0) {
    return {
      shouldEnter: false,
      reason: 'Position size computed to zero',
      ev,
      sizing,
      hardFilterRejection: null,
    };
  }

  log.info(`🟢 ENTRY APPROVED ${packet.symbol || packet.mint.slice(0,8)}: EV=${EV_enter_now.toFixed(6)} Edge=${EntryEdge.toFixed(6)} Size=${sizing.position_size.toFixed(4)} SOL buyers=${features.breadth_topology.unique_buyers_total}`);

  return {
    shouldEnter: true,
    reason: `Entry approved: EV=${EV_enter_now.toFixed(6)}, Edge=${EntryEdge.toFixed(6)}, Size=${sizing.position_size.toFixed(4)} SOL`,
    ev,
    sizing,
    hardFilterRejection: null,
  };
}

// ====== HARD ENTRY FILTERS ======

function checkHardFilters(
  packet: CandidatePacket,
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  currentPositionCount: number,
  dailyLossSol: number
): string | null {
  if (packet.regime === Regime.EXCLUDED) return 'excluded_regime';
  if (packet.regime === Regime.POST_MIGRATION) return 'post_migration_excluded';
  if (features.creator_wallet_prior.creator_sell_flag) return 'creator_sold';
  if (features.manipulation_distribution.hard_shock) return 'manipulation_hard_shock';
  if (features.friction_execution.execution_freshness_s > config.friction.stale_threshold_s) return 'stale_friction';
  if (ageS(packet.last_updated) > config.health.market_feed_stale_s) return 'stale_feed';
  if (features.manipulation_distribution.manipulation_penalty > config.manipulation.hard_threshold) return 'manipulation_risk';
  if (features.friction_execution.expected_entry_slippage > config.entry.max_slippage_pct) return 'slippage_high';
  if (currentPositionCount >= config.risk.max_positions) return 'max_positions';
  if (Math.abs(dailyLossSol) >= config.risk.max_daily_loss_sol) return 'daily_loss_limit';
  if (features.breadth_topology.unique_buyers_total < config.entry.min_unique_buyers) return 'insufficient_buyers';

  // Concentration: scale by buyer count (natural high concentration with few buyers)
  const buyers = features.breadth_topology.unique_buyers_total;
  const concThreshold = buyers < 15 ? 1.0 : buyers < 30 ? 0.90 : config.entry.max_concentration_top10;
  if (features.breadth_topology.top_10_concentration > concThreshold) return 'concentration_high';

  return null;
}

// ====== ROUND-TRIP FRICTION ======

function computeRoundTripFriction(
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  regime: Regime
): number {
  const fees = config.fees;
  const regimeOverride = fees.regime_fee_overrides[regime] || {};
  const pumpFee = regimeOverride.pump_fee_pct ?? fees.pump_fee_pct;
  const portalFee = fees.pump_portal_fee_pct;
  const baseFee = fees.solana_base_fee_sol;
  const priorityFee = config.execution.default_priority_fee_sol;

  // Round-trip: entry fees + exit fees + slippage both ways
  const entryPct = pumpFee + portalFee + features.friction_execution.expected_entry_slippage;
  const exitPct = pumpFee + portalFee + features.friction_execution.expected_exit_slippage;
  const roundTripPct = entryPct + exitPct;

  // Fixed costs: 2x base fee + 2x priority fee (entry + exit)
  const fixedCosts = (baseFee + priorityFee) * 2;

  const size = config.risk.quick_spend_sol;
  return size * roundTripPct + fixedCosts;
}

// ====== EV_WAIT ======

function computeEVWait(
  packet: CandidatePacket,
  features: FeatureSnapshot,
  probabilities: ProbabilityOutputs,
  config: PumpQuantConfig
): number {
  // On Pump.fun, alpha decays FAST. Positive velocity means waiting costs money.
  // (Budish/Cramton/Shim 2015: "The High-Frequency Trading Arms Race")
  const velocity = features.flow_momentum.buy_notional_velocity_5s;
  const positionSize = config.risk.quick_spend_sol;

  if (velocity > 0.1) {
    // Active token: waiting means missing the move
    // Opportunity cost scales with velocity
    return -velocity * 0.05 * positionSize; // Negative = waiting costs money
  }

  // Slow/no velocity: small positive value of waiting (info gain)
  const uncertainty = 1 - Math.abs(probabilities.P_continuation_5s - 0.5) * 2;
  return uncertainty * 0.005 * positionSize;
}

// ====== POSITION SIZING ======

export function computePositionSizing(
  config: PumpQuantConfig,
  features: FeatureSnapshot,
  frictionCost: number
): PositionSizing {
  const risk = config.risk;
  const risk_budget = risk.bankroll_sol * risk.risk_per_trade_pct;

  const effective_stop_pct =
    risk.raw_stop_pct +
    config.fees.pump_fee_pct * 2 + // Entry + exit
    features.friction_execution.expected_exit_slippage +
    config.friction.safety_buffer_pct;

  const rawFromRisk = effective_stop_pct > 0 ? risk_budget / effective_stop_pct : 0;

  const caps: { name: string; value: number }[] = [
    { name: 'risk_budget', value: rawFromRisk },
    { name: 'quick_spend', value: risk.quick_spend_sol },
    { name: 'max_alloc', value: risk.bankroll_sol * risk.max_alloc_pct },
    { name: 'liquidity_cap', value: risk.liquidity_cap_sol },
    { name: 'slippage_cap', value: risk.slippage_cap_sol },
  ];

  let minCap = caps[0];
  for (const cap of caps) {
    if (cap.value < minCap.value) minCap = cap;
  }

  return {
    risk_budget,
    effective_stop_pct,
    raw_position_size: rawFromRisk,
    position_size: Math.max(0, minCap.value),
    limiting_factor: minCap.name as PositionSizing['limiting_factor'],
  };
}

function emptyEV(): EntryEVCalculation {
  return { EV_enter_now: 0, EV_wait: 0, EntryEdge: 0, upside_net: 0, downside_net: 0, manipulation_cost: 0, friction_cost_now: 0, route_ev_adjustment: 0 };
}
