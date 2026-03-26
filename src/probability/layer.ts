/**
 * @module probability/layer
 * Regime-specific probabilistic outputs:
 * P_continuation_5s, P_continuation_15s, P_reversal_5s, P_reversal_15s, P_manipulation_event
 *
 * Signal Stack v2 (arXiv:2602.14860):
 * - Primary predictor: bonding_curve_dynamics (capital efficiency) — weight 0.35
 * - Hard veto rules: creator dump, bot wash, efficiency floor
 * - Recalibrated sigmoid baseline (-2.5) to correct 50% → ~8% base probability
 * - 30s flow signal windows replace noisy 5s derivatives
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
 * Signal Stack v2 changes:
 * - BCD (capital efficiency) is the dominant signal at weight 0.35
 * - Hard veto rules applied before weighted computation
 * - Sigmoid bias of -2.5 anchors base rate to ~8% (vs broken 50%)
 * - 30s flow windows for more stable momentum signal
 */
export function computeProbabilities(
  features: FeatureSnapshot,
  regime: Regime,
  config: PumpQuantConfig
): ProbabilityOutputs {
  const weights = config.entry.probability_weights;
  const calibration = config.entry.calibration;

  // ====== Step 0: Hard veto rules (applied before any weighted computation) ======
  // These return a near-zero probability immediately, bypassing the weighted stack.

  const bcd = features.bonding_curve_dynamics;
  const creatorNetSol = features.creator_net_sol_position;
  const totalSwaps = features.total_swap_count;

  // CREATOR_DUMP_VETO: creator has extracted > 0.5 SOL net → systematic dump, block entry
  if (creatorNetSol > 0.5) {
    log.debug(`Creator dump veto: creatorNetSol=${creatorNetSol.toFixed(3)} SOL`);
    return buildVetoOutput(0.05);
  }

  // BOT_WASH_VETO: very low BCD score with high swap count → bot wash pattern
  if (bcd.bcd_score < 0.1 && totalSwaps > 150) {
    log.debug(`Bot wash veto: bcd_score=${bcd.bcd_score.toFixed(3)}, totalSwaps=${totalSwaps}`);
    return buildVetoOutput(0.05);
  }

  // EFFICIENCY_FLOOR_VETO: extremely low capital efficiency with high swap count
  if (bcd.capital_efficiency_raw < 0.0005 && totalSwaps > 200) {
    log.debug(`Efficiency floor veto: CE=${bcd.capital_efficiency_raw.toFixed(5)}, totalSwaps=${totalSwaps}`);
    return buildVetoOutput(0.05);
  }

  // ====== Step 1: Deterministic weighted feature stack ======

  // BCD signal: primary predictor from arXiv:2602.14860
  // bcd_score is [0,1] → normalize to [-1, 1] for weighted composite
  const bcdSignal = bcd.bcd_score * 2 - 1;

  // Flow/momentum signal: 30s window (more stable than 5s)
  const flowSignal = computeFlowSignal(features);

  // Breadth/topology signal: higher breadth + low concentration = continuation
  const breadthSignal = computeBreadthSignal(features);

  // Creator/wallet prior signal: CAPPED, feeds as prior only.
  // NORMALIZATION: composite_prior = 0.0 means UNKNOWN creator (no enrichment data).
  // For unknown creators, exclude the weight from the denominator entirely.
  const rawCreatorPrior = features.creator_wallet_prior.composite_prior;
  const hasCreatorData = rawCreatorPrior !== 0 || features.creator_wallet_prior.creator_history_score > 0;
  const walletPriorSignal = hasCreatorData ? rawCreatorPrior : 0.0;

  // Friction/execution signal: good execution = higher confidence
  const frictionSignal = computeFrictionSignal(features);

  // Manipulation signal: higher penalty = higher reversal/manipulation probability
  const manipulationSignal = features.manipulation_distribution.manipulation_penalty;

  // Multimodal signal: only for refinement, not core gate
  const multimodalSignal = features.multimodal_junk.is_stale
    ? 0
    : (features.multimodal_junk.junk_score - 0.5) * 2; // Normalize to [-1, 1]

  // Weighted composite — normalized denominator.
  // When creator data is absent, exclude that weight so remaining signals carry full weight.
  const creatorWeight = hasCreatorData ? weights.creator_wallet_prior : 0;
  const effectiveWeightSum =
    weights.bonding_curve_dynamics +
    weights.flow_momentum +
    weights.breadth_topology +
    creatorWeight +
    weights.friction_execution +
    weights.manipulation_distribution +
    weights.multimodal_junk;
  const normFactor = effectiveWeightSum > 0 ? (1.0 / effectiveWeightSum) : 1.0;

  const rawContinuationSignal = normFactor * (
    weights.bonding_curve_dynamics * bcdSignal +
    weights.flow_momentum * flowSignal +
    weights.breadth_topology * breadthSignal +
    creatorWeight * walletPriorSignal +
    weights.friction_execution * frictionSignal -
    weights.manipulation_distribution * manipulationSignal +
    weights.multimodal_junk * multimodalSignal
  );

  // ====== Step 2: Calibration layer ======

  // Apply regime-specific adjustments
  const regimeAdjustment = getRegimeAdjustment(regime);

  // Calibrated continuation probability.
  // Bias of -1.2 gives ~15% baseline for a zero-signal token.
  // Task is "token continues for 60s after entry" — NOT graduation (0.63%).
  // Empirical base rate for continuation-after-t60s is ~20-25%.
  // -1.2 allows strong signals (BCD > 0.7) to clear 0.52 min_p_continuation
  // while correctly rejecting moderate (0.4 → p=0.43) and weak (0.1 → p=0.29) signals.
  // -2.5 (graduation base rate) was mathematically correct for the wrong outcome.
  const BASE_RATE_BIAS = -1.2;
  const gainMultiplier = 2.0;
  const calibratedContinuation = sigmoid(
    rawContinuationSignal * gainMultiplier + regimeAdjustment + BASE_RATE_BIAS + calibration.continuation_bias
  );

  // Calibrated reversal probability — symmetric gain
  const calibratedReversal = sigmoid(
    -rawContinuationSignal * gainMultiplier - regimeAdjustment + BASE_RATE_BIAS + calibration.reversal_bias
  );

  // Manipulation event probability
  const earlyCurveOffset = regime === 'EARLY_CURVE' ? -1.0 : 0;
  const rawManipulationProb = sigmoid(
    manipulationSignal * 1.5 - 1.5 + earlyCurveOffset +
    (features.manipulation_distribution.hard_shock ? 2.5 : 0) +
    calibration.manipulation_bias
  );

  // ====== Step 3: Horizon-specific probabilities ======

  const P_continuation_5s = Math.min(0.95, Math.max(0.05,
    calibratedContinuation * (1 + 0.1 * flowSignal)
  ));

  const P_continuation_15s = Math.min(0.95, Math.max(0.05,
    calibratedContinuation * (1 + 0.05 * breadthSignal)
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

/** Build a veto output with near-zero continuation probability */
function buildVetoOutput(pContinuation: number): ProbabilityOutputs {
  return {
    P_continuation_5s: pContinuation,
    P_continuation_15s: pContinuation,
    P_reversal_5s: 1 - pContinuation,
    P_reversal_15s: 1 - pContinuation,
    P_manipulation_event: 0.90,
  };
}

/**
 * Compute flow/momentum signal from features → [-1, 1]
 * v2: Use 30s window signals for more stable momentum (less noisy than 5s).
 */
function computeFlowSignal(features: FeatureSnapshot): number {
  const f = features.flow_momentum;

  // 30s buy pressure ratio as primary (0.15 SOL/s = full score)
  const buyPressureNorm = Math.min(1, f.buy_notional_velocity_30s / 0.15);

  // 30s imbalance signal
  const imbalanceSignal = f.buy_sell_imbalance_30s * 0.3;

  // Average trade size (15s window — balanced between noise and recency)
  const sizeSignal = Math.min(1, f.avg_trade_size_15s / 0.05) * 0.2;

  return Math.max(-1, Math.min(1, buyPressureNorm * 0.5 + imbalanceSignal + sizeSignal));
}

/** Compute breadth/topology signal → [-1, 1] */
function computeBreadthSignal(features: FeatureSnapshot): number {
  const b = features.breadth_topology;

  const buyerContribution = Math.min(0.3, b.unique_buyers_total / 100);

  return Math.max(-1, Math.min(1,
    b.breadth_score * 0.35 +
    b.non_dev_participation * 0.2 +
    b.first_100_persistence * 0.1 +
    buyerContribution +
    (1 - b.top_10_concentration) * 0.05,
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
    f.route_ev_adjustment,
  ));
}

/** Regime-specific probability adjustment */
function getRegimeAdjustment(regime: Regime): number {
  switch (regime) {
    case Regime.EARLY_CURVE:
      return 0.1;
    case Regime.MID_CURVE:
      return 0;
    case Regime.LATE_CURVE:
      return -0.05;
    case Regime.GRADUATION_BOUNDARY:
      return -0.15;
    case Regime.POST_MIGRATION:
      return -0.3;
    case Regime.EXCLUDED:
      return -1;
    default:
      return 0;
  }
}

/** Sigmoid function for probability calibration */
function sigmoid(x: number): number {
  return 1 / (1 + Math.exp(-x));
}
