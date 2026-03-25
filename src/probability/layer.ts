/**
 * @module probability/layer
 * Regime-specific probabilistic outputs:
 * P_continuation_5s, P_continuation_15s, P_reversal_5s, P_reversal_15s, P_manipulation_event
 *
 * Implementation order:
 * 1. Deterministic weighted feature stack
 * 2. Calibration layer
 * 3. EV decision layer (used by entry/exit engines)
 */

import { FeatureSnapshot } from '../types/features';
import { ProbabilityOutputs, Regime } from '../types/state';
import { PumpQuantConfig } from '../types/config';
import { createLogger } from '../utils/logger';

const log = createLogger('probability');

/**
 * Compute probability outputs from feature snapshot.
 * Uses deterministic weighted feature stack with calibration.
 *
 * Rules:
 * - qualified-wallet prior: capped positive / stronger negative prior
 * - multimodal junk filter: exclusion/tie-break/ranking refinement only
 * - route_ev_adjustment feeds EV comparisons
 * - no single enhancement module overrides core gates
 */
export function computeProbabilities(
  features: FeatureSnapshot,
  regime: Regime,
  config: PumpQuantConfig
): ProbabilityOutputs {
  const weights = config.entry.probability_weights;
  const calibration = config.entry.calibration;

  // ====== Step 1: Deterministic weighted feature stack ======

  // Flow/momentum signal: higher velocity + positive imbalance + acceleration = continuation
  const flowSignal = computeFlowSignal(features);

  // Breadth/topology signal: higher breadth + low concentration = continuation
  const breadthSignal = computeBreadthSignal(features);

  // Creator/wallet prior signal: CAPPED, feeds as prior only
  const walletPriorSignal = features.creator_wallet_prior.composite_prior;

  // Friction/execution signal: good execution = higher confidence
  const frictionSignal = computeFrictionSignal(features);

  // Manipulation signal: higher penalty = higher reversal/manipulation probability
  const manipulationSignal = features.manipulation_distribution.manipulation_penalty;

  // Multimodal signal: only for refinement, not core gate
  const multimodalSignal = features.multimodal_junk.is_stale
    ? 0 // No contribution when stale
    : (features.multimodal_junk.junk_score - 0.5) * 2; // Normalize to [-1, 1]

  // Weighted composite
  const rawContinuationSignal =
    weights.flow_momentum * flowSignal +
    weights.breadth_topology * breadthSignal +
    weights.creator_wallet_prior * walletPriorSignal +
    weights.friction_execution * frictionSignal -
    weights.manipulation_distribution * manipulationSignal +
    weights.multimodal_junk * multimodalSignal;

  // ====== Step 2: Calibration layer ======

  // Apply regime-specific adjustments
  const regimeAdjustment = getRegimeAdjustment(regime);

  // Calibrated continuation probability
  // Apply 2x gain to raw signal for more dynamic range
  // rawSignal of 0.3 → sigmoid(0.6+adj) ≈ 0.65 instead of 0.57
  const calibratedContinuation = sigmoid(
    rawContinuationSignal * 2 + regimeAdjustment + calibration.continuation_bias
  );

  // Calibrated reversal probability
  const calibratedReversal = sigmoid(
    -rawContinuationSignal * 2 - regimeAdjustment + calibration.reversal_bias
  );

  // Manipulation event probability
  // Use moderate amplification — manipulation_penalty [0,1] maps through sigmoid
  // penalty 0.3 → P≈0.55, penalty 0.5 → P≈0.62, penalty 0.8 → P≈0.69
  const rawManipulationProb = sigmoid(
    manipulationSignal * 1.5 - 0.5 + // Center sigmoid so low penalty → low probability
    (features.manipulation_distribution.hard_shock ? 2 : 0) +
    calibration.manipulation_bias
  );

  // ====== Step 3: Horizon-specific probabilities ======

  // 5s horizon: more responsive to fast-lane signals
  const P_continuation_5s = Math.min(0.95, Math.max(0.05,
    calibratedContinuation * (1 + 0.1 * flowSignal) // Boost by fast flow signal
  ));

  // 15s horizon: more influenced by breadth and structure
  const P_continuation_15s = Math.min(0.95, Math.max(0.05,
    calibratedContinuation * (1 + 0.05 * breadthSignal) // Modest breadth boost
  ));

  const P_reversal_5s = Math.min(0.95, Math.max(0.05,
    calibratedReversal * (1 + 0.1 * manipulationSignal)
  ));

  const P_reversal_15s = Math.min(0.95, Math.max(0.05,
    calibratedReversal * (1 + 0.05 * manipulationSignal)
  ));

  const P_manipulation_event = Math.min(0.95, Math.max(0.01,
    rawManipulationProb
  ));

  // Ensure probabilities are properly bounded (continuation + reversal ≤ 1)
  const totalProb5s = P_continuation_5s + P_reversal_5s;
  const totalProb15s = P_continuation_15s + P_reversal_15s;

  return {
    P_continuation_5s: totalProb5s > 1 ? P_continuation_5s / totalProb5s : P_continuation_5s,
    P_continuation_15s: totalProb15s > 1 ? P_continuation_15s / totalProb15s : P_continuation_15s,
    P_reversal_5s: totalProb5s > 1 ? P_reversal_5s / totalProb5s : P_reversal_5s,
    P_reversal_15s: totalProb15s > 1 ? P_reversal_15s / totalProb15s : P_reversal_15s,
    P_manipulation_event,
  };
}

