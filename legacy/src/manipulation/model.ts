/**
 * @module manipulation/model
 * Manipulation model per spec section 11.
 *
 * A. Hard shock detector: 6 conditions → immediate exit
 * B. Continuous penalty [0,1] → feeds entry and exit engines
 *
 * The feature computation is in features/manipulation-distribution.ts.
 * This module provides higher-level query functions.
 */

import { createLogger } from '../utils/logger';
import { ManipulationDistributionFeatures } from '../types/features';
import { ManipulationConfig } from '../types/config';

const log = createLogger('manipulation');

export interface ManipulationAssessment {
  /** Hard shock triggered — requires immediate action */
  hardShock: boolean;
  /** Which hard shock condition triggered (null if none) */
  hardShockReason: string | null;
  /** Continuous manipulation penalty [0,1] */
  penalty: number;
  /** Whether manipulation is above the hard rejection threshold */
  aboveHardThreshold: boolean;
  /** Risk level description */
  riskLevel: 'none' | 'low' | 'medium' | 'high' | 'critical';
}

/**
 * Assess manipulation risk from feature outputs and config thresholds.
 */
export function assessManipulationRisk(
  features: ManipulationDistributionFeatures,
  config: ManipulationConfig
): ManipulationAssessment {
  // A. Hard shock detector
  let hardShock = false;
  let hardShockReason: string | null = null;

  // 1. Creator sell
  if (features.creator_sell && config.creator_sell_instant_exit) {
    hardShock = true;
    hardShockReason = 'creator_sell';
  }

  // 2. Repeated same-size prints
  if (!hardShock && features.same_size_print_count >= config.same_size_print_min_count) {
    hardShock = true;
    hardShockReason = 'same_size_prints';
  }

  // 3. Price up + breadth flat
  if (!hardShock && features.price_breadth_divergence >= config.price_breadth_divergence_threshold) {
    hardShock = true;
    hardShockReason = 'price_breadth_divergence';
  }

  // 4. Sudden concentration worsening
  if (!hardShock && features.concentration_worsening >= config.concentration_worsening_threshold) {
    hardShock = true;
    hardShockReason = 'concentration_worsening';
  }

  // 5. Cluster exit signature
  if (!hardShock && features.cluster_correlation >= config.cluster_correlation_threshold) {
    hardShock = true;
    hardShockReason = 'cluster_correlation';
  }

  // 6. Slippage blowout without healthy breadth
  if (!hardShock && features.slippage_shock >= config.slippage_shock_threshold) {
    hardShock = true;
    hardShockReason = 'slippage_shock';
  }

  // B. Continuous penalty
  const penalty = features.manipulation_penalty;
  const aboveHardThreshold = penalty > config.hard_threshold;

  // Risk level classification
  let riskLevel: ManipulationAssessment['riskLevel'];
  if (hardShock || penalty > 0.8) {
    riskLevel = 'critical';
  } else if (aboveHardThreshold || penalty > 0.5) {
    riskLevel = 'high';
  } else if (penalty > 0.3) {
    riskLevel = 'medium';
  } else if (penalty > 0.1) {
    riskLevel = 'low';
  } else {
    riskLevel = 'none';
  }

  return {
    hardShock,
    hardShockReason,
    penalty,
    aboveHardThreshold,
    riskLevel,
  };
}

/**
 * Check if manipulation assessment should trigger entry rejection.
 */
export function shouldRejectEntry(assessment: ManipulationAssessment): boolean {
  return assessment.hardShock || assessment.aboveHardThreshold;
}

/**
 * Check if manipulation assessment should trigger immediate exit.
 */
export function shouldForceExit(assessment: ManipulationAssessment): boolean {
  return assessment.hardShock;
}
