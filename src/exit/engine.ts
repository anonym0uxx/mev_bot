/**
 * @module exit/engine
 * Final Exit Engine per spec section 10.
 *
 * Exit doctrine: hold ONLY while EV_hold_h > EV_exit_now.
 * Catastrophic overrides trigger immediate full exit.
 * Uses net liquidation value everywhere.
 */

import { createLogger } from '../utils/logger';
import { nowMs, ageS } from '../utils/time';
import {
  CandidatePacket, ExitEVCalculation, ProbabilityOutputs, Regime, ExitReason,
} from '../types/state';
import { FeatureSnapshot } from '../types/features';
import { PumpQuantConfig } from '../types/config';
import { Position } from '../types/trade';

const log = createLogger('exit-engine');

export interface ExitDecision {
  shouldExit: boolean;
  shouldReduce: boolean;
  exitPct: number; // 0-100, how much to exit
  reason: ExitReason;
  ev: ExitEVCalculation;
}

/**
 * Evaluate whether to exit/reduce a position.
 * Non-negotiable: hold only while EV_hold > EV_exit_now.
 */
export function evaluateExit(
  packet: CandidatePacket,
  position: Position,
  probabilities: ProbabilityOutputs,
  features: FeatureSnapshot,
  config: PumpQuantConfig
): ExitDecision {
  // ====== 10.2 CATASTROPHIC OVERRIDES — immediate full exit ======
  const catastrophicOverride = checkCatastrophicOverrides(features, config);
  if (catastrophicOverride) {
    return {
      shouldExit: true,
      shouldReduce: false,
      exitPct: 100,
      reason: catastrophicOverride,
      ev: computeExitEV(packet, position, probabilities, features, config),
    };
  }

  // ====== 10.3 NET MARKING ======
  const ev = computeExitEV(packet, position, probabilities, features, config);

  // ====== 10.5 PEAK NET PROTECTION ======
  // Update peak
  if (ev.ExpectedNetExitNow > position.peak_net_exit_value) {
    position.peak_net_exit_value = ev.ExpectedNetExitNow;
  }
  ev.PeakNetExitValue = position.peak_net_exit_value;
  ev.NetRetrace = position.peak_net_exit_value > 0
    ? 1 - (ev.ExpectedNetExitNow / position.peak_net_exit_value)
    : 0;

  // Dynamic retrace threshold (tightens under conditions from spec 10.5)
  ev.dynamic_retrace_threshold = computeDynamicRetraceThreshold(
    packet, position, features, config, ev
  );

  // Check retrace threshold
  if (ev.NetRetrace > ev.dynamic_retrace_threshold && ev.PeakNetExitValue > position.entry_sol) {
    return {
      shouldExit: true,
      shouldReduce: false,
      exitPct: 100,
      reason: ExitReason.PEAK_RETRACE,
      ev,
    };
  }

  // ====== 10.6 TIME DECAY ======
  const holdDuration = ageS(position.entry_timestamp);
  ev.time_decay_pressure = computeTimeDecayPressure(holdDuration, config);

  // Max hold time override
  if (holdDuration > config.exit.max_hold_time_s) {
    return {
      shouldExit: true,
      shouldReduce: false,
      exitPct: 100,
      reason: ExitReason.TIME_DECAY,
      ev,
    };
  }

  // ====== 10.4 HOLD FORMULA ======
  // Hold if HoldEdge > 0 and no override
  if (ev.HoldEdge <= 0) {
    return {
      shouldExit: true,
      shouldReduce: false,
      exitPct: 100,
      reason: ExitReason.EV_NEGATIVE,
      ev,
    };
  }

  // Check for REDUCE scenario: HoldEdge positive but weakening
  const holdEdgeWeak = ev.HoldEdge < 0.005; // Very thin edge
  const boundaryRisk = packet.regime === Regime.GRADUATION_BOUNDARY;
  const slippageWorsening = features.friction_execution.expected_exit_slippage > config.friction.default_exit_slippage_pct * 1.5;
  const manipulationRising = features.manipulation_distribution.manipulation_penalty > config.manipulation.hard_threshold * 0.7;

  if (holdEdgeWeak || boundaryRisk || slippageWorsening || manipulationRising) {
    // Partial reduce instead of full exit
    const reducePct = computeReducePct(ev, features, config, packet.regime);
    if (reducePct > 0) {
      return {
        shouldExit: false,
        shouldReduce: true,
        exitPct: reducePct,
        reason: ExitReason.EV_NEGATIVE,
        ev,
      };
    }
  }

  // Hold: everything looks fine
  return {
    shouldExit: false,
    shouldReduce: false,
    exitPct: 0,
    reason: ExitReason.EV_NEGATIVE, // Placeholder, not used since not exiting
    ev,
  };
}