/** Compute flow/momentum signal from features → [-1, 1]
 * Increased gain: positive velocity should produce strong signals.
 * A token with active buying should produce flow signal > 0.5 */
function computeFlowSignal(features: FeatureSnapshot): number {
  const f = features.flow_momentum;

  // Velocity signals — lower normalization thresholds for more sensitivity
  const velocitySignal =
    Math.min(1, f.buy_notional_velocity_5s / 0.15) * 0.35 +
    Math.min(1, f.buy_notional_velocity_15s / 0.1) * 0.2;

  // Acceleration signal
  const accelSignal = Math.tanh(f.buy_velocity_acceleration_5s * 15) * 0.15;

  // Imbalance signal — strong positive imbalance is very bullish
  const imbalanceSignal = f.buy_sell_imbalance_5s * 0.2;

  // Trade count — even 1 trade/s is significant for new tokens
  const tradeCountSignal = Math.min(1, f.trade_count_velocity_5s / 1) * 0.1;

  return Math.max(-1, Math.min(1, velocitySignal + accelSignal + imbalanceSignal + tradeCountSignal));
}

/** Compute breadth/topology signal → [-1, 1]
 * For new tokens, even 3-5 unique buyers is meaningful breadth.
 * Scale buyer contribution more aggressively. */
function computeBreadthSignal(features: FeatureSnapshot): number {
  const b = features.breadth_topology;

  // Buyer count contribution: 5 buyers → 0.15, 10 → 0.25, 20+ → 0.3
  const buyerContribution = Math.min(0.3, b.unique_buyers_total / 60);

  return Math.max(-1, Math.min(1,
    b.breadth_score * 0.35 +
    b.non_dev_participation * 0.2 +
    b.first_100_persistence * 0.1 +
    buyerContribution +
    (1 - b.top_10_concentration) * 0.05
  ));
}

/** Compute friction signal → [-1, 1] where positive = good execution */
function computeFrictionSignal(features: FeatureSnapshot): number {
  const f = features.friction_execution;

  return Math.max(-1, Math.min(1,
    f.route_score * 0.4 -
    f.expected_entry_slippage * 2 -
    f.expected_exit_slippage * 2 -
    f.landing_risk_estimate * 0.3 +
    f.route_ev_adjustment
  ));
}

/** Regime-specific probability adjustment */
function getRegimeAdjustment(regime: Regime): number {
  switch (regime) {
    case Regime.EARLY_CURVE:
      return 0.1;  // Slight positive bias — early stage has more upside potential
    case Regime.MID_CURVE:
      return 0;    // Neutral
    case Regime.LATE_CURVE:
      return -0.05; // Slight caution — more risk of reversal
    case Regime.GRADUATION_BOUNDARY:
      return -0.15; // Higher caution — boundary is risky
    case Regime.POST_MIGRATION:
      return -0.3;  // Strong caution — excluded in initial build
    case Regime.EXCLUDED:
      return -1;    // Should never reach here, but fail safe
    default:
      return 0;
  }
}

/** Sigmoid function for probability calibration */
function sigmoid(x: number): number {
  return 1 / (1 + Math.exp(-x));
}
