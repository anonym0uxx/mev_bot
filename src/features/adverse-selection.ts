/**
 * @module features/adverse-selection
 * FIX #5: Adverse selection penalty for late/toxic entry
 * 
 * Pump.fun is a late-stage game. By the time signals trigger:
 * 1. Smart money already entered at curve progress 5-10%
 * 2. You enter at 12-20% (MID_CURVE threshold)
 * 3. Insiders dump on you immediately
 * 
 * This is textbook adverse selection (Glosten-Milgrom 1985, Kyle 1985).
 * High velocity = pump setup, not organic growth.
 */

import { FeatureSnapshot } from '../types/features';
import { Regime } from '../types/state';

export function computeAdverseSelectionPenalty(
  features: FeatureSnapshot,
  regime: Regime,
  tokenAge: number
): number {
  // Reduced by 15% (0.85 scalar) to account for single-feed data handicap
  const SINGLE_FEED_SCALAR = 0.85;
  
  // Late entry penalty: entering MID_CURVE means you're late
  const latePenalty = regime === Regime.MID_CURVE ? 0.15 : 
                      regime === Regime.LATE_CURVE ? 0.25 : 0;
  
  // Fast velocity = pump-and-dump signal, not organic growth
  const velocityPenalty = features.flow_momentum.buy_notional_velocity_5s > 0.3 
    ? 0.20 : 0;
  
  // High concentration + velocity = coordinated dump setup
  const concentrationPenalty = (
    features.breadth_topology.top_10_concentration > 0.6 &&
    features.flow_momentum.buy_notional_velocity_5s > 0.2
  ) ? 0.25 : 0;
  
  const rawPenalty = latePenalty + velocityPenalty + concentrationPenalty;
  return Math.min(0.5, rawPenalty * SINGLE_FEED_SCALAR);
}