/**
 * 10.2 Catastrophic overrides: immediate full exit if ANY true.
 */
function checkCatastrophicOverrides(
  features: FeatureSnapshot,
  config: PumpQuantConfig
): ExitReason | null {
  // 1. Creator sold
  if (features.manipulation_distribution.creator_sell && config.manipulation.creator_sell_instant_exit) {
    return ExitReason.CREATOR_SELL;
  }

  // 2. Slippage shock
  if (features.manipulation_distribution.slippage_shock >= config.manipulation.slippage_shock_threshold) {
    return ExitReason.SLIPPAGE_SHOCK;
  }

  // 3. Execution path failure
  if (features.friction_execution.retry_failure_rate >= 0.8) {
    return ExitReason.EXECUTION_FAILURE;
  }

  // 4. Manipulation shock (hard shock detected)
  if (features.manipulation_distribution.hard_shock) {
    return ExitReason.MANIPULATION_SHOCK;
  }

  // 5. Concentration shock
  if (features.manipulation_distribution.concentration_worsening >= config.manipulation.concentration_worsening_threshold) {
    return ExitReason.CONCENTRATION_SHOCK;
  }

  return null;
}

/**
 * Compute exit EV calculations (spec sections 10.3, 10.4).
 * Uses net liquidation value everywhere.
 */
function computeExitEV(
  packet: CandidatePacket,
  position: Position,
  probabilities: ProbabilityOutputs,
  features: FeatureSnapshot,
  config: PumpQuantConfig
): ExitEVCalculation {
  const fees = config.fees;
  const regimeOverride = fees.regime_fee_overrides[packet.regime] || {};

  // 10.3 NET MARKING: ExpectedNetExitNow
  const quotedSellValue = position.current_tokens > 0
    ? estimateSellValue(position.current_tokens, packet.v_tokens_in_curve, packet.v_sol_in_curve)
    : 0;

  const exitFee = regimeOverride.pump_fee_pct ?? fees.pump_fee_pct;
  const portalFee = fees.pump_portal_fee_pct;
  const exitSlippage = features.friction_execution.expected_exit_slippage;
  const networkFee = fees.solana_base_fee_sol;
  const priorityFee = config.execution.default_priority_fee_sol;

  const ExpectedNetExitNow = quotedSellValue
    - quotedSellValue * (exitFee + portalFee + exitSlippage)
    - networkFee
    - priorityFee;

  // 10.4 HOLD FORMULA
  const horizon = config.exit.hold_horizon_s;
  const P_continuation = horizon <= 5
    ? probabilities.P_continuation_5s
    : probabilities.P_continuation_15s;
  const P_reversal = horizon <= 5
    ? probabilities.P_reversal_5s
    : probabilities.P_reversal_15s;
  const P_manipulation = probabilities.P_manipulation_event;

  // Upside if hold: expected value increase over hold horizon
  const upsideIfHold = quotedSellValue * 0.1; // ~10% potential upside
  const downsideIfHold = quotedSellValue * 0.15; // ~15% potential downside
  const shockCost = quotedSellValue * 0.5; // 50% loss in shock

  // Extra friction if we hold longer (fees don't change, but slippage might worsen)
  const extraFrictionIfHold = quotedSellValue * 0.01; // Modest additional slippage risk

  const EV_hold_h =
    P_continuation * upsideIfHold
    - P_reversal * downsideIfHold
    - P_manipulation * shockCost
    - extraFrictionIfHold;

  const HoldEdge = EV_hold_h - ExpectedNetExitNow;

  return {
    ExpectedNetExitNow,
    EV_hold_h,
    HoldEdge,
    PeakNetExitValue: position.peak_net_exit_value,
    NetRetrace: 0, // Computed after this return
    dynamic_retrace_threshold: config.exit.retrace_threshold_base,
    time_decay_pressure: 0, // Computed separately
    upside_if_hold: upsideIfHold,
    downside_if_hold: downsideIfHold,
    shock_cost: shockCost,
    extra_friction_if_hold: extraFrictionIfHold,
  };
}

