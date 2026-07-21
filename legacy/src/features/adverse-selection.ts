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
  // Adverse selection penalty expressed as a FRACTION of position size.
  // Must be calibrated so that total penalty does not exceed ~15% of position
  // (i.e., return value <= 0.15, so adverseSelectionCost = penalty * positionSize <= 0.0015 SOL on 0.01).
  // Previous miscalibration had penalties up to 0.50 (50% of position), swamping EV.
  
  // Late entry penalty: MID_CURVE = you're behind smart money
  const latePenalty = regime === Regime.MID_CURVE ? 0.04 :
                      regime === Regime.LATE_CURVE ? 0.08 : 0.01;
  
  // Fast velocity = potential pump-and-dump (soft signal, not killer)
  const velocityPenalty = features.flow_momentum.buy_notional_velocity_5s > 0.3
    ? 0.03 : 0;
  
  // High concentration + velocity = coordinated dump setup (harder signal)
  const concentrationPenalty = (
    features.breadth_topology.top_10_concentration > 0.6 &&
    features.flow_momentum.buy_notional_velocity_5s > 0.2
  ) ? 0.04 : 0;

  const rawPenalty = latePenalty + velocityPenalty + concentrationPenalty;
  // Hard cap at 15% of position — keeps adverse selection as a signal modifier, not an EV killer
  return Math.min(0.15, rawPenalty);
}
