/**
 * @module features/dynamic-slippage
 * FIX #4: Dynamic slippage estimation based on market microstructure
 * 
 * Static 5% exit slippage was underestimating by 5-10x.
 * Pump.fun has adversarial market structure:
 * - Thin bonding curve liquidity
 * - Front-running exit signals
 * - xy=k bonding curve (small sells move price hard)
 */

import { FeatureSnapshot } from '../types/features';
import { Position } from '../types/trade';
import { PumpQuantConfig } from '../types/config';
import { nowMs } from '../utils/time';

export function estimateExitSlippage(
  features: FeatureSnapshot,
  position: Position | null,
  config: PumpQuantConfig
): number {
  const baseSlippage = config.friction.default_exit_slippage_pct;

  // We now have gRPC CoreCast — no single-feed penalty needed
  const SINGLE_FEED_MULTIPLIER = 1.0;
  
  // Penalty for high concentration (top holders can dump on you)
  const concentrationPenalty = features.breadth_topology.top_10_concentration * 0.15;
  
  // Penalty for adverse velocity (fast tokens = more sharks)
  const velocityPenalty = Math.min(0.1, features.flow_momentum.buy_notional_velocity_5s * 0.2);
  
  // Penalty for holding longer (telegraphed exit)
  const holdPenalty = position 
    ? Math.min(0.05, (nowMs() - position.entry_timestamp) / 1000 / 60) // 1% per minute
    : 0;
  
  const rawSlippage = baseSlippage + concentrationPenalty + velocityPenalty + holdPenalty;
  return Math.min(0.30, rawSlippage * SINGLE_FEED_MULTIPLIER);
}
