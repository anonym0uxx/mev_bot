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

  // ====== 10.2b RAW STOP LOSS — hard floor at -40% ======
  if (checkRawStopLoss(position, config)) {
    return {
      shouldExit: true,
      shouldReduce: false,
      exitPct: 100,
      reason: ExitReason.STOP_LOSS,
      ev: computeExitEV(packet, position, probabilities, features, config),
    };
  }

  // ====== TRAILING STOP with TIERED PROFIT TAKING ======
  // Activate trailing stop after +2.5% gain. Trail at 1.2% distance.
  // Tiered: at +4% gain, trigger a 50% partial reduce.
  const gainPct = position.entry_sol > 0
    ? (position.current_value_sol - position.entry_sol) / position.entry_sol
    : 0;
  const TRAILING_ACTIVATION = config.exit.trailing_stop_activation_pct ?? 0.025; // activate at +2.5%
  const TRAILING_DISTANCE = config.exit.trailing_stop_distance_pct ?? 0.012;     // trail 1.2% behind peak gain
  const TIER1_TRIGGER = config.exit.tier1_profit_pct ?? 0.04;                    // 50% reduce at +4%
  const TIER1_PCT = config.exit.tier1_reduce_pct ?? 50;

  // Track peak gain in mfe_sol (already tracked as max favorable excursion)
  if (gainPct > 0 && position.mfe_sol < position.current_value_sol) {
    position.mfe_sol = position.current_value_sol;
  }

  // Hard take-profit
  const takeProfitPct = config.exit.take_profit_pct;
  if (takeProfitPct && takeProfitPct > 0 && gainPct >= takeProfitPct) {
    return {
      shouldExit: true,
      shouldReduce: false,
      exitPct: 100,
      reason: ExitReason.EV_NEGATIVE,
      ev: computeExitEV(packet, position, probabilities, features, config),
    };
  }

  // Tiered exit: sell 50% at +4% if not already reduced
  if (gainPct >= TIER1_TRIGGER && position.exit_orders.length === 0) {
    return {
      shouldExit: false,
      shouldReduce: true,
      exitPct: TIER1_PCT,
      reason: ExitReason.EV_NEGATIVE,
      ev: computeExitEV(packet, position, probabilities, features, config),
    };
  }

  // Trailing stop: if activated and price retraces 1.2% from peak
  if (gainPct >= TRAILING_ACTIVATION && position.mfe_sol > 0) {
    const peakGainPct = (position.mfe_sol - position.entry_sol) / position.entry_sol;
    const retraceFromPeak = peakGainPct - gainPct;
    if (retraceFromPeak >= TRAILING_DISTANCE) {
      return {
        shouldExit: true,
        shouldReduce: false,
        exitPct: 100,
        reason: ExitReason.PEAK_RETRACE,
        ev: computeExitEV(packet, position, probabilities, features, config),
      };
    }
  }

  // ====== 10.2c DOA EXIT — Dead on Arrival ======
  // Token never moved: MFE≈0 after 15s AND unrealized loss > 5%
  // Cuts losers before -40% stop fires. Expected outcome: ~-3% instead of -40%.
  const holdSoFar = ageS(position.entry_timestamp);
  const unrealizedPct = position.current_value_sol > 0
    ? (position.current_value_sol - position.entry_sol) / position.entry_sol
    : 0;
  const noMFE = position.mfe_sol < position.entry_sol * 0.02; // MFE < 2% of entry
  if (holdSoFar > 15 && noMFE && unrealizedPct < -0.05) {
    return {
      shouldExit: true,
      shouldReduce: false,
      exitPct: 100,
      reason: ExitReason.TIME_DECAY, // Closest semantic match
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
    log.info(`Exit: ${position.symbol} HoldEdge=${ev.HoldEdge.toFixed(6)} → EXIT (NetExit=${ev.ExpectedNetExitNow.toFixed(6)} P_cont=${probabilities.P_continuation_15s.toFixed(3)} hold=${holdDuration.toFixed(1)}s)`);
  } else if (holdDuration < 2 || Math.floor(holdDuration) % 5 === 0) {
    log.info(`Hold: ${position.symbol} HoldEdge=+${ev.HoldEdge.toFixed(6)} NetExit=${ev.ExpectedNetExitNow.toFixed(6)} P_cont=${probabilities.P_continuation_15s.toFixed(3)} hold=${holdDuration.toFixed(1)}s`);
  }
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
 * Check raw stop loss: exit if unrealized loss exceeds raw_stop_pct.
 * This is a hard floor — EV-based exits should trigger first, but
 * if not, this guarantees we never ride a position to zero.
 */
function checkRawStopLoss(position: Position, config: PumpQuantConfig): boolean {
  if (position.entry_sol <= 0) return false;
  const unrealizedPct = (position.current_value_sol - position.entry_sol) / position.entry_sol;
  return unrealizedPct <= -(config.risk.raw_stop_pct);
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

  // Upside if hold: Pump.fun empirical distribution is heavily right-skewed.
  // Tokens with positive momentum routinely do 30-100%+ in the hold horizon.
  // Downside is bounded by our exit mechanics (forced exits, retrace protection).
  // Shock cost is real but P_manipulation already discounts it.
  const upsideIfHold = quotedSellValue * 0.30; // 30% potential upside (conservative for Pump.fun)
  const downsideIfHold = quotedSellValue * 0.10; // 10% potential downside (bounded by exits)
  const shockCost = quotedSellValue * 0.25; // 25% shock (reduced: P_manipulation already penalizes)

  // Extra friction if we hold longer (slippage might worsen marginally)
  const extraFrictionIfHold = quotedSellValue * 0.005;

  // EV_hold_h = expected INCREMENTAL gain from holding h seconds longer
  // Positive means holding is worth more than exiting now
  const EV_hold_h =
    P_continuation * upsideIfHold
    - P_reversal * downsideIfHold
    - P_manipulation * shockCost
    - extraFrictionIfHold;

  // FIX #3: Compare holding to exiting NOW (liquidation value)
  // HoldEdge = expected gain from holding MINUS what we'd get by exiting now
  // If HoldEdge < 0, exiting is better (stops the bleeding)
  const HoldEdge = EV_hold_h - (ExpectedNetExitNow * 0.001); // Tiny discount for exit friction already included

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