/**
 * 10.5 Dynamic retrace threshold: tightens under certain conditions.
 */
function computeDynamicRetraceThreshold(
  packet: CandidatePacket,
  position: Position,
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  ev: ExitEVCalculation
): number {
  let threshold = config.exit.retrace_threshold_base;

  // Tighten when curve enters boundary zone
  if (packet.regime === Regime.GRADUATION_BOUNDARY) {
    threshold -= config.exit.retrace_tightening_boundary;
  }

  // Tighten when slippage worsens
  const slippageRatio = features.friction_execution.expected_exit_slippage /
    config.friction.default_exit_slippage_pct;
  if (slippageRatio > 1.5) {
    threshold -= config.exit.retrace_tightening_slippage * Math.min(1, (slippageRatio - 1));
  }

  // Tighten when hold-edge weakens
  if (ev.HoldEdge < 0.01) {
    threshold -= config.exit.retrace_tightening_hold_edge;
  }

  // Tighten with time in trade
  const holdDuration = ageS(position.entry_timestamp);
  if (holdDuration > config.exit.time_decay_start_s) {
    const timeFactor = (holdDuration - config.exit.time_decay_start_s) / config.exit.max_hold_time_s;
    threshold -= config.exit.retrace_tightening_time * Math.min(1, timeFactor);
  }

  // Minimum threshold floor
  return Math.max(0.05, threshold);
}

/**
 * 10.6 Time decay pressure.
 */
function computeTimeDecayPressure(holdDurationS: number, config: PumpQuantConfig): number {
  if (holdDurationS <= config.exit.time_decay_start_s) return 0;
  const excess = holdDurationS - config.exit.time_decay_start_s;
  return excess * config.exit.time_decay_pressure_per_s;
}

/**
 * Compute reduce percentage based on edge weakness and risk factors.
 */
function computeReducePct(
  ev: ExitEVCalculation,
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  regime: Regime
): number {
  let reducePct = 0;

  // Weak hold edge → reduce 25%
  if (ev.HoldEdge < 0.005 && ev.HoldEdge > 0) {
    reducePct = 25;
  }

  // Boundary zone → reduce 50%
  if (regime === Regime.GRADUATION_BOUNDARY) {
    reducePct = Math.max(reducePct, 50);
  }

  // Rising manipulation → reduce 50%
  if (features.manipulation_distribution.manipulation_penalty > config.manipulation.hard_threshold * 0.7) {
    reducePct = Math.max(reducePct, 50);
  }

  // Worsening slippage → reduce 25%
  if (features.friction_execution.expected_exit_slippage > config.friction.default_exit_slippage_pct * 2) {
    reducePct = Math.max(reducePct, 25);
  }

  return reducePct;
}

/**
 * Estimate sell value from bonding curve state.
 * Uses the Pump.fun constant product formula: xy = k
 * Selling tokens: receive SOL from the curve.
 */
function estimateSellValue(
  tokenAmount: number,
  vTokensInCurve: number,
  vSolInCurve: number
): number {
  if (vTokensInCurve <= 0 || vSolInCurve <= 0 || tokenAmount <= 0) return 0;

  // Constant product: k = vTokens * vSol
  const k = vTokensInCurve * vSolInCurve;
  const newVTokens = vTokensInCurve + tokenAmount;
  const newVSol = k / newVTokens;
  const solReceived = vSolInCurve - newVSol;

  return Math.max(0, solReceived);
}
